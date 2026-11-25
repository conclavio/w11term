[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/directory-actions.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_TEST_COPY_FILE = Join-Path $State 'clipboard.txt'
$profiles = Join-Path $State 'profiles.json'
$destination = [IO.File]::ReadAllText((Join-Path $State 'destination'))
$right = 'Split right in current directory'
$below = 'Split below in current directory'
$button = [System.Windows.Automation.ControlType]::Button
$menuItem = [System.Windows.Automation.ControlType]::MenuItem

function Open-PanesMenu {
    Invoke-Button (Wait-Until 'caret menu' { Find-Element 'Tab menu' $button })
    $panes = Wait-Until 'Panes submenu' { Find-Element 'Panes' $menuItem }
    $panes.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
}
function Send-To([int]$Pane, [string]$Text, [string]$Reply) {
    $null = Control @('send-text','--pane',"$Pane",'--no-paste',$Text)
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15',$Reply)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','128','--rows','32') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $source = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$source",'--timeout','15','ready')
    Check-DirectoryActions $false
    Send-To $source 'report-current' 'current-reported'
    Check-DirectoryActions $true
    if ([IO.File]::Exists((Join-Path $State 'iterm2'))) {
        Send-To $source 'nested-host' 'nested-host-seen'
        Check-DirectoryActions $false
        Send-To $source 'nested-dir' 'nested-dir-seen'
        Check-DirectoryActions $false
        Send-To $source 'other-login' 'other-login-seen'
        Check-DirectoryActions $false
        Send-To $source 'login-host' 'login-host-seen'
        Check-DirectoryActions $false
        Send-To $source 'literal-dir' 'literal-dir-seen'
        $expected = "/srv/Pilot's 100%2F;#" + [char]0x96ea
    } else {
        Send-To $source 'report-foreign' 'foreign-reported'
        Check-DirectoryActions $false
        Send-To $source 'report-current-again' 'current-confirmed'
        $expected = "/srv/Pilot's Work"
    }
    if (@((Control @('list')).sessions).Count -ne 1) { throw 'Directory reports unexpectedly created a session' }
    Check-DirectoryActions $true
    if ($destination -eq 'below') {
        Open-PanesMenu
        Invoke-Button (Wait-Until 'enabled Copy current directory menu item' {
            $item = Find-Element 'Copy current directory' $menuItem
            if ($item -and $item.Current.IsEnabled) { $item }
        })
    } else {
        Run-Palette 'Copy current directory'
    }
    $null = Wait-Until 'the exact remote folder was copied' {
        (Read-ClipboardFixture) -ceq $expected
    }
    $original = (Control @('list')).sessions | Where-Object id -eq $source
    if ($destination -eq 'right') {
        Run-Palette $right
    } else {
        Open-PanesMenu
        $command = if ($destination -eq 'tab') { 'New tab in current directory' } else { $below }
        Invoke-Button (Wait-Until "enabled $command menu item" {
            $item = Find-Element $command $menuItem
            if ($item -and $item.Current.IsEnabled) { $item }
        })
    }
    $child = (Wait-DirectoryChild $original 2 $destination).id
    $null = Control @('wait-for-text','--pane',"$child",'--timeout','15','workspace-command-started')
    Send-To $source 'parent-live' 'parent-live'
    Wait-Focus $child
    foreach ($c in 'child-live'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$child",'--timeout','15','child-live')
    $originalScreen = (Control @('get-text','--pane',"$source")).text
    $childScreen = (Control @('get-text','--pane',"$child")).text
    if ($originalScreen.Contains('child-live') -or $originalScreen.Contains('workspace-command-started') -or
        $childScreen.Contains('parent-live') -or $childScreen.Contains('current-reported')) { throw 'The two SSH channels mixed output or input' }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
