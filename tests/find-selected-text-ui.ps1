[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_TEST_COPY_FILE = Join-Path $State 'clipboard-write'
$profiles = Join-Path $State 'profiles.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"find":"F6","command_palette":"F7","copy_mode":"F9","focus_left":"F4","focus_right":"F10"}}')
$textPattern = [System.Windows.Automation.TextPattern]::Pattern
$valuePattern = [System.Windows.Automation.ValuePattern]::Pattern
$togglePattern = [System.Windows.Automation.TogglePattern]::Pattern
$edit = [System.Windows.Automation.ControlType]::Edit
$check = [System.Windows.Automation.ControlType]::CheckBox
$statusId = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'find-status')
$needle = 'Issue.[' + [char]0x96ea + ']+/' + ('n' * 100) + '/end!'
$prior = 'prev[a-z]+ token'

function Field { Find-Element 'Find in scrollback' $edit }
function Provider {
    $e = Find-Element ('Terminal ' + [char]0xb7 + ' palette-ssh') ([System.Windows.Automation.ControlType]::Document)
    if ($e) { $e.GetCurrentPattern($textPattern) }
}
function Toggle([string]$Name) {
    (Find-Element $Name $check).GetCurrentPattern($togglePattern)
}
function Find-Status {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,$statusId).Current.Name
}
function Show-Find {
    Press-Key 0x75 # Remapped Find (F6), including in keyboard copy mode.
    $null = Wait-Until 'Find edit field' { Field }
}
function Hide-Find {
    Press-Key 27
    $null = Wait-Until 'dismissed Find field' { !(Field) }
    Wait-Focus $remote
}
function Check-Find([string]$Query, [bool]$Regex, [int]$Matches) {
    $observed = @{}
    try {
        $null = Wait-Until "exact query '$Query', regex $Regex, match-case on, $Matches matches" {
            $field = Field
            if (!$field) { return $false }
            $observed.query = $field.GetCurrentPattern($valuePattern).Current.Value
            $observed.regex = (Toggle 'Regular expression').Current.ToggleState -eq 'On'
            $observed.case = (Toggle 'Match case').Current.ToggleState -eq 'On'
            $observed.status = Find-Status
            $observed.query -ceq $Query -and $observed.regex -eq $Regex -and $observed.case -and
                $observed.status -match "^[1-9][0-9]* / $Matches$"
        }
    } catch { throw "$($_.Exception.Message). Actual: $($observed | ConvertTo-Json -Compress)" }
}
function Selection-Text {
    $provider = Provider
    if ($provider) { @($provider.GetSelection() | ForEach-Object { $_.GetText(-1) }) -join '' }
}
function Select-Span([string]$Start, [string]$End = '', [switch]$Wrapped) {
    $first = Wait-Until "accessible text range '$Start'" {
        $provider = Provider
        if (!$provider) { return }
        $doc = $provider.DocumentRange
        $range = $doc.FindText($Start, $false, $false)
        if (!$range) { return }
        if ($End) {
            $last = $doc.FindText($End, $false, $false)
            if (!$last) { return }
            $endpoint = [System.Windows.Automation.Text.TextPatternRangeEndpoint]::End
            $range.MoveEndpointByRange($endpoint,$last,$endpoint)
        }
        $range
    }
    if ($Wrapped -and $first.GetBoundingRectangles().Count -lt 2) { throw 'The intended soft-wrapped range fits on one screen row' }
    $expected = $first.GetText(-1)
    if ($End -and !$expected.Contains("`r`n")) { throw 'The intended multi-line range has no hard line break' }
    $first.Select()
    $null = Wait-Until 'visible selection matches the requested UIA text range' { (Selection-Text) -ceq $expected }
}
function Shell-Pid([string]$Tag) {
    $line = "'$Tag=' + " + '$PID'
    foreach ($c in $line.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $pattern = "$Tag=([0-9]+)"
    $frame = Control @('wait-for-text','--pane',"$localPane",'--regex','--timeout','15',$pattern)
    return [regex]::Match($frame.text,$pattern).Groups[1].Value
}
function Assert-ClipboardUntouched {
    if (Test-Path -LiteralPath $env:W11TERM_TEST_COPY_FILE) { throw 'Finding selected terminal text wrote to the clipboard' }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','100','--rows','32') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $remote = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','select-find-ready')
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -ceq 'Palette PowerShell').id }
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','20','PS ')
    $pidBefore = Shell-Pid 'find-selection-before'
    Press-Key 0x73 # F4 focuses the SSH half.
    Wait-Focus $remote

    Show-Find
    (Field).GetCurrentPattern($valuePattern).SetValue($prior)
    (Toggle 'Regular expression').Toggle()
    (Toggle 'Match case').Toggle()
    Check-Find $prior $true 2
    Hide-Find
    Show-Find # No selection: restore the query and both options.
    Check-Find $prior $true 2
    Hide-Find
    Select-Span 'hard-line-first' 'hard-line-second'
    Show-Find # A range containing a hard break is ineligible.
    Check-Find $prior $true 2
    Hide-Find
    Select-Span $needle -Wrapped
    Press-Key 0x72 # F3 opens the prior search even with an eligible selection.
    Check-Find $prior $true 2
    $next = if ((Find-Status) -ceq '1 / 2') { '2 / 2' } else { '1 / 2' }
    Press-Key 0x72
    $null = Wait-Until "F3 navigated the prior regex to $next" { (Find-Status) -ceq $next }
    Hide-Find
    $null = Wait-Until 'SSH text provider restored after navigation' { Provider }
    # A viewport change can dismiss a visual selection. Recreate exactly the
    # same UIA range if needed before exercising an explicit Open Find action.
    if ((Selection-Text) -cne $needle) { Select-Span $needle -Wrapped }
    Show-Find
    Check-Find $needle $false 2 # Two exact hits; one case variant and one regex-only decoy.
    (Field).GetCurrentPattern($valuePattern).SetValue('hard-line-first')
    Check-Find 'hard-line-first' $false 1
    $caseBox = Find-Element 'Match case' $check
    $caseBox.SetFocus()
    $null = Wait-Until 'focus moved to the Find options' { $caseBox.Current.HasKeyboardFocus }
    Press-Key 0x75 # Already open: focus the edit while preserving the user's new query.
    $null = Wait-Until 'F6 returned focus to the edited Find field' { (Field).Current.HasKeyboardFocus }
    Check-Find 'hard-line-first' $false 1
    Assert-ClipboardUntouched
    Hide-Find

    Press-Key 0x78 # F9 enters keyboard copy mode.
    $null = Wait-Until 'keyboard copy mode' { Copy-ModeOpen }
    foreach ($key in 0x47,0x24,0x56) { Press-Key $key } # First retained row, Home, start selection.
    foreach ($i in 1..6) { Press-Key 0x27 }
    $null = Wait-Until 'KEY[42] selected in keyboard copy mode' { (Selection-Text) -ceq 'KEY[42]' }
    Show-Find
    Check-Find 'KEY[42]' $false 1
    if (Copy-ModeOpen) { throw 'Find and keyboard copy mode both remained open' }
    Assert-ClipboardUntouched
    Hide-Find
    foreach ($c in 'selected-find-shell-ok'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','selected-find-shell-ok')
    Press-Key 0x79 # F10 focuses the same PowerShell split.
    Wait-Focus $localPane
    if ((Shell-Pid 'find-selection-after') -cne $pidBefore) { throw 'Find replaced the PowerShell sibling' }
    Assert-ClipboardUntouched
} catch {
    throw "$($_.Exception.Message) at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
