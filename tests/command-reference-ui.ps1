[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$edit = [System.Windows.Automation.ControlType]::Edit
$combo = [System.Windows.Automation.ControlType]::ComboBox
$selection = [System.Windows.Automation.SelectionPattern]::Pattern
$value = [System.Windows.Automation.ValuePattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '
$wire = [IO.File]::ReadAllText((Join-Path $State 'ssh-wire'))
$privatePages = Join-Path $env:W11TERM_DATA_DIR 'command-reference/pages/linux'
$null = [IO.Directory]::CreateDirectory($privatePages)
[IO.File]::WriteAllText((Join-Path $privatePages 'rg.md'), @'
# Team rg notes

> Search code in the team workspace.

- Search source:

`rg {{pattern}} src`
'@)

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $button
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}
function Set-Field([string]$Name, [string]$Text) {
    $field = Wait-Until "'$Name' field" { Find-Element $Name $edit }
    $field.SetFocus()
    $field = Wait-Until "'$Name' editor focused" {
        $current = Find-Element $Name $edit -Visible
        if ($current -and $current.Current.HasKeyboardFocus) { $current }
    }
    $field.GetCurrentPattern($value).SetValue($Text)
    $null = Wait-Until "'$Name' contains the entered value" {
        (Find-Element $Name $edit).GetCurrentPattern($value).Current.Value -ceq $Text
    }
}
function Search([string]$Command, [string]$Platform) {
    Set-Field 'Search command reference' $Command
    $null = Wait-Until "$Command page for $Platform" {
        (Find-Element $Command) -and (Find-Element "$Platform${dot}tldr-pages (CC BY 4.0)")
    }
}
function Wait-Platform([string]$Name) {
    $null = Wait-Until "'$Name' platform selection" {
        $selector = Find-Element 'Command reference platform' $combo
        if ($selector) {
            $choice = $selector.GetCurrentPattern($selection).Current.GetSelection()
            $choice.Count -eq 1 -and $choice[0].Current.Name -ceq $Name
        }
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $localPane = ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id
    Wait-Text $localPane 'PS '
    Run-Palette 'Command reference'
    $null = Wait-Until 'offline TLDR catalogue' { Find-Element "Bundled TLDR pages${dot}v2.3" }
    $null = Wait-Until 'reference search has keyboard focus' {
        $field = Find-Element 'Search command reference' $edit
        $field -and $field.Current.HasKeyboardFocus
    }
    Wait-Platform 'Auto (Windows)'
    if (Find-Element 'Installed SSH tools' $button) { throw 'A PowerShell pane offered SSH tool lookup' }
    Search 'rg' 'Common'
    Search 'Get-ChildItem' 'Windows'
    Click 'Edit example 1'
    $null = Wait-Until 'PowerShell command draft' {
        $field = Find-Element 'Edit command example' $edit
        $field -and $field.GetCurrentPattern($value).Current.Value -ceq 'Get-ChildItem'
    }

    $remotePane = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $remotePane
    $null = Control @('wait-ssh-connected','--pane',"$remotePane",'--timeout','15')
    Wait-Platform 'Auto (Linux)'
    $null = Wait-Until 'example from the other pane cannot be inserted' {
        $insert = Find-Element 'Insert into terminal' $button
        !$insert -or !$insert.Current.IsEnabled
    }
    Click 'Installed SSH tools'
    $tools = Wait-Until 'active remote tools offered lookup' { Find-Element 'Installed SSH commands' $combo }
    $tools.SetFocus()
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $tools.GetCurrentPattern($expand).Expand()
    $choice = Wait-Until 'installed rg from the portable bundle' {
        Find-Element "rg${dot}portable" ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'installed command opened its private Linux reference' {
        (Find-Element 'Team rg notes') -and (Find-Element "Linux${dot}Custom page")
    }
    if ([IO.File]::ReadAllText($wire)) { throw 'Looking up a managed tool wrote to the SSH shell' }
    Search 'ip' 'Linux'
    Click 'Edit example 1'
    Set-Field 'Edit command example' 'ip address'
    Click 'Insert into terminal'
    $null = Wait-Until 'edited example reaches SSH' {
        try { [IO.File]::ReadAllText($wire) -ceq 'ip address' } catch [IO.IOException] { $false }
    }

    $selector = Find-Element 'Command reference platform' $combo
    $selector.SetFocus()
    $popup = $selector.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
    $popup.Expand()
    $choice = Wait-Until 'visible Windows override' {
        Find-Element 'Windows' ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
    Wait-Platform 'Windows'
    Search 'Get-ChildItem' 'Windows'
    Click 'Close command reference'
    $null = Wait-Until 'close command reference' { !(Find-Element 'Search command reference' $edit) }
    Wait-Focus $remotePane
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
