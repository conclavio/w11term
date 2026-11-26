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

function Choose-Item([string]$Name, [string]$Choice) {
    $selector = Wait-Until "'$Name' selector" { Find-Element $Name $combo }
    $selector.SetFocus()
    $selector = Wait-Until "'$Name' scrolled into view" { Find-Element $Name $combo -Visible }
    $popup = $selector.GetCurrentPattern($expand)
    $popup.Expand()
    $choiceItem = Wait-Until "'$Choice' option" { Find-Element $Choice $item -Visible }
    $choiceItem.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}

function Wait-AuthFailure([string]$Profile, [string]$Reason) {
    $pane = (Control @('open','--profile',$Profile)).pane
    $null = Wait-Until "'$Profile' rejects the configured key" {
        $state = (Control @('events','--pane',"$pane")).ssh_connection
        $state.phase -eq 'failed' -and $state.reason -match $Reason
    }
}

function Set-AgentPipe([string]$Pipe) {
    $field = Wait-Until 'SSH agent pipe field' { Find-Element 'SSH agent pipe (optional)' $edit }
    $field.GetCurrentPattern($value).SetValue($Pipe)
    $null = Wait-Until 'agent pipe updated' {
        (Find-Element 'SSH agent pipe (optional)' $edit).GetCurrentPattern($value).Current.Value -ceq $Pipe
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--demo', '--profiles', ('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control manifest' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    Run-Palette 'Settings'
    Click 'Edit profile Agent profile'
    Choose-Item 'Authentication' 'Specific SSH agent key'
    Click 'Load agent keys'
    Choose-Item 'Keys in the SSH agent' ("Inherited agent key " + [char]0xb7 + " $($fixture.inherited_fingerprint)")
    Set-AgentPipe $fixture.selected_pipe
    $null = Wait-Until 'previous inventory is hidden when the pipe changes' {
        !(Find-Element 'Keys in the SSH agent' $combo)
    }
    Click 'Load agent keys'
    Choose-Item 'Keys in the SSH agent' ("Selected agent key " + [char]0xb7 + " $($fixture.wanted)")
    $null = Wait-Until 'the selected agent fingerprint is in the draft' {
        $field = Find-Element 'SSH agent key fingerprint' $edit
        $field -and $field.GetCurrentPattern($value).Current.Value -ceq $fixture.wanted
    }
    Click 'Save and connect'
    $null = Wait-Until 'the profile persists the selected key' {
        $entry = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'Agent profile'
        $entry.auth -eq 'agent-key' -and $entry.agent_fingerprint -ceq $fixture.wanted -and
            $entry.agent_pipe -ceq $fixture.selected_pipe
    }
    $pane = Wait-Until 'the saved SSH pane' {
        ((Control @('list')).sessions | Where-Object label -eq 'Agent profile').id
    }
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    Wait-AuthFailure 'Missing agent key' 'does not hold the selected key'
    Wait-AuthFailure 'Rejected agent key' 'rejected the selected SSH agent key'
    Run-Palette 'Settings'
    Click 'Edit profile Agent profile'
    Set-AgentPipe ''
    Click 'Save profile changes'
    $null = Wait-Until 'clearing the field removes the saved pipe override' {
        $entry = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'Agent profile'
        !$entry.agent_pipe -and $entry.agent_fingerprint -ceq $fixture.wanted
    }
    Wait-AuthFailure 'Agent profile' 'does not hold the selected key'
} finally {
    Close-FixtureApp $app
}
