[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$tabItem = [System.Windows.Automation.ControlType]::TabItem
$button = [System.Windows.Automation.ControlType]::Button
$togglePattern = [System.Windows.Automation.TogglePattern]::Pattern
$valuePattern = [System.Windows.Automation.ValuePattern]::Pattern
$selection = [System.Windows.Automation.SelectionItemPattern]::Pattern
$esc = [char]27
$bell = [char]7

function Wait-Titles([string]$Active, [string]$Background = '') {
    $expected = 'w11term ' + [char]0xb7 + " $Active"
    try {
        $null = Wait-Until "selected tab and window '$Active', background '$Background'" {
            $app.Refresh()
            $selected = Find-Element $Active $tabItem
            if ($app.MainWindowTitle -cne $expected -or !$selected -or
                !$selected.GetCurrentPattern($selection).Current.IsSelected) { return $false }
            if (!$Background) { return $true }
            $other = Find-Element $Background $tabItem
            $other -and !$other.GetCurrentPattern($selection).Current.IsSelected
        }
    } catch {
        $app.Refresh()
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $tabs = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, $tabItem)
        $headers = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$tabs) | ForEach-Object { $_.Current.Name })
        throw "$($_.Exception.Message). Window '$($app.MainWindowTitle)', tab headers [$($headers -join ', ')]."
    }
}
function Write-Remote([string]$Data, [string]$ResultTitle) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Data))
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64',$encoded)
    $null = Wait-Until "SSH title '$ResultTitle'" { (Control @('get-text','--pane',"$remote")).title -ceq $ResultTitle }
}
function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled button '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Menu-Action([string[]]$Path) {
    Click 'Tab menu'
    $menu = [System.Windows.Automation.ControlType]::MenuItem
    foreach ($name in $Path) {
        $item = Wait-Until "menu item '$name'" { Find-Element $name $menu }
        if ($name -ceq $Path[-1]) { Invoke-Button $item }
        else { $item.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand() }
    }
}
function Title-Setting {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $label = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty,'Use application titles for tabs')
    $toggle = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::IsTogglePatternAvailableProperty,$true)
    $element = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.AndCondition]::new($label,$toggle))
    if ($element) { $element.GetCurrentPattern($togglePattern) }
}
function Type-Text([string]$Text) {
    foreach ($c in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
}
function Shell-Witness([string]$Tag) {
    Type-Text ("'$Tag=' + " + '$PID')
    Press-Key 13
    $pattern = "$Tag=([0-9]+)"
    $frame = Control @('wait-for-text','--pane',"$localPane",'--regex','--timeout','15',$pattern)
    return [regex]::Match($frame.text,$pattern).Groups[1].Value
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $origin = (Control @('list')).sessions[0]
    $remote = $origin.id
    $null = Wait-Until 'initial SSH title' { (Control @('get-text','--pane',"$remote")).title -ceq 'ssh-seed' }
    Wait-Titles 'palette-ssh'
    Menu-Action @('Settings')
    Wait-Titles 'Settings' 'palette-ssh'
    $setting = Wait-Until 'application title switch initially off' {
        $switch = Title-Setting
        if ($switch -and $switch.Current.ToggleState -eq 'Off') { $switch }
    }
    $setting.Toggle()
    $null = Wait-Until 'enabled application title setting persisted' {
        (Title-Setting).Current.ToggleState -eq 'On' -and
            (Get-Content $env:W11TERM_SETTINGS_PATH -Raw | ConvertFrom-Json).interaction.applicationTabTitles -eq $true
    }
    Wait-Titles 'Settings' 'ssh-seed'
    Write-Remote "$esc]2;background-build$bell" 'background-build'
    Wait-Titles 'Settings' 'background-build'
    (Find-Element 'background-build' $tabItem).GetCurrentPattern($selection).Select()
    Wait-Focus $remote
    Wait-Titles 'background-build'
    $bufferTitle = 'editor: ' + [char]0x96ea + '.txt'
    Write-Remote "$esc[22;0t$esc]2;$bufferTitle$bell" $bufferTitle
    Wait-Titles $bufferTitle
    Write-Remote "$esc[23;0t" 'background-build'
    Wait-Titles 'background-build'

    Menu-Action @('Panes','Split right with profile','Split right with Palette PowerShell')
    $localPane = Wait-Until 'local shell split beside SSH' {
        $p = (Control @('list')).sessions | Where-Object label -ceq 'Palette PowerShell'
        if ($p.active -and $p.tab_id -eq $origin.tab_id) { $p.id }
    }
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','20','PS ')
    $pidBefore = Shell-Witness 'title-before'
    Type-Text '$Host.UI.RawUI.WindowTitle = "local-workbench"'
    Press-Key 13
    $localTitle = Wait-Until 'native PowerShell title from ConPTY' {
        $current = (Control @('get-text','--pane',"$localPane")).title
        if ($current -and $current.EndsWith('local-workbench')) { $current }
    }
    Wait-Titles $localTitle
    Write-Remote "$esc]2;unfocused-watch$bell" 'unfocused-watch'
    Wait-Focus $localPane
    Wait-Titles $localTitle
    Menu-Action @('Panes','Focus left    Ctrl+Alt+Left')
    Wait-Focus $remote
    Wait-Titles 'unfocused-watch'

    $rename = 'Rename tab' + [char]0x2026
    Menu-Action @($rename)
    $field = Wait-Until 'tab name edit' { Find-Element 'Tab name' ([System.Windows.Automation.ControlType]::Edit) }
    $field.GetCurrentPattern($valuePattern).SetValue('Operations')
    Click 'Save'
    Wait-Titles 'Operations'
    Write-Remote "$esc]2;renamed-build$bell" 'renamed-build'
    Wait-Titles 'Operations'
    Menu-Action @('Panes','Focus right    Ctrl+Alt+Right')
    Wait-Focus $localPane
    Wait-Titles 'Operations'
    if ((Shell-Witness 'title-after') -cne $pidBefore) { throw 'The neighboring PowerShell process was replaced' }
    $panes = @((Control @('list')).sessions | Where-Object tab_id -eq $origin.tab_id)
    if ($panes.Count -ne 2 -or @($panes | Where-Object tab_title -cne 'Operations').Count -or
        ($panes | Where-Object id -eq $remote).title -cne 'renamed-build') { throw 'Application titles changed the saved name or raw pane metadata' }
    Menu-Action @('Panes','Focus left    Ctrl+Alt+Left')
    Wait-Focus $remote
    Menu-Action @($rename)
    Click 'Use default'
    Wait-Titles 'renamed-build'
    if (@((Control @('list')).sessions | Where-Object tab_title).Count) { throw 'Application title was persisted as a manual rename' }
    Write-Remote "$esc]2;$bell" ''
    Wait-Titles 'palette-ssh'
    Type-Text 'ssh-title-controls-ok'
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ssh-title-controls-ok')
    $current = (Control @('list')).sessions | Where-Object id -eq $remote
    if ($current.tab_id -ne $origin.tab_id -or $current.connection_generation -ne $origin.connection_generation) { throw 'The original SSH connection was replaced' }
} catch {
    throw "$($_.Exception.Message) at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
