[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$jumps = Get-Content -Raw -LiteralPath (Join-Path $State 'fixture-jumps.json') | ConvertFrom-Json
$outer, $inner = $jumps
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
$separator = [char]0xb7

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
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
function Select-Keyless {
    $combo = [System.Windows.Automation.ControlType]::ComboBox
    $listItem = [System.Windows.Automation.ControlType]::ListItem
    $selection = [System.Windows.Automation.SelectionPattern]::Pattern
    $field = Wait-Until 'jump authentication selector' { Find-Element 'Jump host authentication' $combo }
    $field.SetFocus()
    $null = Wait-Until 'jump authentication scrolled into view' {
        Find-Element 'Jump host authentication' $combo -Visible
    }
    $popup = $field.GetCurrentPattern($expand)
    $popup.Expand()
    $choice = Wait-Until 'keyless jump authentication option' { Find-Element 'None (keyless server)' $listItem -Visible }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'keyless authentication selected for the draft hop' {
        $current = (Find-Element 'Jump host authentication' $combo).GetCurrentPattern($selection)
        $selected = $current.Current.GetSelection()
        $selected.Count -eq 1 -and $selected[0].Current.Name -ceq 'None (keyless server)'
    }
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}
function Wait-EditorClosed {
    $null = Wait-Until 'jump editor closed' { !(Find-Element 'Jump host address' $edit) }
}
function Add-Jump($Peer, [switch]$CheckInvalidPort) {
    Click 'Add jump host'
    Set-Field 'Jump host address' $Peer.host
    Set-Field 'Jump host user' $Peer.user
    Set-Field 'Jump host fingerprint' $Peer.host_key
    Select-Keyless
    if ($CheckInvalidPort) {
        Set-Field 'Jump host port' '0'
        Click 'Apply jump host'
        $null = Wait-Until 'zero jump port rejected' { Find-Element 'Port must be between 1 and 65535' }
        if ((Find-Element 'Save profile changes' $button).Current.IsEnabled) {
            throw 'Profile can be saved while a jump edit is pending'
        }
    }
    Set-Field 'Jump host port' $Peer.port
    Click 'Apply jump host'
    Wait-EditorClosed
}
function Jump-Action([int]$Index, [string]$Name) {
    Invoke-Button (Get-ScrollableMenuAction "Jump host $Index actions" $Name -VisibleItem)
}
function Wait-Jump([int]$Index, $Peer, [string]$Alias) {
    $trust = if ($Alias) { ", host key alias $Alias" } else { '' }
    $label = "Jump host $Index $separator $($Peer.user)@$($Peer.host):$($Peer.port) (none$trust)"
    try {
        $null = Wait-Until "'$label' in the route" { Find-Element $label }
    } catch {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $observed = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition) |
            ForEach-Object { $_.Current.Name } | Where-Object { $_ -like "Jump host * $separator *" }
        throw "$_ Route rows: $($observed -join ' | ')"
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control manifest' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $original = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$original",'--timeout','15','ready')
    Run-Palette 'Settings'
    Click 'Edit profile palette-ssh'
    Set-NativeExpansion 'SSH jump hosts' $true

    Add-Jump $inner -CheckInvalidPort
    Add-Jump $outer
    Wait-Jump 1 $inner
    Wait-Jump 2 $outer
    Jump-Action 2 'Move earlier'
    Wait-Jump 1 $outer
    Wait-Jump 2 $inner

    Jump-Action 1 'Edit jump host'
    Set-Field 'Jump host key alias' 'OUTER.BASTION.TEST'
    Set-Field 'Jump host connection timeout (seconds)' '9'
    Set-Field 'Jump host keepalive interval (seconds)' '11'
    Set-Field 'Jump host unanswered keepalives allowed' '4'
    Click 'Apply jump host'
    Wait-EditorClosed
    Wait-Jump 1 $outer 'outer.bastion.test'
    Jump-Action 1 'Edit jump host'
    Set-Field 'Jump host user' 'discarded'
    Click 'Cancel jump host edit'
    Wait-EditorClosed
    Wait-Jump 1 $outer 'outer.bastion.test'

    Add-Jump $inner
    Wait-Jump 3 $inner
    Jump-Action 3 'Remove jump host'
    $null = Wait-Until 'third jump removed' { !(Find-Element 'Jump host 3 actions' $button) }

    if ((Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'palette-ssh'.proxy_jump) {
        throw 'Editing the route changed the profile before saving'
    }
    Set-NativeExpansion 'SSH jump hosts' $false
    Click 'Save and connect'
    $pane = Wait-Until 'new pane from the edited profile' {
        ((Control @('list')).sessions | Where-Object { $_.label -ceq 'palette-ssh' -and $_.id -ne $original }).id
    }
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    $null = Control @('send-text','--pane',"$pane",'--no-paste','routed-shell-probe')
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','routed-shell-probe')
    $verified = (Control @('inspect','--pane',"$pane")).ssh.jump_hosts
    if ($verified.Count -ne 2 -or $verified[0].key_fingerprint -cne $outer.host_key -or
        $verified[1].key_fingerprint -cne $inner.host_key -or
        $verified[0].host_key_alias -cne 'outer.bastion.test') {
        throw "Wrong verified route: $($verified | ConvertTo-Json -Compress)"
    }
    $saved = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'palette-ssh'.proxy_jump
    if ($saved.Count -ne 2 -or $saved[0].user -cne $outer.user -or $saved[0].port -ne $outer.port -or
        $saved[1].user -cne $inner.user -or $saved[1].port -ne $inner.port -or
        $saved[0].connect_timeout_seconds -ne 9 -or $saved[0].keepalive.interval_seconds -ne 11 -or
        $saved[0].keepalive.max_unanswered -ne 4 -or $saved[0].auth -cne 'none' -or
        $saved[1].auth -cne 'none') {
        throw "Wrong saved route: $($saved | ConvertTo-Json -Compress)"
    }
    if ((Control @('inspect','--pane',"$original")).ssh.jump_hosts.Count -ne 0) {
        throw 'Saving the profile changed an existing direct login'
    }
} finally {
    Close-FixtureApp $app
}
