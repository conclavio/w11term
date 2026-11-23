[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"keyboard_hints":"F8"}}')
$profiles = Join-Path $State 'profiles.json'
$catalog = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
$local = $catalog.profiles.'Palette PowerShell'
$local | Add-Member NoteProperty cwd $State
$local | Add-Member NoteProperty powershell_integration $true
[IO.File]::WriteAllText($profiles, ($catalog | ConvertTo-Json -Depth 12))

# The opt-in shell hook reports this folder after Set-Location. The profile
# starts elsewhere, so using its original cwd cannot reveal the expected file.
$env:W11TERM_HINT_CWD = Join-Path $State ('100% # [pilot] ' + [char]0x96ea)
$leaf = 'w11term-hint-' + [guid]::NewGuid().ToString('N')
$fixtureDir = Join-Path $env:W11TERM_HINT_CWD $leaf
$null = [IO.Directory]::CreateDirectory($fixtureDir)
$proof = '.owner-' + [guid]::NewGuid().ToString('N')
[IO.File]::WriteAllText((Join-Path $fixtureDir $proof), '')
$target = Join-Path $fixtureDir 'trace.rs'
$payload = [guid]::NewGuid().ToString('N')
$env:W11TERM_TEST_EXPLORER_TRACE = Join-Path $State 'explorer-result.json'
$env:W11TERM_HINT_TEXT = $leaf + '/trace.rs'
$explorer = New-Object -ComObject Shell.Application
$originalHandles = @($explorer.Windows() | ForEach-Object { $_.HWND })

# An independent probe runs in the same STA and logon session as the test.
# A Windows service desktop can host WinUI windows without an Explorer shell.
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class FixtureExplorer {
    [DllImport("user32.dll")] public static extern IntPtr GetShellWindow();
    [DllImport("shell32.dll", CharSet=CharSet.Unicode, ExactSpelling=true)]
    static extern int SHParseDisplayName(string name, IntPtr context, out IntPtr pidl, uint flags, IntPtr attributes);
    [DllImport("shell32.dll")] static extern int SHOpenFolderAndSelectItems(IntPtr pidl, uint count, IntPtr children, uint flags);
    public static int SelectItem(string path) {
        IntPtr pidl;
        Marshal.ThrowExceptionForHR(SHParseDisplayName(path, IntPtr.Zero, out pidl, 0, IntPtr.Zero));
        try { return SHOpenFolderAndSelectItems(pidl, 0, IntPtr.Zero, 0); }
        finally { Marshal.FreeCoTaskMem(pidl); }
    }
}
'@

function Get-FixtureViews {
    foreach ($view in $explorer.Windows()) {
        try {
            $folder = $view.Document.Folder.Self
            # The marker identifies the exact directory with either the long
            # or 8.3 spelling of the runner's temp path.
            if ($folder -and $folder.Name -eq $leaf -and
                [IO.File]::Exists([IO.Path]::Combine($folder.Path, $proof))) { $view }
        } catch [Runtime.InteropServices.COMException] {
            # Explorer can finish closing a view during this enumeration.
            $script:lastViewError = $_.Exception.Message
        }
    }
}
function Get-ShellState {
    $session = [Diagnostics.Process]::GetCurrentProcess().SessionId
    [pscustomobject]@{
        Session = $session
        Interactive = [Environment]::UserInteractive
        ShellWindow = [FixtureExplorer]::GetShellWindow().ToInt64()
        ExplorerProcesses = @(Get-Process explorer -ErrorAction SilentlyContinue | Where-Object SessionId -eq $session).Count
        RegisteredViews = $explorer.Windows().Count
        FixtureViews = @(Get-FixtureViews).Count
        LastViewError = $script:lastViewError
    }
}
function Test-SelectedFile([string]$File) {
    foreach ($view in @(Get-FixtureViews)) {
        try {
            $items = $view.Document.SelectedItems()
            if ($items.Count -eq 1 -and [IO.Path]::GetFileName($items.Item(0).Path) -eq $File) { return $true }
        } catch [Runtime.InteropServices.COMException] {
            $script:lastViewError = $_.Exception.Message
        }
    }
    return $false
}
function Wait-ForSelection([string]$File) {
    $deadline = [DateTime]::UtcNow.AddSeconds(12)
    do {
        if (Test-SelectedFile $File) { return $true }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    return $false
}
function Close-FixtureViews {
    foreach ($view in @(Get-FixtureViews)) {
        $handle = $view.HWND
        $root = [System.Windows.Automation.AutomationElement]::FromHandle([IntPtr]$handle)
        $tabs = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty,
                [System.Windows.Automation.ControlType]::TabItem))
        if ($handle -notin $originalHandles -and $tabs.Count -le 1) {
            $view.Quit()
        } else {
            # Windows 11 can open a tab inside an existing Explorer window.
            # Invoke only the close button of the uniquely named fixture tab.
            $tab = $tabs | Where-Object { $_.Current.Name.Contains($leaf) } | Select-Object -First 1
            if (!$tab) { throw 'Could not identify the fixture tab in the shared Explorer window' }
            $buttons = $tab.FindAll([System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty,
                    [System.Windows.Automation.ControlType]::Button))
            $close = if ($buttons.Count -eq 1) { $buttons[0] } else {
                $buttons | Where-Object { $_.Current.AutomationId -like '*Close*' -or $_.Current.Name -like 'Close tab*' } | Select-Object -First 1
            }
            if (!$close) { throw 'Could not identify the fixture tab close button' }
            Invoke-Button $close
        }
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(5)
    while (@(Get-FixtureViews).Count) {
        if ([DateTime]::UtcNow -gt $deadline) { throw 'The fixture Explorer view did not close' }
        Start-Sleep -Milliseconds 80
    }
}
function Send-PowerShellLine([string]$Line) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Line + [char]13))
    $null = Control @('send-text','--pane',"$pane",'--no-paste','--base64',$encoded)
}
function Find-HintText([string]$Pattern) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $texts = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $texts) |
        Where-Object { $_.Current.Name -match $Pattern } | Select-Object -First 1
}

$app = $null
try {
    $before = Get-ShellState
    $openError = $null
    if ($before.ShellWindow -or ($before.Interactive -and $before.Session -ne 0)) {
        # Start with a view of this exact test directory so an inactive desktop
        # shell has a chance to register it. No unrelated window is activated.
        try { $explorer.Explore($fixtureDir) } catch { $openError = $_.Exception.Message }
        $deadline = [DateTime]::UtcNow.AddSeconds(5)
        while (-not @(Get-FixtureViews).Count -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 100 }
    }
    $probeHr = [FixtureExplorer]::SelectItem((Join-Path $fixtureDir $proof))
    $sawProbe = $probeHr -ge 0 -and (Wait-ForSelection $proof)
    $baseline = Get-ShellState
    $hosted = $env:GITHUB_ACTIONS -eq 'true' -and $env:RUNNER_ENVIRONMENT -eq 'github-hosted'
    $desktopless = !$before.ShellWindow -and !$before.RegisteredViews -and !$baseline.ShellWindow -and !$baseline.RegisteredViews
    $allowHandoff = !$sawProbe -and ($hosted -or $desktopless)
    $diagnostics = "initial=$($before | ConvertTo-Json -Compress); baseline=$($baseline | ConvertTo-Json -Compress); Explore error=$openError; independent selection HRESULT=$probeHr; observed=$sawProbe"
    $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','144','--rows','36') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
    $window = Wait-InputWindow
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    $pane = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $pane
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','45','PS ')
    Send-PowerShellLine 'Set-Location -LiteralPath $env:W11TERM_HINT_CWD'
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15',('[pilot] ' + [char]0x96ea + '>'))
    Send-PowerShellLine '[Console]::Write(([string][char]27 + "[2J" + [char]27 + "[H")); Write-Output ($env:W11TERM_HINT_TEXT + ":12:8 hint-" + "ready")'
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15',($env:W11TERM_HINT_TEXT + ':12:8 hint-ready'))
    Press-Key 0x77 # F8 opens hints
    $dot = ' ' + [char]0xb7 + ' '
    $null = Wait-Until 'viewport hints' { Find-HintText '^Type a hint label' }
    Press-Key 0x41 # the diagnostic starts the first visible row
    $legend = 'Enter copy' + $dot + 'R reveal in File Explorer' + $dot + 'Esc cancel   ' + $env:W11TERM_HINT_TEXT
    $null = Wait-Until 'local reveal action for a diagnostic path without its line and column' { Find-Element $legend }
    Press-Key 0x52
    $null = Wait-Until 'missing file error in hint mode' { Find-HintText '^Reveal failed' }
    if ([IO.File]::Exists($env:W11TERM_TEST_EXPLORER_TRACE)) { throw 'A missing file reached the native Shell selector' }
    [IO.File]::WriteAllText($target, $payload)
    Press-Key 0x52 # retry the same selection
    $trace = Wait-Until "the native Shell selection call ($diagnostics)" {
        if ([IO.File]::Exists($env:W11TERM_TEST_EXPLORER_TRACE)) {
            [IO.File]::ReadAllText($env:W11TERM_TEST_EXPLORER_TRACE) | ConvertFrom-Json
        }
    }
    if (!$trace.path -or $null -eq $trace.hresult -or [IO.Path]::GetFileName($trace.path) -ne 'trace.rs' -or
        [IO.File]::ReadAllText($trace.path) -cne $payload) {
        throw "The native Shell call received the wrong item: $($trace | ConvertTo-Json -Compress)"
    }
    $selected = $trace.hresult -ge 0 -and (Wait-ForSelection 'trace.rs')
    $after = Get-ShellState
    $diagnostics += "; after=$($after | ConvertTo-Json -Compress); actual HRESULT=$($trace.hresult)"
    if ($selected) {
        $evidence = 'observed the selected file in Explorer'
    } elseif ($allowHandoff -and $trace.hresult -eq $probeHr) {
        # On hosted machines the desktop shell may run in another session or
        # at a different integrity level. The independent call established
        # whether this process could observe the selection before w11term ran.
        $evidence = 'verified the exact Shell PIDL and HRESULT; the independent Explorer selection was unobservable'
        if ($trace.hresult -lt 0) {
            $null = Wait-Until 'the Shell failure to remain retryable' { Find-HintText '^Reveal failed: Selecting ' }
        }
    } else {
        throw "Explorer did not select the fixture file. $diagnostics"
    }
    if ($trace.hresult -ge 0) {
        $null = Wait-Until 'a successful reveal to close the hint legend' {
            -not (Find-HintText '^(Type a hint label|Enter copy|Reveal failed|Opening File Explorer|No matching hints)')
        }
    }
    if ($env:GITHUB_STEP_SUMMARY) {
        $summary = "Explorer reveal $evidence. Session $($baseline.Session), shell HWND $($baseline.ShellWindow), fixture views $($baseline.FixtureViews), independent HRESULT $probeHr, actual HRESULT $($trace.hresult)."
        [IO.File]::AppendAllText($env:GITHUB_STEP_SUMMARY, "$summary`n")
    }
    Write-Output "Verified local reveal ($evidence). $diagnostics"
    # The new folder has never been opened, so an observable view proves
    # that the folder command opens the reported directory itself.
    if (Find-HintText '^(Type a hint label|Enter copy|Reveal failed)') { Press-Key 27 }
    Close-FixtureViews
    $leaf += '-folder'
    $fixtureDir = Join-Path $env:W11TERM_HINT_CWD $leaf
    $null = [IO.Directory]::CreateDirectory($fixtureDir)
    [IO.File]::WriteAllText((Join-Path $fixtureDir $proof), '')
    Send-PowerShellLine ('Set-Location -LiteralPath (Join-Path $env:W11TERM_HINT_CWD "{0}")' -f $leaf)
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15',"$leaf>")
    $menu = [System.Windows.Automation.ControlType]::MenuItem
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    $panes = Wait-Until 'Panes submenu' { Find-Element 'Panes' $menu }
    $panes.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    Invoke-Button (Wait-Until 'enabled Explorer folder action' {
        $entry = Find-Element 'Open current folder in File Explorer' $menu
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
    $trace = Wait-Until 'Explorer folder handoff for the reported PowerShell directory' {
        $result = [IO.File]::ReadAllText($env:W11TERM_TEST_EXPLORER_TRACE) | ConvertFrom-Json
        if ($result.action -eq 'explore') { $result }
    }
    if (!$trace.path -or ![IO.File]::Exists((Join-Path $trace.path $proof))) {
        throw "Explorer received the wrong folder: $($trace | ConvertTo-Json -Compress)"
    }
    if (!$allowHandoff) {
        if ($trace.hresult -lt 0) { throw "Explorer rejected the folder: $($trace.hresult)" }
        $null = Wait-Until 'Explorer displays the reported folder itself' { @(Get-FixtureViews).Count -gt 0 }
    }
    $null = Control @('send-text','--pane',"$remote",'--no-paste','ssh-neighbor-ok')
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','ssh-neighbor-ok')
} finally {
    try { Close-FixtureViews }
    finally {
        if ($app -and !$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        if ($app) { $app.Dispose() }
    }
}
