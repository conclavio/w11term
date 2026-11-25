param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$env:USERPROFILE = Join-Path $State 'ssh-home'
$null = [IO.Directory]::CreateDirectory((Join-Path $env:USERPROFILE '.ssh'))
$known = Join-Path $env:USERPROFILE '.ssh/known_hosts'
$before = [IO.File]::ReadAllText($profiles)
$peer = ($before | ConvertFrom-Json).profiles.'palette-ssh'
$endpoint = "$($peer.user)@$($peer.host):$($peer.port)"
$label = 'Quick connect' + [char]0x2026
$menu = [System.Windows.Automation.ControlType]::MenuItem
$edit = [System.Windows.Automation.ControlType]::Edit
$combo = [System.Windows.Automation.ControlType]::ComboBox
$tab = [System.Windows.Automation.ControlType]::TabItem
$item = [System.Windows.Automation.ControlType]::ListItem
$value = [System.Windows.Automation.ValuePattern]::Pattern

function Click([string]$Name, $Type = $button) {
    Invoke-Button (Wait-Until "enabled '$Name' control" {
        $control = Find-Element $Name $Type
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}
function Address {
    try {
        Wait-Until 'SSH destination field' {
            $field = Find-Element 'SSH destination' $edit
            if ($field -and ([FixtureMessages]::GetForegroundWindow() -ne $window -or $field.Current.HasKeyboardFocus)) {
                $field.GetCurrentPattern($value)
            }
        }
    } catch {
        $mounted = $null -ne (Find-Element 'SSH destination' $edit)
        $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
        $focus = if ($focused -and $focused.Current.ProcessId -eq $app.Id) { $focused.Current.Name } else { '(outside fixture)' }
        throw "$_ Address mounted: $mounted. Foreground HWND: $([FixtureMessages]::GetForegroundWindow()), fixture HWND: $window. Focus: $focus"
    }
}
function Stop-Fixture {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
function Wait-Control {
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
}
function Wait-Connected {
    $id = Wait-Until 'one-off SSH tab becomes the active pane' {
        $pane = Sessions | Where-Object { $_.active -and $_.label -ceq $endpoint }
        if ($pane) { $pane.id }
    }
    Wait-Text $id 'ready'
    Send-Remote $id 'quick-connect-echo'
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    Wait-Control
    Click 'New tab'
    Click $label $menu
    (Address).SetValue("ssh://$endpoint/path")
    Click 'Connect'
    $null = Wait-Until 'invalid address message' { Find-Element 'Enter an SSH address without spaces, a path, query, or fragment' }

    Press-Key 0x77 # Select the original terminal lab tab.
    $null = Wait-Until 'Quick Connect editor hidden' { !(Find-Element 'SSH destination' $edit) }
    Run-Palette $label
    $address = Address
    if ($address.Current.Value -cne "ssh://$endpoint/path") { throw 'The palette did not reopen the current draft' }
    $address.SetValue("ssh://$endpoint")

    $authentication = Wait-Until 'authentication selector' { Find-Element 'Authentication' $combo }
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $selection = [System.Windows.Automation.SelectionPattern]::Pattern
    $authentication.GetCurrentPattern($expand).Expand()
    $keyless = Wait-Until 'visible keyless choice' {
        $control = Find-Element 'None (keyless server)' $item
        if ($control -and !$control.Current.IsOffscreen) { $control }
    }
    $keyless.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'keyless authentication selected' {
        $control = Find-Element 'Authentication' $combo
        if ($control) {
            $selected = $control.GetCurrentPattern($selection).Current.GetSelection()
            $selected.Count -eq 1 -and $selected[0].Current.Name -ceq 'None (keyless server)'
        }
    }
    $popup = (Find-Element 'Authentication' $combo).GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
    Click 'Connect'
    Click 'Accept and save'
    Wait-Connected
    if (Find-Element 'Quick connect' $tab) { throw 'The editor did not become the SSH tab' }
    if (!(Test-Path -LiteralPath $known) -or !(Get-Content -LiteralPath $known -Raw).Contains("[$($peer.host)]:$($peer.port)")) {
        throw 'Quick Connect did not save the accepted host key in the fixture home'
    }
    if ([IO.File]::ReadAllText($profiles) -cne $before) { throw 'Quick Connect changed the profiles catalog' }
} finally {
    Stop-Fixture
}

$app = Start-Process -FilePath $Gui -ArgumentList @('ssh',"ssh://$endpoint",'--auth','none','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'cli-gui.log')
try {
    $window = Wait-InputWindow
    Wait-Control
    Wait-Connected
    if (Find-Element 'Accept and save' $button) { throw 'The CLI launch did not reuse the verified host key' }
    if ([IO.File]::ReadAllText($profiles) -cne $before) { throw 'The direct SSH launch changed the profiles catalog' }
} finally {
    Stop-Fixture
}
