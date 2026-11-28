param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$trace = Join-Path $State 'reviewed calls.strace'
$first = Join-Path $State 'split trace.501'
$second = Join-Path $State 'split trace.502'
$clockBefore = Join-Path $State 'clock trace.601'
$clockAfter = Join-Path $State 'clock trace.602'
$working = Join-Path $State 'working diag.strace'
$failing = Join-Path $State 'failing diag.strace'
$openLabel = 'Open syscall trace' + [char]0x2026
$dot = ' ' + [char]0xb7 + ' '
$times = [char]0xd7
$combo = [System.Windows.Automation.ControlType]::ComboBox
$listItem = [System.Windows.Automation.ControlType]::ListItem
$expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
$select = [System.Windows.Automation.SelectionItemPattern]::Pattern
$rawId = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'syscall-original-text')
$summaryId = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'syscall-summary-original-text')
$descriptorId = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'descriptor-original-text')
$workingId = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'comparison-working-original')
$failingId = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'comparison-failing-original')
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$byListItem = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $listItem)
$env:W11TERM_TEST_COPY_FILE = Join-Path $State 'copied-trace-paths.txt'
[IO.File]::WriteAllText($profiles, '{"profiles":{}}')
[IO.File]::WriteAllText($trace, @'
501 1740000000.000000 openat(AT_FDCWD, "/tmp/retry", O_RDONLY) = -1 ENOENT (No such file or directory) <0.000001>
501 1740000000.000100 openat(AT_FDCWD, "/tmp/retry", O_RDONLY) = 3</tmp/retry> <0.000002>
501 1740000000.001000 read(3</tmp/retry>, "a", 1) = 1 <0.000002>
501 1740000000.002000 read(3</tmp/retry>,  <unfinished ...>
502 1740000000.002100 execve("/usr/bin/cat", ["cat"], 0x0) = 0 <0.000005>
501 1740000000.003000 <... read resumed>"b", 1) = 1 <0.000003>
501 1740000000.004000 openat(AT_FDCWD, "/tmp/missing", O_RDONLY) = -1 ENOENT (No such file or directory) <0.000001>
501 1740000000.005000 close(3</tmp/retry>) = 0 <0.000001>
501 1740000000.006000 openat(AT_FDCWD, "/tmp/reused", O_WRONLY) = 3</tmp/reused> <0.000001>
501 1740000000.007000 write(3</tmp/reused>, "x", 1) = 1 <0.000001>
501 1740000000.008000 dup(3</tmp/reused>) = 7</tmp/reused> <0.000001>
501 1740000000.009000 close(7</tmp/reused>) = 0 <0.000001>
501 1740000000.010000 close(3</tmp/reused>) = 0 <0.000001>
'@ + [char]10)
[IO.File]::WriteAllText($first, '1740000010.011000 close(5</tmp/split>) = 0 <0.000002>' + [char]10)
[IO.File]::WriteAllText($second, '1740000010.001000 open("/tmp/split", O_RDONLY) = 5</tmp/split> <0.000003>' + [char]10)
[IO.File]::WriteAllText($clockBefore, @'
23:59:59.980000 open("/tmp/clock-split", O_RDONLY) = 5</tmp/clock-split>
23:59:59.985000 clone(child_stack=NULL, flags=SIGCHLD) = 602
00:00:00.020000 close(5</tmp/clock-split>) = 0
'@ + [char]10)
[IO.File]::WriteAllText($clockAfter, '00:00:00.010000 close(5</tmp/clock-split>) = 0' + [char]10)
[IO.File]::WriteAllText($working, @'
21 1700000000.000000 (+     0.000000) execve("/bin/diag", ["diag"], NULL) = 0 <0.000001>
21 1700000000.001000 (+     0.001000) openat(AT_FDCWD, "/tmp/settings", O_RDONLY) = 3</tmp/settings> <0.000001>
21 1700000000.002000 (+     0.001000) close(3</tmp/settings>) = 0 <0.000001>
21 1700000000.003000 (+     0.001000) openat(AT_FDCWD, "/tmp/data", O_RDONLY) = 3</tmp/data> <0.000001>
21 1700000000.004000 (+     0.001000) read(3</tmp/data>, "a", 1) = 1 <0.000001>
21 1700000000.005000 (+     0.001000) read(3</tmp/data>, "b", 1) = 1 <0.000001>
21 1700000000.006000 (+     0.001000) close(3</tmp/data>) = 0 <0.000001>
'@ + [char]10)
[IO.File]::WriteAllText($failing, @'
77      0.000000 execve("/bin/diag", ["diag"], NULL) = 0 <0.000001>
77      0.001000 openat(AT_FDCWD, "/tmp/settings", O_RDONLY) = -1 ENOENT (No such file or directory) <0.010001>
77      0.002000 openat(AT_FDCWD, "/tmp/data", O_RDONLY) = 8</tmp/data> <0.000001>
77      0.001000 read(8</tmp/data>, "a", 1) = 1 <0.000001>
77      0.002000 close(8</tmp/data>) = 0 <0.004001>
'@ + [char]10)
$before = [Convert]::ToBase64String([IO.File]::ReadAllBytes($trace))

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Open-Trace([string]$Text, [string]$Start = $openLabel) {
    Click $Start
    $dialog = Wait-Until 'owned trace file dialog' { Find-OwnedDialog 'Open syscall trace' }
    $null = Wait-Until 'enter trace source path' { [FixtureDialog]::SetFilename($dialog, $Text, '') }
    $null = Wait-Until 'select trace source' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'trace file dialog closes' { !(Find-OwnedDialog 'Open syscall trace') }
}

function Find-TraceGroup([string]$Pattern) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $byListItem)) {
        if ($item.Current.Name -like $Pattern) { return $item }
    }
}

function Raw-Text($Which = $rawId) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $raw = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $Which)
    if ($raw) { $raw.Current.Name }
}

function Set-ChangeFilter([string]$Text) {
    $field = Wait-Until 'syscall comparison filter field' {
        Find-Element 'Filter syscall changes' $edit
    }
    $field.SetFocus()
    $field = Wait-Until 'syscall comparison filter focused' {
        $current = Find-Element 'Filter syscall changes' $edit -Visible
        if ($current -and $current.Current.HasKeyboardFocus) { $current }
    }
    $field.GetCurrentPattern($value).SetValue($Text)
    $null = Wait-Until 'filter text entered' {
        (Find-Element 'Filter syscall changes' $edit).GetCurrentPattern($value).Current.Value -ceq $Text
    }
}

function Apply-ChangeFilter([string]$Text) {
    Set-ChangeFilter $Text
    Click 'Apply change filter'
}

function Save-Comparison([string]$Destination, [switch]$Json) {
    Click ('Export comparison' + [char]0x2026)
    $dialog = Wait-Until 'owned comparison save dialog' { Find-OwnedDialog 'Save syscall comparison' }
    if ($Json) {
        $null = Wait-Until 'select JSON in Save as type' {
            [FixtureDialog]::SelectFileType($dialog, 'JSON report (*.json)')
        }
    }
    $null = Wait-Until 'enter comparison export path' {
        [FixtureDialog]::SetFilename($dialog, $Destination, 'syscall-comparison.txt') -or
            ($Json -and [FixtureDialog]::SetFilename($dialog, $Destination, 'syscall-comparison.json'))
    }
    $null = Wait-Until 'confirm comparison export' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'comparison save dialog closes' { !(Find-OwnedDialog 'Save syscall comparison') }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    Run-Palette 'Syscall trace viewer'
    Open-Trace $trace
    $readGroup = Wait-Until 'two adjacent reads grouped with aggregate timing' {
        Find-TraceGroup "*PID 501*read*3</tmp/retry>*2$times*0.005 ms in calls*"
    }
    $readGroup.GetCurrentPattern($select).Select()
    $null = Wait-Until 'raw records include both sides of the resumed call' {
        $raw = Raw-Text
        $raw -and $raw.Contains('<unfinished ...>') -and
            $raw.Contains('<... read resumed>') -and $raw.Contains('Line 6')
    }

    Click 'Syscall timing summary'
    $readTiming = Wait-Until 'timing summary counts and ranks the grouped reads' {
        Find-TraceGroup 'read*2 calls*2 timed*total 0.005 ms*max 0.003 ms'
    }
    $readTiming.GetCurrentPattern($select).Select()
    $null = Wait-Until 'slowest read shows both source records' {
        $raw = Raw-Text $summaryId
        $raw -and $raw.Contains('<unfinished ...>') -and $raw.Contains('<... read resumed>') -and
            $raw.Contains('Line 4') -and $raw.Contains('Line 6')
    }
    $sortPicker = Wait-Until 'summary sort selector' { Find-Element 'Syscall summary sort' $combo }
    $sortPopup = $sortPicker.GetCurrentPattern($expand)
    $sortPopup.Expand()
    $sortErrors = Wait-Until 'error count ranking' { Find-Element 'Error count' $listItem -Visible }
    $sortErrors.GetCurrentPattern($select).Select()
    if ($sortPopup.Current.ExpandCollapseState -eq 'Expanded') { $sortPopup.Collapse() }
    $null = Wait-Until 'error ranking selects the call with most errors' {
        $raw = Raw-Text $summaryId
        $raw -and $raw.Contains('openat') -and $raw.Contains('Line 2')
    }
    $summaryPid = Wait-Until 'summary PID filter' { Find-Element 'Summary PID' $edit }
    $summaryPid.SetFocus()
    $summaryPid.GetCurrentPattern($value).SetValue('502')
    $null = Wait-Until 'summary PID text entered' {
        (Find-Element 'Summary PID' $edit).GetCurrentPattern($value).Current.Value -ceq '502'
    }
    Click 'Apply summary PID'
    $null = Wait-Until 'only the requested process is summarized' {
        (Find-TraceGroup 'execve*1 calls*1 timed*') -and !(Find-TraceGroup 'read*2 calls*')
    }
    Click 'Back to syscall groups'
    $null = Wait-Until 'returning to groups restores the selected raw records' {
        $raw = Raw-Text
        $raw -and $raw.Contains('<... read resumed>')
    }

    $picker = Wait-Until 'syscall filter selector' { Find-Element 'Syscall filter' $combo }
    $popup = $picker.GetCurrentPattern($expand)
    $popup.Expand()
    $errorFilter = Wait-Until 'error filter choice' { Find-Element 'Errors and abnormal exits' $listItem -Visible }
    $errorFilter.GetCurrentPattern($select).Select()
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
    Click 'Apply filters'
    $null = Wait-Until 'error filtering shows observed retry and unresolved failure' {
        (Find-Element "2 groups${dot}Page 1 of 1") -and
        (Find-TraceGroup '*openat*"/tmp/retry"*ENOENT*Later success on same path*') -and
        (Find-TraceGroup '*openat*"/tmp/missing"*ENOENT*')
    }
    if (Find-TraceGroup '*read*0.005 ms in calls*') { throw 'A successful read survived error filtering' }

    Click 'File descriptors'
    $field = Wait-Until 'descriptor lifetime search' { Find-Element 'Search descriptor lifetimes' $edit }
    $field.SetFocus()
    $field.GetCurrentPattern($value).SetValue('FD 3')
    $null = Wait-Until 'descriptor filter value applied' {
        (Find-Element 'Search descriptor lifetimes' $edit).GetCurrentPattern($value).Current.Value -ceq 'FD 3'
    }
    Click 'Filter descriptors'
    $secondOpen = Wait-Until 'reused FD 3 appears as two separate closed lifetimes' {
        if ((Find-Element "2 descriptor lifetimes${dot}Page 1 of 1") -and
            (Find-TraceGroup '*/tmp/retry*PID 501*FD 3*Lifetime 1*Closed')) {
            Find-TraceGroup '*/tmp/reused*PID 501*FD 3*Lifetime 2*Closed'
        }
    }
    $secondOpen.GetCurrentPattern($select).Select()
    $null = Wait-Until 'new lifetime shows its own write and closing evidence' {
        $evidence = Raw-Text $descriptorId
        $evidence -and $evidence.Contains('write(3</tmp/reused>') -and
            $evidence.Contains('close(3</tmp/reused>)') -and !$evidence.Contains('/tmp/retry')
    }
    Click 'Show related lifetimes'
    $alias = Wait-Until 'duplicate is related only to the new open file' {
        if (Find-Element "2 descriptor lifetimes${dot}Page 1 of 1") {
            Find-TraceGroup '*/tmp/reused*PID 501*FD 7*Lifetime 3*Closed'
        }
    }
    $alias.GetCurrentPattern($select).Select()
    $null = Wait-Until 'original dup and close lines explain the alias' {
        $evidence = Raw-Text $descriptorId
        $evidence -and $evidence.Contains('Duplicated from lifetime 2') -and $evidence.Contains('close(7</tmp/reused>)')
    }
    Click 'Back to syscall groups'

    Open-Trace ('"' + $first + '" "' + $second + '"')
    $null = Wait-Until 'per-process files merged by epoch and opened on the earlier record' {
        $raw = Raw-Text
        (Find-Element 'Files merged by epoch timestamp') -and
        (Find-TraceGroup '*PID 501*close*10.000 ms*') -and
        $raw -and $raw.Contains('split trace.502')
    }

    Open-Trace ('"' + $clockAfter + '" "' + $clockBefore + '"')
    $null = Wait-Until 'time-of-day files merge across midnight with relative elapsed time' {
        $raw = Raw-Text
        (Find-Element 'Files merged by time of day') -and
        (Find-TraceGroup '*PID 602*close*+30.000 ms*') -and
        (Find-TraceGroup '*PID 601*close*+40.000 ms*') -and
        $raw -and $raw.Contains('clock trace.601') -and $raw.Contains('23:59:59.980000 open')
    }
    Click 'File descriptors'
    $childDescriptor = Wait-Until 'child descriptor inherited across midnight' {
        Find-TraceGroup '*/tmp/clock-split*PID 602*FD 5*Closed'
    }
    $childDescriptor.GetCurrentPattern($select).Select()
    $null = Wait-Until 'child lifetime links the original clone and the later close' {
        $evidence = Raw-Text $descriptorId
        $evidence -and $evidence.Contains('Inherited from lifetime 1') -and
            $evidence.Contains('23:59:59.985000 clone') -and $evidence.Contains('00:00:00.010000 close')
    }
    Click 'Back to syscall groups'

    Open-Trace $working
    $null = Wait-Until 'working trace replaces split inputs' {
        Find-TraceGroup '*PID 21*openat*"/tmp/settings"*OK*'
    }
    Open-Trace $failing ('Compare with failing trace' + [char]0x2026)
    $null = Wait-Until 'first divergence identifies the failed open and the per-call delta' {
        $left = Raw-Text $workingId
        $right = Raw-Text $failingId
        (Find-TraceGroup 'First divergence*Changed result*openat*"/tmp/settings"*') -and
            (Find-Element 'Selected call duration delta +10.000 ms') -and
            $left -and $right -and $left.Contains('3</tmp/settings>') -and
            $right.Contains('ENOENT') -and $left.Contains('(+     0.001000) openat') -and
            $right.Contains('77      0.001000 openat') -and $left.Contains('+1.000 ms from run start') -and
            $right.Contains('+1.000 ms from run start')
    }
    Click 'Use selected pair as time zero'
    $null = Wait-Until 'both records start at zero on the selected reference' {
        $left = Raw-Text $workingId
        $right = Raw-Text $failingId
        $left -and $right -and $left.Contains('+0.000 ms from timing reference') -and
            $right.Contains('+0.000 ms from timing reference')
    }
    Apply-ChangeFilter 'OPENAT enoent /BIN/DIAG'
    $null = Wait-Until 'filter narrows by syscall, errno, and executable' {
        $right = Raw-Text $failingId
        (Find-Element "1 matching change row${dot}Page 1 of 1") -and
            !(Find-TraceGroup 'Changed count*read*') -and
            $right -and $right.Contains('ENOENT')
    }
    $cliReport = & $Cli compare-syscall-traces --working $working --failing $failing --filter 'OPENAT enoent /BIN/DIAG' |
        ConvertFrom-Json
    $cliChange = $cliReport.changes[0]
    if ($LASTEXITCODE -ne 0 -or $cliReport.changes.Count -ne 1 -or !$cliChange.first_divergence -or
        $cliChange.syscall -cne 'openat' -or $cliChange.failing_evidence.errno -cne 'ENOENT' -or
        $cliChange.failing_evidence.elapsed_ns -ne 1000000 -or $cliChange.duration_delta_ms -ne 10) {
        throw 'The offline CLI and viewer disagreed on the first filtered syscall divergence'
    }
    Apply-ChangeFilter 'read /TMP/DATA'
    $reads = Wait-Until 'FD reuse is normalized before comparing read counts' {
        Find-TraceGroup 'Changed count*read*/tmp/data*'
    }
    $reads.GetCurrentPattern($select).Select()
    $null = Wait-Until 'the excess read has its own evidence' {
        $left = Raw-Text $workingId
        $right = Raw-Text $failingId
        (Find-Element "Working 2${dot}Failing 1${dot}Count delta -1") -and
            $left -and $right -and $left.Contains('read(3</tmp/data>, "b"') -and
            $right.Contains('No corresponding call')
    }
    $missing = Find-Element 'Open failing evidence' $button
    if (!$missing -or $missing.Current.IsEnabled) {
        throw 'The missing counterpart offered a source jump'
    }
    Click 'Open working evidence'
    $null = Wait-Until 'working source marks the excess read next to the earlier read' {
        $raw = Raw-Text
        (Find-TraceGroup '*PID 21*read*/tmp/data*') -and
            $raw -and $raw.Contains('read(3</tmp/data>, "a"') -and
            ($raw -match "Comparison evidence\s+working diag.strace${dot}Line 6\s+[^\r\n]+read\(3</tmp/data>, ") -and
            $raw.Contains('read(3</tmp/data>, "b"') -and !(Raw-Text $workingId)
    }
    Press-Key 0x79 # F10 closes the source tab.
    $null = Wait-Until 'comparison restores the selected excess sample' {
        (Find-Element 'Evidence 2 of 2') -and (Raw-Text $workingId) -and (Raw-Text $failingId)
    }
    Click 'Previous comparison evidence'
    $null = Wait-Until 'the aligned reads show each original descriptor number' {
        $left = Raw-Text $workingId
        $right = Raw-Text $failingId
        $left -and $right -and $left.Contains('read(3</tmp/data>, "a"') -and
            $right.Contains('read(8</tmp/data>, "a"') -and
            $left.Contains('+3.000 ms from timing reference') -and
            $right.Contains('+3.000 ms from timing reference')
    }
    Click 'Open failing evidence'
    $null = Wait-Until 'failing source opens at the corresponding read' {
        $raw = Raw-Text
        (Find-TraceGroup '*PID 77*read*/tmp/data*') -and
            $raw -and ($raw -match "Comparison evidence\s+failing diag.strace${dot}Line 4\s+") -and
            $raw.Contains('read(8</tmp/data>, "a"') -and !(Raw-Text $failingId)
    }
    Press-Key 0x79
    $null = Wait-Until 'comparison retains the prior selection and timing reference' {
        $left = Raw-Text $workingId
        (Find-Element 'Evidence 1 of 2') -and (Find-Element "1 matching change row${dot}Page 1 of 1") -and
            $left -and $left.Contains('+3.000 ms from timing reference')
    }
    $anchorButton = Find-Element 'Use selected pair as time zero' $button
    if (!$anchorButton -or $anchorButton.Current.IsEnabled) {
        throw 'A repeat count mismatch was offered as a timing reference'
    }
    Set-ChangeFilter '/unapplied/draft'
    $report = Join-Path $State 'comparison report.txt'
    Save-Comparison $report
    $null = Wait-Until 'text report is atomically published' { [IO.File]::Exists($report) }
    $saved = [IO.File]::ReadAllText($report)
    if (!$saved.Contains("Applied filter${dot}read /TMP/DATA") -or
        !$saved.Contains("Reported changes${dot}1 of 4") -or
        !$saved.Contains("Count delta -1") -or
        !$saved.Contains("Timing reference${dot}openat${dot}Working working diag.strace line 2") -or
        !$saved.Contains('+3.000 ms from timing reference') -or
        !$saved.Contains('working diag.strace line 6') -or
        !$saved.Contains('read(3</tmp/data>, "a"') -or
        !$saved.Contains('read(8</tmp/data>, "a"') -or
        $saved.Contains('/tmp/settings') -or $saved.Contains('/unapplied/draft')) {
        throw "Comparison export did not capture the applied filter and selected evidence: $saved"
    }
    Save-Comparison $report
    $null = Wait-Until 'export refuses an existing destination' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        foreach ($control in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.Condition]::TrueCondition)) {
            if ($control.Current.Name.StartsWith('Could not save syscall comparison.')) { return $true }
        }
    }
    if ([IO.File]::ReadAllText($report) -cne $saved) { throw 'Export replaced an existing comparison file' }
    $jsonBase = Join-Path $State 'comparison structured report'
    Save-Comparison $jsonBase -Json
    $null = Wait-Until 'JSON report is published with the selected extension' {
        [IO.File]::Exists("$jsonBase.json")
    }
    $structured = [IO.File]::ReadAllText("$jsonBase.json") | ConvertFrom-Json
    $change = $structured.changes[0]
    if ($structured.schema_version -ne 1 -or $structured.filter -cne 'read /TMP/DATA' -or
        $structured.include_timing -or $structured.changes.Count -ne 1 -or $change.syscall -cne 'read' -or
        $change.count_delta -ne -1 -or $change.evidence_pair -ne 1 -or $change.evidence_pairs -ne 2 -or
        $structured.timing_anchor.syscall -cne 'openat' -or
        $structured.timing_anchor.working.file_number -ne 1 -or $structured.timing_anchor.failing.line -ne 2 -or
        $change.working_evidence.offset_from_anchor_ns -ne 3000000 -or
        $change.failing_evidence.offset_from_anchor_ns -ne 3000000 -or
        $change.working_evidence.sources[0].file_name -cne 'working diag.strace' -or
        $change.working_evidence.sources[0].line -ne 5 -or
        $change.failing_evidence.sources[0].file_name -cne 'failing diag.strace' -or
        $change.failing_evidence.sources[0].line -ne 4) {
        throw 'JSON export did not capture the applied filter and selected evidence pair'
    }
    Apply-ChangeFilter '/missing/filter/fixture'
    $null = Wait-Until 'a nonmatching filter clears stale comparison evidence' {
        (Find-Element 'No changes match the filter.') -and
            !(Find-TraceGroup 'Changed count*read*')
    }
    Click 'Use run starts'
    Apply-ChangeFilter ''
    $null = Wait-Until 'clearing the filter restores the first divergence and all changes' {
        $right = Raw-Text $failingId
        (Find-TraceGroup 'First divergence*Changed result*openat*') -and
            (Find-TraceGroup 'Changed count*read*') -and $right -and $right.Contains('ENOENT') -and
            $right.Contains('+1.000 ms from run start')
    }
    Apply-ChangeFilter 'changed timing close /tmp/data'
    $null = Wait-Until 'timing-only changes stay hidden by default while filtering' {
        Find-Element 'No changes match the filter.'
    }
    $timing = Wait-Until 'timing-only change toggle' {
        Find-Element 'Include timing-only changes' ([System.Windows.Automation.ControlType]::CheckBox)
    }
    $timing.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
    $null = Wait-Until 'the timing toggle reveals only the timing change matching the filter' {
        (Find-TraceGroup 'Changed timing*close*/tmp/data*') -and
            (Find-Element 'Selected call duration delta +4.000 ms') -and
            !(Find-TraceGroup 'Changed count*read*')
    }
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($trace)) -cne $before) { throw 'The viewer changed the original trace' }
    Run-Palette 'Diagnostics overview'
    $recent = Wait-Until 'split trace remembered as a recent file set' {
        $item = Find-Element 'Reopen clock trace.601' $button
        if (!$item) { $item = Find-Element 'Reopen clock trace.602' $button }
        if ($item -and $item.Current.IsEnabled) { $item }
    }
    $openRecentLabel = $recent.Current.Name
    Choose-DiagnosticFileAction ($openRecentLabel.Substring('Reopen '.Length)) 'Copy paths'
    $paths = Wait-Until 'clipboard has the split trace paths' { Read-ClipboardFixture }
    $paths = $paths -split '\r\n'
    $catalog = [IO.File]::ReadAllText((Join-Path $env:W11TERM_DATA_DIR 'recent-diagnostics.json')) | ConvertFrom-Json
    $recorded = $catalog.entries | Where-Object {
        $_.paths.Count -eq 2 -and ('Reopen ' + [IO.Path]::GetFileName($_.paths[0])) -ceq $openRecentLabel
    }
    if ($paths.Count -ne 2 -or !$recorded -or
        ![FixtureDialog]::SameFile($paths[0], $recorded.paths[0]) -or
        ![FixtureDialog]::SameFile($paths[1], $recorded.paths[1])) {
        throw 'Copy paths did not preserve the split trace selection order'
    }
    Click $openRecentLabel
    $null = Wait-Until 'recent split trace retains both sources and their order' {
        $raw = Raw-Text
        (Find-Element 'Files merged by time of day') -and (Find-TraceGroup '*PID 602*close*+30.000 ms*') -and
        $raw -and $raw.Contains('clock trace.601')
    }
    Run-Palette 'Diagnostics overview'
    Choose-DiagnosticFileAction 'working diag.strace' 'Use as baseline'
    $null = Wait-Until 'saved syscall baseline selected' { Find-Element "Baseline${dot}working diag.strace" }
    Choose-DiagnosticFileAction 'failing diag.strace' 'Compare with baseline'
    $null = Wait-Until 'saved syscall comparison restores both original traces in order' {
        $left = Raw-Text $workingId
        $right = Raw-Text $failingId
        (Find-TraceGroup 'First divergence*Changed result*openat*"/tmp/settings"*') -and
        $left -and $right -and $left.Contains('working diag.strace') -and
        $right.Contains('failing diag.strace') -and $right.Contains('ENOENT')
    }
    Click 'Back to working trace'
    $null = Wait-Until 'syscall viewer returns to the saved working trace' {
        Find-TraceGroup '*PID 21*openat*"/tmp/settings"*OK*'
    }
} finally {
    Close-FixtureApp $app
}
