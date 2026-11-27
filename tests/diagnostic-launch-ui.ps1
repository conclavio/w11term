[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$dot = ' ' + [char]0xb7 + ' '
$dropFixture = Join-Path $State 'diagnostic-drop.json'
$env:W11TERM_TEST_DIAGNOSTIC_DROP = $dropFixture
[IO.File]::WriteAllText($profiles, '{"startup_profile":"Startup shell","profiles":{"Startup shell":{"backend":"local","executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}}}')
[IO.File]::WriteAllText((Join-Path $State 'saved CPU.folded'), "worker;main 5`n")
[IO.File]::WriteAllText((Join-Path $State 'baseline CPU.folded'), "worker;main 5`n")
[IO.File]::WriteAllText((Join-Path $State 'candidate CPU.folded'), "worker;main 3`nworker;other 3`n")
[IO.File]::WriteAllText((Join-Path $State 'empty CPU.folded'), '')
[IO.File]::WriteAllText((Join-Path $State 'split trace.300'), "1700000000.001000 getpid() = 300`n")
[IO.File]::WriteAllText((Join-Path $State 'split trace.301'), "1700000000.002000 close(3) = 0`n")
$workingTrace = @'
1700000000.000000 execve("/bin/diag", ["diag"], NULL) = 0
1700000000.001000 openat(AT_FDCWD, "/tmp/settings", O_RDONLY) = 3</tmp/settings>
'@
$failingTrace = @'
1800000000.000000 execve("/bin/diag", ["diag"], NULL) = 0
1800000000.001000 openat(AT_FDCWD, "/tmp/settings", O_RDONLY) = -1 ENOENT (No such file or directory)
'@
[IO.File]::WriteAllText((Join-Path $State 'working trace.600'), $workingTrace + "`n")
[IO.File]::WriteAllText((Join-Path $State 'failing trace.700'), $failingTrace + "`n")
[IO.File]::WriteAllText((Join-Path $State 'working trace.601'), '1700000000.002000 execve("/bin/helper", ["helper"], NULL) = 0' + "`n")
[IO.File]::WriteAllText((Join-Path $State 'failing trace.701'), '1800000000.002000 execve("/bin/helper", ["helper"], NULL) = 0' + "`n")
[IO.File]::WriteAllBytes((Join-Path $State 'saved packets.pcap'), [byte[]]@(
    0xd4,0xc3,0xb2,0xa1, 2,0,4,0, 0,0,0,0, 0,0,0,0, 0xff,0xff,0,0, 1,0,0,0
))
$ids = @('uptime','dmesg','vmstat','mpstat','pidstat','iostat','free','sar-dev','sar-tcp','top')
$triage = @{
    version = 1
    target = @{
        connection_generation = 1; host = 'lab.example'; user = 'user'; port = 22
        host_key_fingerprint = 'SHA256:fixture'; deadline_seconds = 60; max_bytes_per_stream = 16384
        probes = @($ids | ForEach-Object { @{id = $_; command = $_; sampling = 'Fixture'} })
    }
    collection = @{
        started_unix_ms = 1; finished_unix_ms = 2; supervisor_status = 0
        samples = @($ids | ForEach-Object { @{
            id = $_; status = 'ok'; exit_status = 0
            stdout = @{text = ''; at_byte_limit = $false}; stderr = @{text = ''; at_byte_limit = $false}
        } })
    }
}
[IO.File]::WriteAllText((Join-Path $State 'saved triage.json'), ($triage | ConvertTo-Json -Depth 10))
[IO.File]::WriteAllText((Join-Path $State 'baseline triage.json'), ($triage | ConvertTo-Json -Depth 10))
$triage.collection.samples[1].status = 'permission-denied'
$triage.collection.samples[1].exit_status = 1
$triage.collection.samples[1].stderr.text = 'Operation not permitted'
[IO.File]::WriteAllText((Join-Path $State 'candidate triage.json'), ($triage | ConvertTo-Json -Depth 10))

function Open-Artifact([string[]]$LaunchArguments, [string]$Title) {
    $launch = @('--profiles', ('"' + $profiles + '"')) + $LaunchArguments
    $script:app = Start-Process -FilePath $Gui -ArgumentList $launch -WorkingDirectory $State `
        -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
    $null = $app.Handle
    $script:window = Wait-InputWindow
    $null = Wait-Until "window title for $Title" {
        $app.Refresh()
        $app.MainWindowTitle -ceq "w11term${dot}$Title"
    }
    if ((Control @('list')).sessions.Count -ne 0) { throw 'Opening the saved file started a terminal session' }
}

function Close-Artifact {
    if ($script:app) {
        Close-FixtureApp $script:app
        $script:app = $null
    }
}

try {
    Open-Artifact @('--open-cpu-profile','"saved CPU.folded"') 'CPU flame graph'
    $null = Wait-Until 'the saved profile fills the window' {
        $chart = Find-Element 'CPU flame graph chart'
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $chart -and (Find-Element '5 samples') -and
            $chart.Current.BoundingRectangle.Width -gt ($root.Current.BoundingRectangle.Width * 0.8)
    }
    $history = Join-Path $env:W11TERM_DATA_DIR 'recent-diagnostics.json'
    $null = Wait-Until 'the launched report is remembered under its absolute path' {
        if (![IO.File]::Exists($history)) { return }
        $recent = [IO.File]::ReadAllText($history) | ConvertFrom-Json
        $recent.entries[0].paths[0] -eq (Join-Path $State 'saved CPU.folded')
    }
    Invoke-Button (Find-Element 'Close CPU flame graph' $button)
    if (!$app.WaitForExit(5000)) { throw 'Closing the only viewer left the process running' }
    if ($app.ExitCode -ne 0) { throw "Closing the only viewer returned exit code '$($app.ExitCode)'" }
    Close-Artifact

    Open-Artifact @('--open-triage-report','"saved triage.json"') 'Linux performance triage'
    $null = Wait-Until 'the saved performance report loads' { Find-Element 'Opened Linux triage report' }
    Close-Artifact

    Open-Artifact @('--open-packet-capture','"saved packets.pcap"') 'Packet capture'
    $null = Wait-Until 'packet capture details and Wireshark action' {
        (Find-Element "PCAP${dot}24 bytes") -and (Find-Element 'Open in Wireshark' $button)
    }
    Close-Artifact

    Open-Artifact @('--open-syscall-trace','"split trace.301"','--open-syscall-trace','"split trace.300"') 'Syscall trace viewer'
    $null = Wait-Until 'both trace parts merge by epoch' { Find-Element 'Files merged by epoch timestamp' }
    Close-Artifact

    Open-Artifact @('--open-cpu-profile','"baseline CPU.folded"','--compare-with','"candidate CPU.folded"') 'CPU flame graph'
    $null = Wait-Until 'both CPU comparison sources load with their roles' {
        (Find-Element "Before${dot}baseline CPU.folded${dot}5 samples") -and
            (Find-Element "After${dot}candidate CPU.folded${dot}6 samples") -and
            (Find-Element "2 of 2 changed stacks${dot}Page 1 of 1")
    }
    $null = Wait-Until 'both newly opened CPU inputs appear in history' {
        $recent = [IO.File]::ReadAllText($history) | ConvertFrom-Json
        $files = @($recent.entries | ForEach-Object { $_.paths[0] })
        ($files -contains (Join-Path $State 'baseline CPU.folded')) -and
            ($files -contains (Join-Path $State 'candidate CPU.folded'))
    }
    Invoke-Button (Find-Element 'Back to flame graph' $button)
    $null = Wait-Until 'the baseline remains open after leaving the comparison' { Find-Element '5 samples' }
    Close-Artifact

    Open-Artifact @('--open-triage-report','"baseline triage.json"','--compare-with','"candidate triage.json"') 'Linux performance triage'
    $null = Wait-Until 'the saved triage pair shows the coverage regression' {
        (Find-Element "Before${dot}baseline triage.json") -and
            (Find-Element "After${dot}candidate triage.json") -and
            (Find-Element "After${dot}Permission denied${dot}Exit 1")
    }
    Close-Artifact

    $splitComparison = @('--open-syscall-trace','"working trace.600"','--open-syscall-trace','"working trace.601"',
        '--compare-with','"failing trace.700"','--compare-with','"failing trace.701"')
    Open-Artifact $splitComparison 'Syscall trace viewer'
    $null = Wait-Until 'the split comparison identifies the baseline and first result divergence' {
        (Find-Element "Working${dot}working trace.600") -and (Find-Element "Failing${dot}failing trace.700") -and
            (Find-Element "First divergence${dot}Changed result${dot}openat${dot}`"/tmp/settings`"")
    }
    $null = Wait-Until 'recent history retains both split trace sets' {
        $recent = [IO.File]::ReadAllText($history) | ConvertFrom-Json
        $working = $recent.entries | Where-Object { $_.paths[0] -eq (Join-Path $State 'working trace.600') }
        $failing = $recent.entries | Where-Object { $_.paths[0] -eq (Join-Path $State 'failing trace.700') }
        $working -and $failing -and $working.paths[1] -eq (Join-Path $State 'working trace.601') -and
            $failing.paths[1] -eq (Join-Path $State 'failing trace.701')
    }
    Invoke-Button (Find-Element 'Back to working trace' $button)
    $null = Wait-Until 'the working side reopens as a merged trace' { Find-Element 'Files merged by epoch timestamp' }
    Close-Artifact

    Open-Artifact @('--open-cpu-profile','"saved CPU.folded"') 'CPU flame graph'
    $null = Wait-Until 'saved CPU profile is ready before returning to the overview' { Find-Element '5 samples' }
    Invoke-Button (Find-Element 'Return to diagnostics overview' $button)
    $browse = Wait-Until 'open a diagnostic directly from the overview' {
        Find-Element ('Open saved diagnostic' + [char]0x2026) $button
    }
    Invoke-Button $browse
    $picker = Wait-Until 'owned diagnostic picker' { Find-OwnedDialog 'Open saved diagnostic' }
    $packetPath = Join-Path $State 'saved packets.pcap'
    $null = Wait-Until 'enter the packet capture in the diagnostic picker' { [FixtureDialog]::SetFilename($picker, $packetPath, '') }
    $null = Wait-Until 'accept the generic diagnostic chooser' { [FixtureDialog]::ClickButton($picker, 1) }
    $null = Wait-Until 'the generic picker routes a packet capture' { Find-Element "PCAP${dot}24 bytes" }
    Invoke-Button (Find-Element 'Return to diagnostics overview' $button)

    $mixedPaths = @($packetPath, (Join-Path $State 'saved CPU.folded'))
    [IO.File]::WriteAllText($dropFixture, (ConvertTo-Json -InputObject $mixedPaths))
    $target = Wait-Until 'drop target in Diagnostics overview' { Find-Element 'Drop fixture diagnostic' $button }
    Invoke-Button $target
    $null = Wait-Until 'a mixed drop is rejected in the overview' {
        Find-Element 'Choose one diagnostic or the matching parts of one split syscall trace: Select one combined trace or matching NAME.PID files'
    }
    $splitPaths = @((Join-Path $State 'split trace.301'), (Join-Path $State 'split trace.300'))
    [IO.File]::WriteAllText($dropFixture, (ConvertTo-Json -InputObject $splitPaths))
    Invoke-Button (Find-Element 'Drop fixture diagnostic' $button)
    $null = Wait-Until 'the diagnostic drop opens both parts of the trace' { Find-Element 'Files merged by epoch timestamp' }
    Close-Artifact

    Open-Artifact @('--open-cpu-profile','"empty CPU.folded"') 'CPU flame graph'
    $null = Wait-Until 'a malformed artifact is reported in its viewer' { Find-Element 'The folded stacks file has no samples' }

    $windowId = ((Control @('list-windows')).windows | Select-Object -First 1).window_id
    $other = Start-Process -FilePath $Gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) `
        -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'other-gui.log')
    try {
        $otherWindow = Wait-Until 'other window is ready' {
            $other.Refresh()
            if ($other.MainWindowTitle.StartsWith("w11term${dot}")) { $other.MainWindowHandle }
        }
        $foreground = [FixtureMessages]::GetForegroundWindow()
        $open = @('open-diagnostic','--window',"$windowId")
        $details = Control ($open + @('--kind','cpu-profile','--file',(Join-Path $State 'baseline CPU.folded'),
            '--compare-with',(Join-Path $State 'candidate CPU.folded')))
        if ($details.window_id -ne $windowId -or $details.kind -cne 'cpu-profile' -or !$details.comparison) {
            throw 'Control replied with the wrong diagnosis target'
        }
        $null = Wait-Until 'control opens a comparison in the requested window' {
            (Find-Element "Before${dot}baseline CPU.folded${dot}5 samples") -and
                (Find-Element "After${dot}candidate CPU.folded${dot}6 samples")
        }
        if ($foreground -ne $window -and [FixtureMessages]::GetForegroundWindow() -eq $window) {
            throw "Opening a diagnostic activated the target window over $foreground (other window $otherWindow)"
        }

        $empty = Join-Path $State 'empty CPU.folded'
        $invalid = & $Cli --pid $app.Id @open --kind cpu-profile --file $empty | ConvertFrom-Json
        if ($LASTEXITCODE -ne 1 -or $invalid.ok -or $invalid.error -notmatch 'has no samples') {
            throw 'Control did not return the diagnostic reader failure'
        }
        $badWindow = & $Cli --pid $app.Id open-diagnostic --window 999 --kind cpu-profile --file $empty | ConvertFrom-Json
        if ($LASTEXITCODE -ne 1 -or $badWindow.ok -or $badWindow.error -notmatch 'window') {
            throw 'Control did not reject an unknown target window'
        }

        $null = Control ($open + @('--kind','triage-report','--file',(Join-Path $State 'saved triage.json')))
        $null = Wait-Until 'control opens a saved triage report' { Find-Element 'Opened Linux triage report' }
        $null = Control ($open + @('--kind','packet-capture','--file',(Join-Path $State 'saved packets.pcap')))
        $null = Wait-Until 'control opens a saved packet capture' { Find-Element "PCAP${dot}24 bytes" }
        $null = Control ($open + @('--kind','syscall-trace','--file',(Join-Path $State 'split trace.300'),
            '--file',(Join-Path $State 'split trace.301')))
        $null = Wait-Until 'control opens a split trace' { Find-Element 'Files merged by epoch timestamp' }
        $details = Control ($open + @('--kind','syscall-trace','--file',(Join-Path $State 'working trace.600'),
            '--file',(Join-Path $State 'working trace.601'),'--compare-with',(Join-Path $State 'failing trace.700'),
            '--compare-with',(Join-Path $State 'failing trace.701')))
        if (!$details.comparison) { throw 'Expected a completed syscall comparison' }
        $null = Wait-Until 'control opens the requested split comparison' {
            (Find-Element "Working${dot}working trace.600") -and (Find-Element "Failing${dot}failing trace.700")
        }
        if ($foreground -ne $window -and [FixtureMessages]::GetForegroundWindow() -eq $window) {
            throw 'Opening a syscall viewer activated the target window'
        }
        if ((Control @('list')).sessions.Count -ne 0) { throw 'A diagnostic control request opened a terminal session' }
    } finally {
        Close-FixtureApp $other
    }
} finally {
    Close-Artifact
}
