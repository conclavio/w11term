# The loopback peer records the SSH exec request without executing the command.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State, [Parameter(Mandatory)][string]$Trace)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'settings.json'
$profiles = Join-Path $State 'profiles.json'

function Set-Field([string]$Name, [string]$Value) {
    $edit = Wait-Until $Name { Find-Element $Name ([System.Windows.Automation.ControlType]::Edit) }
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value)
}
foreach ($mode in @('profile', 'workspace')) {
    $name = if ($mode -eq 'profile') { 'fixture' } else { 'recipe' }
    $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), "--$mode", $name) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
    try {
        $window = Wait-Until 'window' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
        $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
        $pane = (Control @('list')).sessions[0].id
        if ($mode -eq 'workspace') {
            $null = Control @('wait-for-text','--pane',"$pane",'--timeout','12','workspace-command-started')
            continue
        }
        $null = Control @('wait-for-text','--pane',"$pane",'--timeout','12','ready')
        Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
        Invoke-Button (Wait-Until 'workspace startup' { Find-Element ('Workspace startup' + [char]0x2026) })
        # Exercise consecutive changes to both fields before applying the draft.
        Set-Field 'Remote startup command' 'initial command'
        Set-Field 'Remote starting directory' "/srv/crew's project"
        Set-Field 'Remote startup command' 'tail -f build.log'
        $preview = 'On reopen ' + [char]0xb7 + " Run tail -f build.log in /srv/crew's project"
        # WinUI posts TextChanged asynchronously after ValuePattern.SetValue returns.
        $null = Wait-Until 'the editor to recognize both changes' { Find-Element $preview }
        Invoke-Button (Wait-Until 'Apply to pane' { Find-Element 'Apply to pane' })
        $null = Wait-Until 'staged workspace command' { Find-Element "$preview. Save or update the workspace to keep this setting." }
        Set-Field 'Workspace name' 'recipe'
        Invoke-Button (Wait-Until 'enabled Save workspace' {
            $button = Find-Element 'Save workspace'
            if ($button -and $button.Current.IsEnabled) { $button }
        })
        $null = Wait-Until 'workspace saved' { Find-Element 'Saved recipe' }
        if ((Get-Item -LiteralPath $Trace).Length -ne 0) { throw 'Editing the live pane dispatched a command' }
    } finally {
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
}
