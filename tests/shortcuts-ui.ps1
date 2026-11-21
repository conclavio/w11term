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
        $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
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
        Press-Key 0x77
        Wait-Focus $local
        # Splitting the string makes the echoed command distinct from its result.
        foreach ($ch in "'local-' + 'result-$launch'".ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
        Press-Key 13
        $null = Control @('wait-for-text','--pane',"$local",'--timeout','12',"local-result-$launch")
    } finally {
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
}
