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
    $selection = [System.Windows.Automation.SelectionPattern]::Pattern
    $option = Wait-Until 'the one-at-a-time login choice' {
        $field = Find-Element 'Parallel SSH logins' $combo
        if (!$field) { return }
        $popup = $field.GetCurrentPattern($expand)
        if ($popup.Current.ExpandCollapseState -eq 'Collapsed') { $popup.Expand() }
        Find-Element '1 at a time' ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $option.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'the selected login limit' {
        $field = Find-Element 'Parallel SSH logins' $combo
        if (!$field) { return }
        $items = $field.GetCurrentPattern($selection).Current.GetSelection()
        $items.Count -eq 1 -and $items[0].Current.Name -ceq '1 at a time'
    }
    $field = Find-Element 'Parallel SSH logins' $combo
    $popup = $field.GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
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

$launch = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $origin = $window
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    Run-Palette "Review SSH group${dot}Limited" -Enter
    Set-LoginLimit
    Click-Button 'Open reviewed group in new window'
    $window = Wait-Until 'the group window' {
        [FixtureDialog]::Windows($app.Id) | Where-Object {
            $_ -ne $origin -and [FixtureDialog]::GetWindow($_, 4) -eq [IntPtr]::Zero
        } | Select-Object -First 1
    }
    $limits = Wait-Until 'one login awaits a password and the other remains queued' {
        $logins = @((Control @('list')).sessions | Where-Object label -like 'Login *')
        $queued = $logins | Where-Object { $_.ssh_connection.phase -eq 'queued' }
        $auth = $logins | Where-Object {
            $_.ssh_connection.phase -eq 'connecting' -and
                (Find-Element "Pane $($_.id)${dot}Authentication required")
        }
        if ($logins.Count -eq 2 -and $queued -and $auth -and
            (Find-Element "Pane $($queued.id)${dot}Waiting to connect")) {
            @{ queued = $queued; auth = $auth }
        }
    }
    Stop-HostLogin $limits.auth $limits.queued
    $field = Wait-Until 'the roster filter' { Find-Element 'Host filter' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('')
    $null = Wait-Until 'the queued login receives the released slot' {
        (Control @('events','--pane',"$($limits.queued.id)")).ssh_connection.phase -eq 'connecting' -and
            (Find-Element "Pane $($limits.queued.id)${dot}Authentication required")
    }
    Stop-HostLogin $limits.queued $limits.auth
    $ready = (Control @('list')).sessions | Where-Object label -ceq 'Ready'
    $null = Control @('wait-for-text','--pane',"$($ready.id)",'--timeout','15','ready')
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
