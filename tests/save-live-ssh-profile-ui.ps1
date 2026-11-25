param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$menu = [System.Windows.Automation.ControlType]::MenuItem
$edit = [System.Windows.Automation.ControlType]::Edit
$document = [System.Windows.Automation.ControlType]::Document
$value = [System.Windows.Automation.ValuePattern]::Pattern
$saveCommand = 'Save SSH connection as profile' + [char]0x2026
$before = [IO.File]::ReadAllText($profiles)
$peer = ($before | ConvertFrom-Json).profiles.'palette-ssh'
$endpoint = "$($peer.user)@$($peer.host):$($peer.port)"
$name = 'Saved loopback SSH'

function Click([string]$Name, $Type = $button) {
    Invoke-Button (Wait-Until "enabled '$Name' control" {
        $control = Find-Element $Name $Type
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}
function Profile-Name {
    $field = Wait-Until 'profile name editor' { Find-Element 'Profile name' $edit }
    return $field.GetCurrentPattern($value)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('ssh', $endpoint, '--auth', 'none', '--host-key', $peer.host_key, '--profiles', ('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $first = Wait-Until 'one-off SSH pane' { Sessions | Where-Object { $_.active -and $_.label -ceq $endpoint } }
    Wait-Text $first.id 'ready'
    $first = Sessions | Where-Object id -eq $first.id
    Send-Remote $first.id 'original-shell-history'

    Click 'Tab menu'
    Click $saveCommand $menu
    (Profile-Name).SetValue('Discarded name')
    Click 'Cancel'
    $null = Wait-Until 'name dialog closes without saving' { !(Find-Element 'Profile name' $edit) }
    if ([IO.File]::ReadAllText($profiles) -cne $before) { throw 'Cancelling changed the profile catalog' }

    # The dialog disappears before WinUI delivers Closed. Wait for w11term
    # to restore its terminal input and accessibility surface.
    $terminalName = 'Terminal ' + [char]0xb7 + " $endpoint"
    $null = Wait-Until 'input returns to the terminal after cancel' { Find-Element $terminalName $document }
    Run-Palette $saveCommand
    (Profile-Name).SetValue('palette-ssh')
    Click 'Save profile'
    $null = Wait-Until 'name collision displayed' { Find-Element 'Profile palette-ssh already exists' }
    $retry = Profile-Name
    if ($retry.Current.Value -cne 'palette-ssh') { throw 'Profile name was lost when saving failed' }
    if ([IO.File]::ReadAllText($profiles) -cne $before) { throw 'The collision changed the profile catalog' }
    $retry.SetValue($name)
    Click 'Save profile'
    $null = Wait-Until 'name dialog closes after saving' { !(Find-Element 'Profile name' $edit) }
    $null = Wait-Until 'the original pane advertises the new profile' {
        $pane = Sessions | Where-Object id -eq $first.id
        $pane.active -and $pane.label -ceq $name -and $pane.tab_id -eq $first.tab_id -and
            $pane.connection_generation -eq $first.connection_generation
    }
    Wait-Text $first.id 'original-shell-history'
    Send-Remote $first.id 'same-shell-after-save'

    $saved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.$name
    if ($saved.host -cne $peer.host -or $saved.user -cne $peer.user -or $saved.port -ne $peer.port -or
        $saved.auth -cne 'none' -or $saved.host_key -cne $peer.host_key) {
        throw 'The saved profile did not preserve the live SSH settings'
    }
    Click 'Tab menu'
    $null = Wait-Until 'save action disabled for the named pane' {
        $control = Find-Element $saveCommand $menu
        $control -and !$control.Current.IsEnabled
    }
    Click ('Workspaces' + [char]0x2026) $menu
    $workspace = Wait-Until 'workspace name editor' { Find-Element 'Workspace name' $edit }
    $workspace.GetCurrentPattern($value).SetValue('saved-live-profile')
    Click 'Save workspace'
    $null = Wait-Until 'workspace saved' { Find-Element 'Saved saved-live-profile' }
    $catalog = [IO.File]::ReadAllText((Join-Path $env:W11TERM_DATA_DIR 'workspaces.json')) | ConvertFrom-Json
    $reference = $catalog.workspaces.'saved-live-profile'.tabs[0].layout.source
    if ($reference.kind -cne 'saved_profile' -or $reference.value -cne $name) {
        throw 'The active pane was not bound to its profile in the saved workspace'
    }

    $fresh = (Control @('open','--profile',$name,'--activate')).pane
    if ($fresh -eq $first.id) { throw 'The saved profile failed to open a fresh pane' }
    Wait-Focus $fresh
    Wait-Text $fresh 'ready'
    Send-Remote $fresh 'reopened-saved-profile'
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
