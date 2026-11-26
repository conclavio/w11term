[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

function Wait-KeyPrompt([int]$Pane, [bool]$Retry) {
    Wait-Until "local key prompt on pane $Pane (retry=$Retry)" {
        $row = (Control @('list')).sessions | Where-Object id -eq $Pane
        $question = $row.prompt.question
        if ($question.kind -eq 'identity-passphrase' -and $question.retry -eq $Retry -and
            $question.identity -like '*fixture-key.ed25519') {
            $row.prompt.id
        }
    }
}

function Submit-KeyPassphrase([string]$Value, [switch]$Remember) {
    $label = 'Keep this key unlocked for reconnects'
    $checkbox = Wait-CheckBox $label Off
    if ($Remember) {
        $checkbox.Toggle()
        $null = Wait-CheckBox $label On
    }

    $field = Wait-Until 'masked key passphrase field' {
        $element = Find-Element 'Key passphrase' $edit
        if ($element -and $element.Current.IsPassword -and !$element.Current.IsOffscreen) { $element }
    }
    $field.SetFocus()
    $null = Wait-Until 'key passphrase field to receive focus' { $field.Current.HasKeyboardFocus }
    $target = Wait-Until 'WinUI input window for the key prompt' {
        $focused = [FixtureMessages]::FocusedWindow($window, $app.Id)
        if ($focused -ne [IntPtr]::Zero) { $focused }
    }
    foreach ($character in $Value.ToCharArray()) {
        if (![FixtureMessages]::PostKey($target, 0x102, [uint32]$character)) {
            throw 'Could not type into the test dialog'
        }
    }
    Invoke-Button (Wait-Until 'Unlock button' { Find-Element 'Unlock' $button })
}

function Drop-Link([int]$Pane, [uint64]$Generation) {
    $null = Control @('send-text','--pane',"$Pane",'--connection-generation',"$Generation",'--no-paste','--base64','BA==')
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','90','--rows','24') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $first = ((Control @('list')).sessions | Where-Object active).id
    $initial = Wait-KeyPrompt $first $false
    Submit-KeyPassphrase 'wrong-fixture-passphrase'
    $retried = Wait-KeyPrompt $first $true
    if ($initial -eq $retried) { throw 'Unlock retry reused the previous prompt ID' }
    Invoke-Button (Wait-Until 'Cancel unlock button' { Find-Element 'Cancel' $button })
    $null = Wait-Until 'cancelled unlock to settle' {
        $reply = Control @('events','--pane',"$first")
        $reply.ssh_connection.phase -eq 'failed' -and $reply.ssh_connection.reason -match 'unlock cancelled'
    }

    $second = (Control @('open','--profile','palette-ssh','--activate')).pane
    $null = Wait-KeyPrompt $second $false
    Submit-KeyPassphrase 'ui-fixture-passphrase' -Remember
    $generation = (Control @('wait-ssh-connected','--pane',"$second",'--timeout','15')).connection_generation
    $result = Control @('wait-for-text','--pane',"$second",'--timeout','15','ready')
    if ($result.text -match 'fixture-passphrase') { throw 'The key passphrase reached the terminal' }

    Drop-Link $second $generation
    $reconnected = Control @('wait-ssh-connected','--pane',"$second",'--after-generation',"$generation",'--timeout','15')
    if ($reconnected.pane -ne $second -or $reconnected.connection_generation -le $generation) {
        throw 'The same pane did not automatically reconnect with its unlocked key'
    }
    $null = Control @('wait-for-text','--pane',"$second",'--connection-generation',"$($reconnected.connection_generation)",'--timeout','15','ready')

    $independent = (Control @('open','--profile','palette-ssh')).pane
    $null = Wait-KeyPrompt $independent $false
    $null = Control @('close-pane','--pane',"$independent")
    Wait-Focus $second

    [IO.File]::Copy((Join-Path $State 'rotated-key.ed25519'), (Join-Path $State 'fixture-key.ed25519'), $true)
    Drop-Link $second $reconnected.connection_generation
    $rotationPrompt = Wait-KeyPrompt $second $false
    Submit-KeyPassphrase 'ui-rotated-passphrase'
    $latest = Control @('wait-ssh-connected','--pane',"$second",'--after-generation',"$($reconnected.connection_generation)",'--timeout','15')
    $null = Control @('wait-for-text','--pane',"$second",'--connection-generation',"$($latest.connection_generation)",'--timeout','15','ready')
    Drop-Link $second $latest.connection_generation
    $uncached = Wait-KeyPrompt $second $false
    if ($uncached -eq $rotationPrompt) { throw 'The unchecked login reused its previous prompt' }
    Invoke-Button (Wait-Until 'Cancel unlock button' { Find-Element 'Cancel' $button })
} finally {
    Close-FixtureApp $app
}
