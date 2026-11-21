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
    Invoke-Button (Wait-Until 'save binding' { Find-Element 'Save shortcut' })
    $null = Wait-Until "saved $After binding" {
        if (Test-Path -LiteralPath $env:W11TERM_SETTINGS_PATH) {
            (Get-Content -Raw -LiteralPath $env:W11TERM_SETTINGS_PATH | ConvertFrom-Json).shortcuts.focus_right -eq $After
        }
    }
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
        $wanted = [string]::new([char]27, 1) + '[18~'
        $null = Wait-Until 'released F7 on the SSH wire' {
            [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($Trace)) -ceq ($wanted * $launch)
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
