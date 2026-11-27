param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$trace = Join-Path $State 'reviewed calls.strace'
$first = Join-Path $State 'split trace.501'
$second = Join-Path $State 'split trace.502'
$openLabel = 'Open syscall trace' + [char]0x2026
$dot = ' ' + [char]0xb7 + ' '
$times = [char]0xd7
$combo = [System.Windows.Automation.ControlType]::ComboBox
$listItem = [System.Windows.Automation.ControlType]::ListItem
$expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
$select = [System.Windows.Automation.SelectionItemPattern]::Pattern
$rawId = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'syscall-original-text')
$descriptorId = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'descriptor-original-text')
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$byListItem = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $listItem)
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
$before = [Convert]::ToBase64String([IO.File]::ReadAllBytes($trace))

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Open-Trace([string]$Text) {
    Click $openLabel
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
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($trace)) -cne $before) { throw 'The viewer changed the original trace' }
} finally {
    Close-FixtureApp $app
}
