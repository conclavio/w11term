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
$env:W11TERM_HINT_TEXT = $leaf + '/trace.rs'
$explorer = New-Object -ComObject Shell.Application
$originalHandles = @($explorer.Windows() | ForEach-Object { $_.HWND })

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
        }
    }
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
function Hint-StartingWith([string]$Prefix) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $texts = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $texts) |
        Where-Object { $_.Current.Name.StartsWith($Prefix) } | Select-Object -First 1
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','144','--rows','36') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
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
    $null = Wait-Until 'viewport hints' { Hint-StartingWith 'Type a hint label' }
    Press-Key 0x41 # the diagnostic starts the first visible row
    $legend = 'Enter copy' + $dot + 'R reveal in File Explorer' + $dot + 'Esc cancel   ' + $env:W11TERM_HINT_TEXT
    $null = Wait-Until 'local reveal action for a diagnostic path without its line and column' { Find-Element $legend }
    Press-Key 0x52
    $null = Wait-Until 'missing file error in hint mode' { Hint-StartingWith 'Reveal failed' }
    if (@(Get-FixtureViews).Count) { throw 'Explorer opened the parent for a missing file' }
    [IO.File]::WriteAllText($target, 'fixture file')
    Press-Key 0x52 # retry the same selection
    $null = Wait-Until 'File Explorer to select the exact fixture file' {
        foreach ($view in @(Get-FixtureViews)) {
            $items = $view.Document.SelectedItems()
            if ($items.Count -eq 1 -and [IO.Path]::GetFileName($items.Item(0).Path) -eq 'trace.rs') { return $true }
        }
    }
    $null = Control @('send-text','--pane',"$remote",'--no-paste','ssh-neighbor-ok')
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','ssh-neighbor-ok')
} finally {
    try { Close-FixtureViews }
    finally {
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
}
