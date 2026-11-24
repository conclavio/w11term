[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$config = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$next = 'Open next SSH prompt'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Set-HostSearch([string]$Text) {
    $field = Wait-Until 'the host search field' { Find-Element 'Host filter' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Text)
}
function Select-PromptGroup {
    $tab = Wait-Until 'the prompt review group tab' {
        Find-Element "SSH group${dot}Prompt review" ([System.Windows.Automation.ControlType]::TabItem)
    }
    $tab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'return to the group roster' { Find-Element 'Host filter' $edit }
}
function Filter-HostState([string]$Name) {
    $combo = [System.Windows.Automation.ControlType]::ComboBox
    $list = [System.Windows.Automation.ControlType]::ListItem
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $item = Wait-Until "the '$Name' state option" {
        $field = Find-Element 'Filter host state' $combo
        if (!$field) { return }
        $popup = $field.GetCurrentPattern($expand)
        if ($popup.Current.ExpandCollapseState -eq 'Collapsed') { $popup.Expand() }
        $option = Find-Element $Name $list
        if ($option -and !$option.Current.IsOffscreen) { $option }
    }
    $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $field = Wait-Until "the host state is '$Name'" {
        $current = Find-Element 'Filter host state' $combo
        if (!$current) { return }
        $selected = $current.GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
        if ($selected.Count -eq 1 -and $selected[0].Current.Name -ceq $Name) { $current }
    }
    $popup = $field.GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}

$argsForApp = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $argsForApp -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'the control pipe' {
        Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json")
    }
    Run-Palette "Open SSH group${dot}Prompt review"
    $sessions = Wait-Until 'the two SSH panes are created' {
        $live = @((Control @('list')).sessions)
        if ($live.Count -eq 3) { ,$live }
    }
    $trust = ($sessions | Where-Object label -ceq 'Trust host').id
    $login = ($sessions | Where-Object label -ceq 'Password host').id
    $loginName = "Password host${dot}test@127.0.0.1:$($config.'Password host'.port)${dot}Pane $login"
    $null = Wait-Until 'both prompts appear in the roster without leaving the group' {
        (Find-Element "Pane $trust${dot}Host key verification required") -and
            (Find-Element "Pane $login${dot}Authentication required") -and
            (Find-Element $next $button)
    }
    (Wait-CheckBox $loginName On).Toggle()
    $null = Wait-CheckBox $loginName Off
    Set-HostSearch 'PASSWORD'
    $null = Wait-Until 'the host search hides the trust prompt' { !(Find-Element "Focus pane $trust" $button) }
    Run-Palette 'Open next SSH sign-in or host-key prompt' -Enter
    Wait-Focus $login
    $null = Wait-Until 'the unchecked host opens its real sign-in dialog' {
        Find-Element 'Sign in' $button
    }
    Click 'Cancel'
    $null = Wait-Until 'the sign-in cancellation reaches the connection' {
        !(Find-Element 'Sign in' $button) -and (Control @('events','--pane',"$login")).restart_token
    }
    Select-PromptGroup
    $null = Wait-Until 'the group selection and search are restored' {
        $field = Find-Element 'Host filter' $edit
        $field -and $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq 'PASSWORD'
    }
    $null = Wait-CheckBox $loginName Off
    Set-HostSearch ''
    Filter-HostState 'Host key verification required'
    $null = Wait-Until 'the trust filter scopes the prompt action to that pane' {
        (Find-Element "Focus pane $trust" $button) -and !(Find-Element "Focus pane $login" $button)
    }
    Click $next
    Wait-Focus $trust
    $null = Wait-Until 'the native host-key dialog is shown' {
        (Find-Element 'Trust this SSH host?') -and (Find-Element 'Accept and save' $button)
    }
    Click 'Cancel'
    $null = Wait-Until 'the host-key cancellation reaches the connection' {
        !(Find-Element 'Accept and save' $button) -and (Control @('events','--pane',"$trust")).restart_token
    }
    Select-PromptGroup
    Filter-HostState 'All host states'
    $null = Wait-Until 'cancelled prompts leave no pending action' {
        !(Find-Element "Pane $trust${dot}Host key verification required") -and
            !(Find-Element "Pane $login${dot}Authentication required") -and
            (Find-Element "Focus pane $trust" $button) -and (Find-Element "Focus pane $login" $button) -and
            !(Find-Element $next $button)
    }
} catch {
    $failure = $_
    if ($window) {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $uia = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.Condition]::TrueCondition)
        $names = foreach ($item in $uia) {
            if ($item.Current.Name -match 'Pane |next SSH prompt|host states|Host filter|Cancel|Sign in|Accept and save') {
                "$($item.Current.Name) [offscreen=$($item.Current.IsOffscreen)]"
            }
        }
        Write-Output ($names -join [Environment]::NewLine)
    }
    throw ($failure.Exception.Message + [Environment]::NewLine + $failure.ScriptStackTrace)
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
