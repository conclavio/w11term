param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
. (Join-Path $PSScriptRoot 'support/profile-scheme-editor.ps1')

$tab = [System.Windows.Automation.ControlType]::TabItem

function Select-TabColor([string]$Name) {
    $field = Wait-Until 'tab color selector' { Find-Element 'Tab color' $combo }
    $field.SetFocus()
    $field = Wait-Until 'visible tab color selector' {
        $box = Find-Element 'Tab color' $combo
        if ($box -and !$box.Current.IsOffscreen) { $box }
    }
    $field.GetCurrentPattern($expand).Expand()
    $choice = Wait-Until "visible tab color '$Name'" {
        $item = Find-Element $Name $listItem
        if ($item -and !$item.Current.IsOffscreen) { $item }
    }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $field = Wait-Until "selected tab color '$Name'" {
        $box = Find-Element 'Tab color' $combo
        if ($box) {
            $chosen = $box.GetCurrentPattern($selection).Current.GetSelection()
            if ($chosen.Count -eq 1 -and $chosen[0].Current.Name -ceq $Name) { $box }
        }
    }
    $popup = $field.GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}

function Selected-Tab([string]$Name) {
    $null = Wait-Until "selected tab '$Name'" {
        $item = Find-Element $Name $tab
        $item -and $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Current.IsSelected
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','132','--rows','36') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $original = @(Sessions)[0].id
    Wait-Text $original 'ready'
    Selected-Tab 'palette-ssh'

    Run-Palette 'Settings'
    Click 'Edit local profile Palette PowerShell'
    Select-TabColor 'Green'
    Click 'Save profile'
    Click 'Edit profile palette-ssh'
    Select-TabColor 'Red'
    Click 'Save profile changes'
    $null = Wait-Until 'SSH profile changes saved' { Find-Element 'Changes saved.' }
    $saved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles
    if ($saved.'palette-ssh'.tab_color -cne 'red' -or $saved.'Palette PowerShell'.tab_color -cne 'green') {
        throw 'The profile editors did not save the selected tab colors'
    }

    Press-Key 0x77 # F8 selects the original SSH tab
    Wait-Focus $original
    Selected-Tab 'palette-ssh'
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'active PowerShell in the original SSH tab' {
        $pane = Sessions | Where-Object { $_.active -and $_.label -eq 'Palette PowerShell' }
        if ($pane) { $pane.id }
    }
    Wait-Text $local 'PS '
    Selected-Tab 'Palette PowerShell (Green tab)'
    Press-Key 0x73 # F4 returns focus to the original SSH pane
    Wait-Focus $original
    Selected-Tab 'palette-ssh'

    $new = (Control @('open','--profile','palette-ssh','--activate')).pane
    if ($new -eq $original) { throw 'Opening the profile reused the old SSH pane' }
    Wait-Focus $new
    Wait-Text $new 'ready'
    Selected-Tab 'palette-ssh (Red tab)'
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
