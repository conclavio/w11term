[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$menuItem = [System.Windows.Automation.ControlType]::MenuItem
$tab = [System.Windows.Automation.ControlType]::TabItem

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    Invoke-Button (Wait-Until 'tab menu button' { Find-Element 'Tab menu' $button })
    $original = Wait-Until 'original Settings menu item' { Find-Element 'Settings' $menuItem }
    $invoke = $original.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)

    foreach ($percent in 20, 40, 60, 80) {
        $osc = [Text.Encoding]::ASCII.GetBytes("$([char]27)]9;4;1;$percent$([char]7)")
        $null = Control @('send-text','--pane',"$pane",'--no-paste','--base64',[Convert]::ToBase64String($osc))
        $caption = "$percent% $([char]0xb7) palette-ssh"
        $null = Wait-Until "native tab header '$caption'" { Find-Element $caption $tab }
        $shown = Find-Element 'Settings' $menuItem
        if (!$shown -or ![System.Windows.Automation.Automation]::Compare($original, $shown)) {
            throw "The visible Settings item disappeared or was replaced at $percent% progress."
        }
    }
    $invoke.Invoke()
    $null = Wait-Until 'Settings page opened by the original flyout item' { Find-Element 'Edit profile palette-ssh' $button }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
