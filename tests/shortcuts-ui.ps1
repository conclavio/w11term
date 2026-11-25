# Run by tests/sftp/native_gui.rs against an isolated two-pane workspace.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State, [Parameter(Mandatory)][string]$Trace)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'

function Set-FocusRight([string]$Before, [string]$After) {
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    Invoke-Button (Wait-Until 'settings menu' { Find-Element 'Settings' ([System.Windows.Automation.ControlType]::MenuItem) })
    Invoke-Button (Wait-Until 'keyboard settings' { Find-Element 'Keyboard' })
    Invoke-Button (Wait-Until 'edit Focus right' { Find-Element 'Edit Focus right' })
    $field = Wait-Until 'shortcut editor' { Find-Element 'Shortcut for Focus right' }
    $value = $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    if ($value.Current.Value -ne $Before) { throw "Expected $Before in Focus right, found $($value.Current.Value)" }
    $value.SetValue($After)
    $save = Wait-Until "editor to recognize $After" {
        $button = Find-Element 'Save shortcut'
        if ($button -and $button.Current.IsEnabled -and $value.Current.Value -eq $After) { return $button }
    }
    Invoke-Button $save
    # The editor returns to the action list after committing the file. Repeated
    # reads during Save can lock the destination against replacement on Windows.
    $null = Wait-Until "saved $After binding" { Find-Element 'Edit Focus right' }
    $saved = (Get-Content -Raw -LiteralPath $env:W11TERM_SETTINGS_PATH | ConvertFrom-Json).shortcuts.focus_right
    if ($saved -ne $After) { throw "Expected saved $After binding, found '$saved'" }
    Invoke-Button (Wait-Until 'back to settings' { Find-Element 'Back to settings' })
    Picker-Query 'fixture-ssh'
    Press-Key 13
    Wait-Focus $remote
}

for ($launch = 1; $launch -le 2; $launch++) {
    $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--workspace', 'shortcuts') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
    try {
        $window = Wait-Until 'window handle' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
        $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
        $panes = (Control @('list')).sessions
        $remote = ($panes | Where-Object label -eq 'fixture-ssh').id
        $local = ($panes | Where-Object label -eq 'Windows PowerShell').id
        if ($panes.Count -ne 2 -or !$remote -or !$local) { throw "Missing SSH/PowerShell split" }
        $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
        Wait-Focus $remote
        if ($launch -eq 1) {
            # Unmodified F-keys can be addressed to the fixture HWND without
            # manipulating the Windows user's real Ctrl/Alt keyboard state.
            Set-FocusRight 'Ctrl+Alt+Right' 'F7'
            Press-Key 0x76
            Wait-Focus $local
            Set-FocusRight 'F7' 'F8'
        }
        Press-Key 0x76
        $f7Length = ([string][char]27 + '[18~').Length
        $null = Wait-Until 'released F7 on the SSH wire' {
            (Get-Item -LiteralPath $Trace).Length -ge ($f7Length * $launch)
        }
        Wait-Focus $remote
        $before = (Control @('list')).sessions
        $remoteWidth = ($before | Where-Object id -eq $remote).cols
        $localWidth = ($before | Where-Object id -eq $local).cols
        Post-Key 0x100 0x78 # F9, resize right
        $wider = Wait-Until 'SSH to grow and PowerShell to shrink' {
            $now = (Control @('list')).sessions
            if (($now | Where-Object id -eq $remote).cols -gt $remoteWidth -and
                ($now | Where-Object id -eq $local).cols -lt $localWidth) { $now }
        }
        $ssh = $wider | Where-Object id -eq $remote
        $repeating = [FixtureMessages]::PostMessage($window, 0x100, [UIntPtr]::new([uint32]0x78), [IntPtr]::new(0x40000000))
        if (!$repeating) { throw 'Could not post the held resize key to the fixture' }
        Post-Key 0x101 0x78
        $held = Wait-Until 'the held key to keep resizing' {
            $current = (Control @('list')).sessions | Where-Object id -eq $remote
            if ($current.cols -gt $ssh.cols) { $current }
        }
        $null = Control @('wait-for-text','--pane',"$remote",'--timeout','12',"ssh-grid-$($held.cols)x$($held.rows)")
        Wait-Focus $remote
        Press-Key 0x77
        Wait-Focus $local
        Press-Key 0x79 # F10, resize left from the other side of the divider
        Press-Key 0x79
        $localGrid = Wait-Until 'PowerShell to regain its original columns' {
            $now = (Control @('list')).sessions
            $shell = $now | Where-Object id -eq $local
            if ($shell.cols -eq $localWidth -and ($now | Where-Object id -eq $remote).cols -eq $remoteWidth) { $shell }
        }
        Wait-Focus $local
        # Ask the shell itself for its ConPTY dimensions, distinct from the echoed command.
        $command = '"local-{0}x{1}" -f [Console]::WindowWidth,[Console]::WindowHeight'
        foreach ($ch in $command.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
        Press-Key 13
        $null = Control @('wait-for-text','--pane',"$local",'--timeout','12',"local-$($localGrid.cols)x$($localGrid.rows)")
    } finally {
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
}
