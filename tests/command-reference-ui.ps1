[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

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
function Select-InstalledTool([string]$Name) {
    Click 'Installed SSH tools'
    $tools = Wait-Until 'active remote tools offered lookup' { Find-Element 'Installed SSH commands' $combo }
    $tools.SetFocus()
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $tools.GetCurrentPattern($expand).Expand()
    $choice = Wait-Until "installed $Name from the portable bundle" {
        Find-Element "${Name}${dot}portable" ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
}
function Set-Manual([string]$Text) {
    $field = Wait-Until 'editable private Markdown box' {
        $control = Find-Element 'Private page Markdown' $edit
        if ($control -and $control.Current.IsEnabled) { $control }
    }
    $field.SetFocus()
    $markdownLf = $Text.Replace("`r`n", "`n").Replace("`r", "`n")
    $field.GetCurrentPattern($value).SetValue($markdownLf.Replace("`n", "`r"))
    $null = Wait-Until 'complete private Markdown in the editor' {
        $actual = (Find-Element 'Private page Markdown' $edit).GetCurrentPattern($value).Current.Value
        $actual.Replace("`r`n", "`n").Replace("`r", "`n") -ceq $markdownLf
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
    Set-Field 'Search command reference' 'mddog'
    $null = Wait-Until 'project name finds the bundled mdd manual offline' {
        (Find-Element 'mdd') -and (Find-Element "Linux${dot}w11term manual")
    }
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
    Select-InstalledTool 'rg'
    $null = Wait-Until 'installed command opened its private Linux reference' {
        (Find-Element 'Team rg notes') -and (Find-Element "Linux${dot}Custom page")
    }
    Select-InstalledTool 'mdd'
    $null = Wait-Until 'installed mdd opened its bundled manual' {
        (Find-Element 'mdd') -and (Find-Element "Linux${dot}w11term manual")
    }
    Click 'Install SSH tools'
    $null = Wait-Until 'configured profile manifest offered' { Find-Element (Join-Path $State 'fleet.json') }
    Click 'Browse for tool bundle'
    $manifestDialog = Wait-Until 'owned tool manifest picker' { Find-OwnedDialog 'Choose SSH tool bundle' }
    Select-DialogItem $manifestDialog 'Tool review.json'
    $null = Wait-Until 'select the update manifest' { [FixtureDialog]::ClickButton($manifestDialog, 1) }
    $null = Wait-Until 'chosen update manifest shown' { Find-Element (Join-Path $State 'Tool review.json') }
    Click 'Review bundle'
    $null = Wait-Until 'existing launcher blocks installation' {
        $install = Find-Element 'Install reviewed bundle' $button
        (Find-Element "netwatch${dot}Existing entry at /.local/bin/netwatch") -and !$install.Current.IsEnabled
    }
    $conflict = [IO.File]::ReadAllText((Join-Path $State 'tool-launcher-conflict'))
    if ([IO.File]::ReadAllText($conflict) -cne 'existing fixture command') { throw 'Review changed an existing command' }
    $remoteLocal = Split-Path -Parent (Split-Path -Parent $conflict)
    $selectorPath = Join-Path $remoteLocal 'share/w11term/tools/portable/current'
    $oldId = ([IO.File]::ReadAllText($selectorPath).Trim() -split '/')[-1]
    [IO.File]::Delete($conflict)
    Click 'Review bundle'
    $null = Wait-Until 'installation plan shows the new launcher' { Find-Element "netwatch${dot}Create /.local/bin/netwatch" }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'the other pane cannot apply the original review' {
        $install = Find-Element 'Install reviewed bundle' $button
        $install -and !$install.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click 'Install reviewed bundle'
    $null = Wait-Until 'installation completed' { Find-Element 'Installed tool bundle portable' }
    Click 'Back to command reference'
    Select-InstalledTool 'netwatch'
    $null = Wait-Until 'newly installed tool is visible on the same pane' {
        (Find-Element 'netwatch') -and (Find-Element "Linux${dot}w11term manual")
    }
    Click 'Install SSH tools'
    Click 'Bundle versions'
    $priorLabel = "portable${dot}$($oldId.Substring(0,12))"
    $expandPattern = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $selectPattern = [System.Windows.Automation.SelectionItemPattern]::Pattern
    $null = Wait-Until "select retained bundle '$priorLabel'" {
        if (Find-Element "SHA-256 $oldId") { return $true }
        $history = Find-Element 'Retained tool bundle versions' $combo
        if ($history -and $history.Current.IsEnabled) {
            try {
                $popup = $history.GetCurrentPattern($expandPattern)
                if ($popup.Current.ExpandCollapseState -eq [System.Windows.Automation.ExpandCollapseState]::Collapsed) {
                    $history.SetFocus()
                    $popup.Expand()
                } else {
                    $prior = Find-Element $priorLabel ([System.Windows.Automation.ControlType]::ListItem)
                    if ($prior) { $prior.GetCurrentPattern($selectPattern).Select() }
                }
            } catch [System.Windows.Automation.ElementNotAvailableException] {
                # WinUI replaces the dropdown's automation elements when it opens.
            }
        }
    }
    Click 'Review rollback'
    $null = Wait-Until 'rollback shows the prior contents and update policy' {
        (Find-Element 'Retained commands mdd, rg') -and
        (Find-Element 'Restoring this version pauses automatic bundle updates until you apply a bundle again.')
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'the other pane cannot restore the retained bundle' {
        $restore = Find-Element 'Restore reviewed version' $button
        $restore -and !$restore.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click 'Restore reviewed version'
    $null = Wait-Until 'rollback restored and pinned the original version' {
        (Find-Element 'Restored tool bundle portable') -and
        [IO.File]::ReadAllText($selectorPath).Contains("./versions/$oldId")
    }
    Click 'Back to tool installer'
    Click 'Back to command reference'
    Set-Field 'Search command reference' 'crew report'
    Click 'New private page'
    $manual = @'
# Crew runbook

> Check crew status.

- Show report:

`crew-report --summary`
'@
    Set-Manual $manual
    Click 'Save private page'
    $null = Wait-Until 'saved manual shown immediately' {
        (Find-Element 'Crew runbook') -and (Find-Element 'crew-report --summary')
    }
    $manualFile = Join-Path $privatePages 'crew-report.md'
    if (![IO.File]::Exists($manualFile)) { throw 'Private manual was saved in the wrong platform directory' }
    Click 'Edit private page'
    $null = Wait-Until 'existing multiline page seeded without losing the example' {
        $field = Find-Element 'Private page Markdown' $edit
        $field -and $field.Current.IsEnabled -and $field.GetCurrentPattern($value).Current.Value.Contains('crew-report --summary')
    }
    Set-Manual ($manual.Replace('Crew runbook', 'Crew runbook updated').Replace('--summary','--details'))
    Click 'Save private page'
    $null = Wait-Until 'edited manual shown' {
        (Find-Element 'Crew runbook updated') -and (Find-Element 'crew-report --details')
    }
    $saved = [IO.File]::ReadAllText($manualFile)
    Click 'Edit private page'
    Set-Manual ($manual.Replace('Crew runbook','Discarded draft'))
    Click 'Cancel private page edit'
    $null = Wait-Until 'cancel returns to the published page' { Find-Element 'Crew runbook updated' }
    if ([IO.File]::ReadAllText($manualFile) -cne $saved) { throw 'Cancel changed the saved file' }
    Click 'Close command reference'
    $null = Wait-Until 'closed command reference before reload' { !(Find-Element 'Search command reference' $edit) }
    Run-Palette 'Command reference'
    $null = Wait-Until 'the offline catalogue displays the edited page after reopening' { Find-Element 'Crew runbook updated' }
    if ([IO.File]::ReadAllText($wire)) { throw 'Browsing or editing a manual wrote to the SSH shell' }
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
