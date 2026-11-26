[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$fixture = Get-Content -Raw -LiteralPath (Join-Path $State 'agent-fixture.json') | ConvertFrom-Json
$env:SSH_AUTH_SOCK = $fixture.pipe
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$combo = [System.Windows.Automation.ControlType]::ComboBox
$edit = [System.Windows.Automation.ControlType]::Edit
$item = [System.Windows.Automation.ControlType]::ListItem
$expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
$value = [System.Windows.Automation.ValuePattern]::Pattern

function Click([string]$Name) {
    Invoke-Button (Wait-Until "'$Name' enabled" {
        $element = Find-Element $Name $button
        if ($element -and $element.Current.IsEnabled) { $element }
    })
}

function Wait-AuthFailure([string]$Profile, [string]$Reason) {
    $pane = (Control @('open','--profile',$Profile)).pane
    $null = Wait-Until "'$Profile' failed authentication" {
        $state = (Control @('events','--pane',"$pane")).ssh_connection
        $state.phase -eq 'failed' -and $state.reason -match $Reason
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--demo', '--profiles', ('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control manifest' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    Run-Palette 'Settings'
    Click 'Edit profile Agent certificate route'
    $selector = Wait-Until 'authentication selector' { Find-Element 'Authentication' $combo }
    $selector.SetFocus()
    $selector = Wait-Until 'authentication scrolled into view' { Find-Element 'Authentication' $combo -Visible }
    $popup = $selector.GetCurrentPattern($expand)
    $popup.Expand()
    $choice = Wait-Until 'SSH agent with user certificate option' {
        Find-Element 'SSH agent with user certificate' $item -Visible
    }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
    $field = Wait-Until 'agent certificate file editor' { Find-Element 'Agent user certificate file' $edit }
    $field.GetCurrentPattern($value).SetValue($fixture.certificate)
    $null = Wait-Until 'certificate path committed to the draft' {
        (Find-Element 'Agent user certificate file' $edit).GetCurrentPattern($value).Current.Value -ceq $fixture.certificate
    }
    Click 'Save and connect'
    $null = Wait-Until 'destination certificate and jump fingerprint saved' {
        $entry = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'Agent certificate route'
        $entry.auth -ceq 'agent-certificate' -and $entry.certificate -ceq $fixture.certificate -and
            !$entry.agent_fingerprint -and $entry.proxy_jump[0].agent_fingerprint -ceq $fixture.gateway_pin -and
            $entry.agent_pipe -ceq $fixture.destination_pipe -and $entry.proxy_jump[0].agent_pipe -ceq $fixture.gateway_pipe
    }
    $pane = Wait-Until 'authenticated route pane' {
        ((Control @('list')).sessions | Where-Object label -eq 'Agent certificate route').id
    }
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    $route = (Control @('inspect','--pane',"$pane")).ssh
    $destination = $route.destination.authentication
    $gateway = $route.jump_hosts[0].authentication
    if ($destination.method -cne 'public-key' -or $destination.source -cne 'agent' -or
        $destination.key_fingerprint -cne $fixture.destination_pin -or $destination.certificate_authority_fingerprint -cne $fixture.destination_ca -or
        $gateway.method -cne 'public-key' -or $gateway.source -cne 'agent' -or
        $gateway.key_fingerprint -cne $fixture.gateway_pin -or $gateway.certificate_authority_fingerprint -cne $fixture.gateway_ca) {
        throw 'The authenticated route did not report the accepted user certificate at each endpoint'
    }
    Invoke-NativeMenuAction 'Tab menu' 'Terminal inspector: Off'
    Set-NativeExpansion 'Verified SSH route' $true
    $null = Wait-Until 'inspector shows the two user certificate fingerprints' {
        (Find-Element 'Authentication   User certificate (SSH agent)') -and
            (Find-Element "User key   $($fixture.destination_pin)") -and
            (Find-Element "User key   $($fixture.gateway_pin)") -and
            (Find-Element "User certificate CA key   $($fixture.destination_ca)") -and
            (Find-Element "User certificate CA key   $($fixture.gateway_ca)")
    }
    Wait-AuthFailure 'Mismatched certificate pin' 'does not match the selected agent key'
    Wait-AuthFailure 'Missing certificate key' 'does not hold the key for the SSH user certificate'
    Wait-AuthFailure 'Rejected certificate' 'rejected the SSH user certificate'
} finally {
    Close-FixtureApp $app
}
