param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$env:W11TERM_OPENROUTER_FIXTURE_PORT = [IO.File]::ReadAllText((Join-Path $State 'openrouter-port'))
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$preview = 'OpenRouter request preview'
$draft = 'Draft a Bash command with OpenRouter'
$providerKey = 'sk-or-v1-w11term-fixture'
$profilesJson = '{"profiles":{"PowerShell":{"backend":"local","executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}}}'
[IO.File]::WriteAllText($profiles, $profilesJson)

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name' button" {
        $element = Find-Element $Name $button
        if ($element -and $element.Current.IsEnabled) { $element }
    })
}

function Set-Field([string]$Name, [string]$Text) {
    $field = Wait-Until "enabled '$Name' field" {
        $element = Find-Element $Name $edit
        if ($element -and $element.Current.IsEnabled) { $element }
    }
    $field.GetCurrentPattern($value).SetValue($Text)
    $null = Wait-Until "'$Name' contains edited text" {
        $element = Find-Element $Name $edit
        $element -and $element.GetCurrentPattern($value).Current.Value -ceq $Text
    }
}

function Set-MaskedKey {
    $field = Wait-Until 'masked OpenRouter key field' {
        $element = Find-Element 'OpenRouter API key' $edit
        if ($element -and $element.Current.IsPassword -and $element.Current.IsEnabled) { $element }
    }
    $field.SetFocus()
    $target = Wait-Until 'focused key field and its native input window' {
        $focused = [FixtureMessages]::FocusedWindow($window, $app.Id)
        if ($field.Current.HasKeyboardFocus -and $focused -ne [IntPtr]::Zero) { $focused }
    }
    foreach ($character in $providerKey.ToCharArray()) {
        if (![FixtureMessages]::PostKey($target, 0x102, [uint32]$character)) {
            throw 'Could not enter the fixture key'
        }
    }
}

function Confirm-NoRequest([int]$Number) {
    if (Test-Path -LiteralPath (Join-Path $State "openrouter-request-$Number")) {
        throw "OpenRouter request $Number was sent before explicit approval"
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'PowerShell') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    Run-Palette $draft
    Set-MaskedKey
    Click 'Save OpenRouter key'
    $null = Wait-Until 'Windows-protected OpenRouter key was saved' {
        Find-Element 'OpenRouter API key saved'
    }
    $credential = Join-Path $env:W11TERM_DATA_DIR 'openrouter-key.dpapi'
    if ([Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($credential)).Contains($providerKey)) {
        throw 'The OpenRouter key was written in plaintext'
    }
    Confirm-NoRequest 0

    Click 'Refresh model catalog'
    $null = Wait-Until 'fixture model catalog loaded' { Find-Element 'Loaded 1 OpenRouter models' }
    Set-Field 'OpenRouter model ID' 'fixture/model'
    Click 'Save OpenRouter settings'
    $null = Wait-Until 'the selected model is saved' { Find-Element 'OpenRouter settings saved' }
    Set-Field $preview 'unapproved draft'
    Click 'Review OpenRouter request'
    $null = Wait-Until 'review shows the exact destination and model' {
        (Find-Element "Destination http://127.0.0.1:$($env:W11TERM_OPENROUTER_FIXTURE_PORT)/api/v1/chat/completions") -and
            (Find-Element 'Model fixture/model')
    }
    Confirm-NoRequest 1
    Set-Field $preview "approved edit`rwith a second line"
    $null = Wait-Until 'editing invalidates the active Send action' { !(Find-Element 'Send reviewed request' $button) }
    Confirm-NoRequest 1

    Click 'Review OpenRouter request'
    Click 'Send reviewed request'
    $reply = "Set-Content -LiteralPath '" + (Join-Path $State 'executed-provider-reply') + "' -Value ran"
    $dot = ' ' + [char]0xb7 + ' '
    $null = Wait-Until 'shell-looking response and token usage are displayed' {
        (Find-Element $reply) -and (Find-Element "Tokens 17 input${dot}7 output${dot}24 total")
    }
    if (Test-Path -LiteralPath (Join-Path $State 'executed-provider-reply')) {
        throw 'The provider response was executed in PowerShell'
    }
    Set-Field $preview 'try provider rejection'
    Click 'Review OpenRouter request'
    Click 'Send reviewed request'
    $null = Wait-Until 'provider refusal reports the privacy policy' {
        Find-Element 'OpenRouter could not route the selected model under the requested privacy policy (HTTP 404)'
    }
    if (Find-Element 'private provider diagnostics') { throw 'The raw provider error was displayed' }

    Set-Field $preview 'cancel after receipt'
    Click 'Review OpenRouter request'
    Click 'Send reviewed request'
    $null = Wait-Until 'provider receives the final request' { Test-Path -LiteralPath (Join-Path $State 'openrouter-request-3') }
    Click 'Stop waiting'
    $null = Wait-Until 'cancelled state is displayed' { Find-Element 'Stopped waiting for OpenRouter.' }
    [IO.File]::WriteAllText((Join-Path $State 'openrouter-cancel-seen'), '')
    $null = Wait-Until 'provider attempted to deliver the late response' {
        Test-Path -LiteralPath (Join-Path $State 'openrouter-fixture-done')
    }
    Click 'Remove OpenRouter key'
    $null = Wait-Until 'saved key is removed' { Find-Element 'Saved OpenRouter API key removed' }
    if ((Find-Element 'late fixture reply') -or (Test-Path -LiteralPath $credential)) {
        throw 'Cancellation allowed a late response or the key was not removed'
    }
} finally {
    Close-FixtureApp $app
}
