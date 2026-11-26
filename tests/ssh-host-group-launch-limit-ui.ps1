[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

function Click-Button([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Set-LoginLimit {
    $combo = [System.Windows.Automation.ControlType]::ComboBox
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $option = Wait-Until 'the one-at-a-time login choice' {
        $field = Find-Element 'Parallel SSH logins' $combo
        if (!$field) { return }
        $popup = $field.GetCurrentPattern($expand)
        if ($popup.Current.ExpandCollapseState -eq 'Collapsed') { $popup.Expand() }
        Find-Element '1 at a time' ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $option.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Expect-LoginLimit '1 at a time'
    $field = Find-Element 'Parallel SSH logins' $combo
    $popup = $field.GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}
function Expect-LoginLimit([string]$Choice) {
    $combo = [System.Windows.Automation.ControlType]::ComboBox
    $selection = [System.Windows.Automation.SelectionPattern]::Pattern
    $null = Wait-Until 'the selected login limit' {
        $field = Find-Element 'Parallel SSH logins' $combo
        if (!$field) { return }
        $items = $field.GetCurrentPattern($selection).Current.GetSelection()
        $items.Count -eq 1 -and $items[0].Current.Name -ceq $Choice
    }
}
function Stop-HostLogin($Pane, $OtherPane) {
    $field = Wait-Until 'the host filter' { Find-Element 'Host filter' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Pane.label)
    $null = Wait-Until 'only the intended pending pane is shown' {
        (Find-Element "Focus pane $($Pane.id)" $button) -and
            !(Find-Element "Focus pane $($OtherPane.id)" $button)
    }
    Click-Button 'Stop selected SSH attempts'
    $null = Wait-Until "pane $($Pane.id) stops and can reconnect" {
        $state = Control @('events','--pane',"$($Pane.id)")
        $state.ssh_connection.phase -eq 'stopped' -and $state.restart_token
    }
}

function Wait-QueuedLogins([string]$Phase, [string]$Labels = 'Login *') {
    Wait-Until "one $Phase login awaits a password and the other remains queued" {
        $logins = @((Control @('list')).sessions | Where-Object label -like $Labels)
        $queued = $logins | Where-Object { $_.ssh_connection.phase -eq 'queued' }
        $auth = $logins | Where-Object {
            $_.ssh_connection.phase -eq $Phase -and
                (Find-Element "Pane $($_.id)${dot}Authentication required")
        }
        if ($logins.Count -eq 2 -and $queued -and $auth -and
            (Find-Element "Pane $($queued.id)${dot}Waiting to connect")) {
            @{ queued = $queued; auth = $auth }
        }
    }
}
function Clear-HostFilter {
    $field = Wait-Until 'the roster filter' { Find-Element 'Host filter' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('')
}
function Finish-QueuedLogins($Limits, [string]$Phase) {
    Stop-HostLogin $Limits.auth $Limits.queued
    Clear-HostFilter
    $null = Wait-Until 'the queued login receives the released slot' {
        (Control @('events','--pane',"$($Limits.queued.id)")).ssh_connection.phase -eq $Phase -and
            (Find-Element "Pane $($Limits.queued.id)${dot}Authentication required")
    }
    Stop-HostLogin $Limits.queued $Limits.auth
}
function Check-InitialLogins {
    $limits = Wait-QueuedLogins 'connecting'
    Finish-QueuedLogins $limits 'connecting'
    $ready = (Control @('list')).sessions | Where-Object label -ceq 'Ready'
    $null = Control @('wait-for-text','--pane',"$($ready.id)",'--timeout','15','ready')
    return $limits
}
function Check-BulkReconnect($Initial) {
    Clear-HostFilter
    $null = Wait-Until 'both stopped hosts are shown' {
        (Find-Element "Focus pane $($Initial.queued.id)" $button) -and
            (Find-Element "Focus pane $($Initial.auth.id)" $button)
    }
    Click-Button 'Reconnect selected'
    $limits = Wait-QueuedLogins 'reconnecting'
    $initialIds = @($Initial.auth.id, $Initial.queued.id)
    if ($limits.auth.id -notin $initialIds -or $limits.queued.id -notin $initialIds) {
        throw 'Bulk reconnect replaced the SSH panes'
    }
    Stop-HostLogin $limits.queued $limits.auth
    Click-Button 'Reconnect selected'
    Clear-HostFilter
    $overlap = Wait-QueuedLogins 'reconnecting'
    if ($overlap.auth.id -ne $limits.auth.id -or $overlap.queued.id -ne $limits.queued.id) {
        throw 'An overlapping reconnect changed the active login'
    }
    Finish-QueuedLogins $overlap 'reconnecting'
}
function Check-HostListImport {
    Run-Palette 'New SSH group tab' -Enter
    Invoke-NativeMenuAction 'Saved group actions' ('Import host list' + [char]0x2026)
    $field = Wait-Until 'the host list editor' {
        $item = Find-Element 'SSH host list' $edit
        if ($item -and $item.Current.IsEnabled) { $item }
    }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue(
        "Imported East`r`nImported West")
    Click-Button 'Preview hosts'
    Expect-LoginLimit 'Unlimited'
    Set-LoginLimit
    Click-Button 'Edit host list'
    Click-Button 'Preview hosts'
    Expect-LoginLimit '1 at a time'
    if ((Control @('list')).sessions | Where-Object label -like 'Imported *') {
        throw 'The preview opened an SSH session before approval'
    }
    Click-Button 'Open 2 checked hosts'
    $limits = Wait-QueuedLogins 'connecting' 'Imported *'
    Finish-QueuedLogins $limits 'connecting'
}
function Check-SavedProfileDefault {
    Invoke-NativeMenuAction 'Saved group actions' ('Add saved profiles' + [char]0x2026)
    (Wait-CheckBox 'Add SSH profile Imported East' Off).Toggle()
    $null = Wait-CheckBox 'Add SSH profile Imported East' On
    Click-Button 'Review selected profiles'
    Expect-LoginLimit '1 at a time'
    Click-Button 'Cancel'
}

$launch = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $origin = $window
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    Run-Palette "Review SSH group${dot}Limited" -Enter
    Expect-LoginLimit 'Unlimited'
    Set-LoginLimit
    Click-Button 'Save login limit'
    $null = Wait-Until 'login limit was saved' { Find-Element 'Saved the parallel SSH login limit' }
    $saved = [IO.File]::ReadAllText($profiles) | ConvertFrom-Json
    if ($saved.ssh_host_group_defaults.Limited.login_limit -cne 'one') {
        throw 'The profile file does not contain the selected login limit'
    }
    Click-Button 'Cancel'
    Run-Palette "Review SSH group${dot}Limited" -Enter
    Expect-LoginLimit '1 at a time'
    Click-Button 'Open reviewed group in new window'
    $window = Wait-Until 'the group window' {
        [FixtureDialog]::Windows($app.Id) | Where-Object {
            $_ -ne $origin -and [FixtureDialog]::GetWindow($_, 4) -eq [IntPtr]::Zero
        } | Select-Object -First 1
    }
    $null = Check-InitialLogins
    $groupWindow = $window
    $null = [FixtureMessages]::PostMessage($groupWindow, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero)
    $window = $origin
    $null = Wait-Until 'the dedicated group is closed' {
        [FixtureDialog]::Windows($app.Id) -notcontains $groupWindow
    }
    Run-Palette "Open SSH group${dot}Limited" -Enter
    $initial = Check-InitialLogins
    Check-BulkReconnect $initial
    Check-SavedProfileDefault
    Check-HostListImport
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    if (!$app.HasExited) {
        foreach ($hwnd in [FixtureDialog]::Windows($app.Id)) {
            if ([FixtureDialog]::GetWindow($hwnd, 4) -eq [IntPtr]::Zero) {
                $null = [FixtureMessages]::PostMessage($hwnd, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero)
            }
        }
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
