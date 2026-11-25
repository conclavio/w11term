[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$menu = [System.Windows.Automation.ControlType]::MenuItem
$tab = [System.Windows.Automation.ControlType]::TabItem
$toggle = [System.Windows.Automation.TogglePattern]::Pattern

function Click([string]$Name, [System.Windows.Automation.ControlType]$Type = $button) {
    try {
        Invoke-Button (Wait-Until "enabled '$Name'" {
            $element = Find-Element $Name $Type -Visible
            if ($element -and $element.Current.IsEnabled) { $element }
        })
    } catch {
        $element = Find-Element $Name $Type
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        throw "$_ Element=$($element.Current.BoundingRectangle) Offscreen=$($element.Current.IsOffscreen) Enabled=$($element.Current.IsEnabled) Window=$($root.Current.BoundingRectangle)"
    }
}

function Title-Files {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $id = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'w11term-title-files')
    return $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $id)
}

function Wait-FilesToggle([string]$Expected) {
    return Wait-Until "title Files toggle in state $Expected" {
        $item = Title-Files
        if ($item -and $item.Current.IsEnabled -and !$item.Current.IsOffscreen) {
            $state = $item.GetCurrentPattern($toggle)
            if ($state.Current.ToggleState -eq $Expected) { $state }
        }
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh', '--cols', '110', '--rows', '28') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control manifest' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    (Wait-FilesToggle 'Off').Toggle()
    $null = Wait-FilesToggle 'On'
    $null = Wait-Until 'remote folder listing from title bar' { Find-Element 'title-tools.txt' }
    (Wait-FilesToggle 'On').Toggle()
    $null = Wait-FilesToggle 'Off'
    $null = Wait-Until 'remote file drawer closed' { !(Find-Element 'title-tools.txt') }

    Click 'Find in terminal scrollback'
    $null = Wait-Until 'terminal Find opened from title bar' { Find-Element 'Find in scrollback' $edit }
    Press-Key 0x1b
    $null = Wait-Until 'terminal Find closed' { !(Find-Element 'Find in scrollback' $edit) }
    Click 'Command palette'
    $null = Wait-Until 'command palette opened from title bar' { Find-Element 'Search commands' $edit }
    Press-Key 0x1b
    $null = Wait-Until 'command palette closed' { !(Find-Element 'Search commands' $edit) }

    Click 'New tab'
    Click ('Palette PowerShell ' + [char]0xb7 + ' Local') $menu
    $local = Wait-Until 'PowerShell tab' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $local
    $null = Wait-Until 'SFTP toggle disabled for PowerShell' {
        $item = Title-Files
        $item -and !$item.Current.IsEnabled -and $item.GetCurrentPattern($toggle).Current.ToggleState -eq 'Off'
    }
    Click 'Pane layout'
    Click 'Split right    Ctrl+Alt+Shift+Right' $menu
    $split = Wait-Until 'second PowerShell pane from the layout menu' {
        ((Control @('list')).sessions | Where-Object { $_.label -eq 'Palette PowerShell' -and $_.id -ne $local }).id
    }
    Wait-Focus $split
    $null = Control @('wait-for-text','--pane',"$split",'--timeout','15','PS ')
    Click 'Tab menu'
    Click 'Settings' $menu
    $null = Wait-Until 'Settings opened from More' {
        (Find-Element 'Settings' $tab) -and (Find-Element 'Profiles')
    }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
