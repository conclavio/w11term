param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$env:W11TERM_OPENROUTER_FIXTURE_PORT = [IO.File]::ReadAllText((Join-Path $State 'openrouter-port'))
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$preview = 'OpenRouter request preview'
$draft = 'Draft a Bash command with OpenRouter'
$providerKey = 'sk-or-v1-w11term-fixture'
$explain = 'Explain visible pane with OpenRouter'

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

function Review-VisibleSshPane {
    $local = (Sessions | Where-Object label -eq 'Palette PowerShell').id
    $null = Control @('focus-pane','--pane',"$local")
    Wait-Focus $local
    Type-Line "'neighbor-private-' + '42'"
    Wait-Text $local 'neighbor-private-42'
    Run-Palette 'Split right with palette-ssh'
    $ssh = Wait-Until 'SSH split becomes active' {
        (Sessions | Where-Object { $_.label -eq 'palette-ssh' -and $_.active }).id
    }
    Wait-Text $ssh 'ssh-live-tail'
    if ((Control @('get-text','--pane',"$ssh")).text.Contains('ssh-visible-issue')) {
        throw 'The SSH fixture issue is still on the live screen'
    }
    $document = Wait-Until 'accessible SSH pane' {
        Find-Element ('Terminal ' + [char]0xb7 + ' palette-ssh') ([System.Windows.Automation.ControlType]::Document)
    }
    $scroll = $document.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
    $null = Wait-Until 'SSH pane exposes retained history' { $scroll.Current.VerticallyScrollable }
    $scroll.SetScrollPercent(-1, 50)
    $range = $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
    $null = Wait-Until 'scrolling to the middle shows the earlier SSH text' {
        $range.GetVisibleRanges()[0].GetText(-1).Contains('ssh-visible-issue')
    }
    Run-Palette $explain -Enter
    $seed = Wait-Until 'snapshot of the scrolled SSH pane is available to edit' {
        $field = Find-Element $preview $edit
        if ($field -and $field.Current.IsEnabled) {
            $text = $field.GetCurrentPattern($value).Current.Value
            if ($text.Contains('ssh-visible-issue redact-this-fixture')) { $text }
        }
    }
    foreach ($excluded in @('ssh-offscreen-archive','ssh-live-tail','neighbor-private-42')) {
        if ($seed.Contains($excluded)) { throw "The preview included '$excluded' from outside the focused viewport" }
    }
    Confirm-NoRequest 4
    $edited = $seed.Replace("`r`n", "`r").Replace("`n", "`r").Replace('redact-this-fixture', '[redacted]')
    Set-Field $preview $edited
    Click 'Review OpenRouter request'
    $null = Wait-Until 'the edited viewport is ready for Send' { Find-Element 'Send reviewed request' $button }
    Confirm-NoRequest 4
    [IO.File]::WriteAllText((Join-Path $State 'approved-viewport'), $edited.Replace("`r", "`n"))
    Click 'Send reviewed request'
    $null = Wait-Until 'reviewed viewport response' { Find-Element 'explained the reviewed viewport' }
}

function Review-SyscallComparison {
    $working = Join-Path $State 'private working trace.strace'
    $failing = Join-Path $State 'private failing trace.strace'
    [IO.File]::WriteAllText($working, @'
21 1700000000.000000 execve("/bin/diag", ["diag"], NULL) = 0
21 1700000000.001000 openat(AT_FDCWD, "/tmp/unmatched-private", O_RDONLY) = 3
21 1700000000.002000 openat(AT_FDCWD, "/tmp/review-data", O_RDONLY) = 4
21 1700000000.003000 read(4</tmp/review-data>, "raw-sensitive-buffer", 20) = 20
21 1700000000.004000 read(4</tmp/review-data>, "extra-buffer", 12) = 12
'@ + "`n")
    [IO.File]::WriteAllText($failing, @'
77 1800000000.000000 execve("/bin/diag", ["diag"], NULL) = 0
77 1800000000.001000 openat(AT_FDCWD, "/tmp/unmatched-private", O_RDONLY) = -1 ENOENT (No such file or directory)
77 1800000000.002000 openat(AT_FDCWD, "/tmp/review-data", O_RDONLY) = 8
77 1800000000.003000 read(8</tmp/review-data>, "raw-sensitive-buffer", 20) = 20
'@ + "`n")
    $windowId = ((Control @('list-windows')).windows | Select-Object -First 1).window_id
    $null = Control @('open-diagnostic','--window',"$windowId",'--kind','syscall-trace','--file',$working,'--compare-with',$failing)
    Set-Field 'Filter syscall changes' 'read /tmp/review-data'
    Click 'Apply change filter'
    $dot = ' ' + [char]0xb7 + ' '
    $null = Wait-Until 'the applied filter selects the count change' {
        (Find-Element "1 matching change row${dot}Page 1 of 1") -and (Find-Element 'Evidence 2 of 2')
    }
    Click 'Previous comparison evidence'
    $null = Wait-Until 'the first pair is selected' { Find-Element 'Evidence 1 of 2' }
    Set-Field 'Filter syscall changes' 'unapplied-private-filter'
    Click ('Review comparison with OpenRouter' + [char]0x2026)
    $seed = Wait-Until 'the syscall review draft reflects the selected evidence' {
        $field = Find-Element $preview $edit
        if ($field -and $field.Current.IsEnabled) {
            $text = $field.GetCurrentPattern($value).Current.Value
            if ($text.Contains('Syscall trace comparison') -and
                $text.Contains('1 of 2 change rows match the applied filter') -and
                $text.Contains('Evidence pair 1 of 2') -and
                $text.Contains('Working') -and $text.Contains('Failing') -and
                $text.Contains('Source 1 line 4')) { $text }
        }
    }
    foreach ($excluded in @('raw-sensitive-buffer','extra-buffer','unmatched-private',
            'unapplied-private-filter','private working trace.strace','private failing trace.strace')) {
        if ($seed.Contains($excluded)) { throw "Syscall review unexpectedly included '$excluded'" }
    }
    Confirm-NoRequest 5
    $edited = $seed.Replace("`r`n", "`r").Replace("`n", "`r").Replace('/tmp/review-data', '[resource hidden]')
    if ($edited -ceq $seed) { throw 'The syscall preview omitted its normalized resource before redaction' }
    Set-Field $preview $edited
    Click 'Review OpenRouter request'
    $null = Wait-Until 'the revised syscall evidence is ready to send' { Find-Element 'Send reviewed request' $button }
    Confirm-NoRequest 5
    [IO.File]::WriteAllText((Join-Path $State 'approved-syscall'), $edited.Replace("`r", "`n"))
    Click 'Send reviewed request'
    $null = Wait-Until 'the reviewed syscall response is displayed' { Find-Element 'reviewed the syscall evidence' }
    Click 'Back to syscall viewer'
    $null = Wait-Until 'return restores the selected trace evidence' {
        (Find-Element 'Evidence 1 of 2') -and (Find-Element "1 matching change row${dot}Page 1 of 1")
    }
    Click ('Review comparison with OpenRouter' + [char]0x2026)
    $null = Wait-Until 'the source can start another draft' { Find-Element 'Review syscall comparison evidence' }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', '"Palette PowerShell"', '--cols', '120', '--rows', '24') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
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
        Test-Path -LiteralPath (Join-Path $State 'openrouter-late-sent')
    }
    if (Find-Element 'late fixture reply') { throw 'Cancellation allowed a late response' }
    Review-VisibleSshPane
    Review-SyscallComparison
    Click 'Remove OpenRouter key'
    $null = Wait-Until 'saved key is removed' { Find-Element 'Saved OpenRouter API key removed' }
    if ((Find-Element 'late fixture reply') -or (Test-Path -LiteralPath $credential)) {
        throw 'Cancellation allowed a late response or the key was not removed'
    }
} finally {
    Close-FixtureApp $app
}
