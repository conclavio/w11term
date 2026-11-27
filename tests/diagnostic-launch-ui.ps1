[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$dot = ' ' + [char]0xb7 + ' '
[IO.File]::WriteAllText($profiles, '{"startup_profile":"Startup shell","profiles":{"Startup shell":{"backend":"local","executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}}}')
[IO.File]::WriteAllText((Join-Path $State 'saved CPU.folded'), "worker;main 5`n")
[IO.File]::WriteAllText((Join-Path $State 'empty CPU.folded'), '')
[IO.File]::WriteAllText((Join-Path $State 'split trace.300'), "1700000000.001000 getpid() = 300`n")
[IO.File]::WriteAllText((Join-Path $State 'split trace.301'), "1700000000.002000 close(3) = 0`n")
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

    Open-Artifact @('--open-cpu-profile','"empty CPU.folded"') 'CPU flame graph'
    $null = Wait-Until 'a malformed artifact is reported in its viewer' { Find-Element 'The folded stacks file has no samples' }
} finally {
    Close-Artifact
}
