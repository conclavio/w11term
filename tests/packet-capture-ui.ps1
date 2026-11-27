param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$destination = Join-Path $State 'reviewed capture.pcap'
$olderCapture = Join-Path $State 'older capture.pcapng'
$partialCapture = Join-Path $State 'incomplete capture.pcapng'
$conversationCapture = Join-Path $State 'IP conversations.pcap'
$slowerCapture = Join-Path $State 'IP conversations spaced out.pcap'
$protocolCapture = Join-Path $State 'IP protocols.pcap'
$manyPortsCapture = Join-Path $State 'Fifteen UDP conversations.pcap'
$manyIpCapture = Join-Path $State 'Fifteen IP conversations.pcap'
$emptyCapture = Join-Path $State 'Empty capture.pcap'
$invalidCapture = Join-Path $State 'not a capture.pcap'
$viewer = Join-Path $State 'Fixture Wireshark.exe'
$startMarker = [IO.File]::ReadAllText((Join-Path $State 'pcap-start-marker'))
$saveLabel = 'Capture and save' + [char]0x2026
$openCaptureLabel = 'Open capture file' + [char]0x2026
$compareCaptureLabel = 'Compare captures' + [char]0x2026
$chooseViewerLabel = 'Choose Wireshark executable' + [char]0x2026
$edit = [System.Windows.Automation.ControlType]::Edit
$check = [System.Windows.Automation.ControlType]::CheckBox
$value = [System.Windows.Automation.ValuePattern]::Pattern
$toggle = [System.Windows.Automation.TogglePattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '
$bothDirections = [char]0x2194
[IO.File]::WriteAllBytes($olderCapture, [byte[]]@(
    # Little-endian section, two named interfaces, two timed packets and an untimed packet.
    10,13,13,10, 28,0,0,0, 77,60,43,26, 1,0,0,0, 255,255,255,255,255,255,255,255, 28,0,0,0,
    1,0,0,0, 32,0,0,0, 1,0,0,0, 255,0,0,0, 2,0,5,0, 119,105,114,101,100,0,0,0, 32,0,0,0,
    1,0,0,0, 32,0,0,0, 20,1,0,0, 0,0,0,0, 2,0,8,0, 119,105,114,101,108,101,115,115, 32,0,0,0,
    6,0,0,0, 36,0,0,0, 0,0,0,0, 0,0,0,0, 64,66,15,0, 4,0,0,0, 8,0,0,0, 0,0,0,0, 36,0,0,0,
    6,0,0,0, 36,0,0,0, 1,0,0,0, 0,0,0,0, 96,227,22,0, 4,0,0,0, 4,0,0,0, 0,0,0,0, 36,0,0,0,
    3,0,0,0, 20,0,0,0, 4,0,0,0, 0,0,0,0, 20,0,0,0,
    # ISB snapshots for wired and wireless. Wireless reports zero interface drops and five OS drops.
    5,0,0,0, 48,0,0,0, 0,0,0,0, 0,0,0,0, 128,132,30,0,
    4,0,8,0, 12,0,0,0,0,0,0,0, 5,0,8,0, 1,0,0,0,0,0,0,0, 48,0,0,0,
    5,0,0,0, 60,0,0,0, 1,0,0,0, 0,0,0,0, 160,37,38,0,
    4,0,8,0, 100,0,0,0,0,0,0,0, 5,0,8,0, 0,0,0,0,0,0,0,0,
    7,0,8,0, 5,0,0,0,0,0,0,0, 60,0,0,0
))
[IO.File]::WriteAllBytes($partialCapture, [byte[]]([IO.File]::ReadAllBytes($olderCapture) + @(6,0,0,0,36,0,0,0,0,0,0,0)))
$eth4 = [byte[]](@(0) * 12 + @(8,0))
$eth6 = [byte[]](@(0) * 12 + @(134,221))
$ipv4 = [byte[]]@(69,0,0,20, 0,0,0,0, 64,253,0,0, 192,0,2,1, 192,0,2,2)
$reverse = [byte[]]($ipv4 + @(0,0,0,0))
$reverse[3] = 24
$reverse[15] = 2
$reverse[19] = 1
$ipv6 = [byte[]]( @(96,0,0,0,0,0,59,64) +
    [Net.IPAddress]::Parse('2001:db8::1').GetAddressBytes() +
    [Net.IPAddress]::Parse('2001:db8::2').GetAddressBytes() )
$ipv4Tunnel = [byte[]]$ipv4.Clone()
$ipv4Tunnel[3] = 60
$ipv4Tunnel[9] = 41
$ipv6Tunnel = [byte[]]$ipv6.Clone()
$ipv6Tunnel[5] = 20
$ipv6Tunnel[6] = 4
$sameVersionTunnel = [byte[]]$ipv4.Clone()
$sameVersionTunnel[3] = 40
$sameVersionTunnel[9] = 4
$sameVersionTunnel[15] = 3
$sameVersionTunnel[19] = 4
function Write-CaptureFixture([string]$Path, [uint32]$LinkType, [byte[][]]$Frames, [uint32]$Spacing = 1) {
    $writer = [IO.BinaryWriter]::new([IO.File]::Create($Path))
    try {
        $writer.Write([byte[]]@(212,195,178,161, 2,0,4,0, 0,0,0,0, 0,0,0,0, 128,0,0,0))
        $writer.Write($LinkType)
        $number = 0
        foreach ($frame in $Frames) {
            $number++
            $writer.Write([uint32]($number * $Spacing))
            $writer.Write([uint32]0)
            $writer.Write([uint32]$frame.Length)
            $writer.Write([uint32]$frame.Length)
            $writer.Write($frame)
        }
    } finally {
        $writer.Dispose()
    }
}
$conversationFrames = @(
    ,([byte[]]($eth4 + $ipv4))
    ,([byte[]]($eth4 + $reverse))
    ,([byte[]](@(0) * 12 + @(8,6)))
    ,([byte[]]($eth6 + $ipv6))
    ,([byte[]]($eth4 + $ipv4Tunnel + $ipv6))
    ,([byte[]]($eth6 + $ipv6Tunnel + $ipv4))
    ,([byte[]]($eth4 + $sameVersionTunnel + $ipv4))
)
Write-CaptureFixture $conversationCapture 1 $conversationFrames
Write-CaptureFixture $slowerCapture 1 $conversationFrames 2
$protocolFrames = [System.Collections.Generic.List[byte[]]]::new()
$servicePorts = [byte[]]@(0xc0, 0x00, 0x01, 0xbb)
foreach ($flags in @(0x02, 0x12, 0x04, 0x11, 0x02)) {
    $tcp = [byte[]]($ipv4 + [byte[]]::new(20))
    $tcp[3] = 40
    $tcp[9] = 6
    [Array]::Copy($servicePorts, 0, $tcp, 20, 4)
    $tcp[32] = 0x50
    $tcp[33] = $flags
    $protocolFrames.Add($tcp)
}
$protocolFrames[1][15] = 2
$protocolFrames[1][19] = 1
[Array]::Copy([byte[]]@(0x01, 0xbb, 0xc0, 0x00), 0, $protocolFrames[1], 20, 4)
$protocolFrames[4][7] = 1 # Later IPv4 fragment with bytes that look like a SYN.
$udp = [byte[]]($ipv4 + [byte[]]::new(8))
$udp[3] = 28
$udp[9] = 17
[Array]::Copy($servicePorts, 0, $udp, 20, 4)
$udp[25] = 8
$protocolFrames.Add($udp)
$icmpv6 = [byte[]]($ipv6 + [byte[]]::new(8))
$icmpv6[5] = 8
$icmpv6[6] = 58
$icmpv6[40] = 128
$protocolFrames.Add($icmpv6)
$missingExtension = [byte[]]$ipv6.Clone()
$missingExtension[5] = 8
$missingExtension[6] = 60
$protocolFrames.Add($missingExtension)
$protocolFrames.Add([byte[]]@(0))
Write-CaptureFixture $protocolCapture 101 ($protocolFrames.ToArray())
$manyPortsFrames = [System.Collections.Generic.List[byte[]]]::new()
foreach ($port in 50001..50015) {
    $frame = [byte[]]$udp.Clone()
    $frame[20] = [byte]($port -shr 8)
    $frame[21] = [byte]($port -band 255)
    $manyPortsFrames.Add($frame)
}
Write-CaptureFixture $manyPortsCapture 101 ($manyPortsFrames.ToArray())
$manyIpFrames = [System.Collections.Generic.List[byte[]]]::new()
foreach ($peer in 2..16) {
    $frame = [byte[]]$ipv4.Clone()
    $frame[19] = [byte]$peer
    $manyIpFrames.Add($frame)
}
Write-CaptureFixture $manyIpCapture 101 ($manyIpFrames.ToArray())
Write-CaptureFixture $emptyCapture 101 ([byte[][]]@())
[IO.File]::WriteAllText($invalidCapture, 'plain text')
Add-Type -OutputType WindowsApplication -OutputAssembly $viewer -TypeDefinition @'
using System.IO;
public static class Viewer {
    public static void Main(string[] args) {
        if (args.Length == 2 && args[0] == "-r" && File.Exists(args[1])) {
            File.WriteAllText(args[1] + ".opened", args[0]);
        }
        if (args.Length == 4 && args[0] == "-r" && args[2] == "-Y" && File.Exists(args[1])) {
            File.WriteAllText(args[1] + ".filter", args[3]);
        }
    }
}
'@

function Click([string]$Name, [System.Windows.Automation.ControlType]$Type = $button) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $Type
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Set-Field([string]$Name, [string]$Text) {
    $field = Wait-Until "editable '$Name' field" { Find-Element $Name $edit }
    $field.SetFocus()
    $field = Wait-Until "focused '$Name' field" {
        $item = Find-Element $Name $edit
        if ($item -and $item.Current.HasKeyboardFocus) { $item }
    }
    $field.GetCurrentPattern($value).SetValue($Text)
    $null = Wait-Until "'$Name' accepted the value" {
        (Find-Element $Name $edit).GetCurrentPattern($value).Current.Value -ceq $Text
    }
}

function Select-Capture([string]$Path, [string]$Trigger = $openCaptureLabel, [string]$Title = 'Open packet capture') {
    Click $Trigger
    $dialog = Wait-Until 'owned source capture picker' { Find-OwnedDialog $Title }
    $null = Wait-Until 'enter the source capture path' { [FixtureDialog]::SetFilename($dialog, $Path, '') }
    $null = Wait-Until 'select the source capture' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'source capture picker closed' { !(Find-OwnedDialog $Title) }
}

function Test-CaptureFile([string]$Path) {
    $filename = [IO.Path]::GetFileName($Path)
    foreach ($label in Get-NativeTextLabels) {
        if ($label.EndsWith($filename, [StringComparison]::OrdinalIgnoreCase) -and
            [FixtureDialog]::SameFile($Path, $label)) {
            return $true
        }
    }
}

function Select-Viewer([switch]$FromComparison) {
    $type = if ($FromComparison) { [System.Windows.Automation.ControlType]::Hyperlink } else { $button }
    Click $chooseViewerLabel $type
    $dialog = Wait-Until 'owned executable picker' { Find-OwnedDialog 'Choose Wireshark executable' }
    $null = Wait-Until 'enter the fixture viewer path' { [FixtureDialog]::SetFilename($dialog, $viewer, '') }
    $null = Wait-Until 'select the fixture viewer' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'viewer picker closed' { !(Find-OwnedDialog 'Choose Wireshark executable') }
}

function Save-PacketComparison([string]$Path, [switch]$Json) {
    Click ('Export comparison' + [char]0x2026)
    $dialog = Wait-Until 'owned packet comparison export dialog' {
        Find-OwnedDialog 'Save packet capture comparison'
    }
    if ($Json) {
        $null = Wait-Until 'select JSON in the packet comparison export' {
            [FixtureDialog]::SelectFileType($dialog, 'JSON report (*.json)')
        }
    }
    $null = Wait-Until 'enter the packet comparison export path' {
        [FixtureDialog]::SetFilename($dialog, $Path, 'packet-capture-comparison.txt') -or
            [FixtureDialog]::SetFilename($dialog, $Path, 'packet-capture-comparison.json')
    }
    $null = Wait-Until 'save the packet comparison report' { [FixtureDialog]::ClickButton($dialog, 1) }
}

function Save-CaptureExcerpt([string]$Label, [string]$Path) {
    Click $Label
    $dialog = Wait-Until 'owned capture excerpt save dialog' { Find-OwnedDialog 'Save capture excerpt' }
    $suggestion = 'capture-excerpt' + [IO.Path]::GetExtension($Path)
    $null = Wait-Until 'enter the capture excerpt path' { [FixtureDialog]::SetFilename($dialog, $Path, $suggestion) }
    $null = Wait-Until 'save the capture excerpt' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the capture excerpt is published' { [IO.File]::Exists($Path) }
}

function Open-PacketSection([string]$Id) {
    $expander = Wait-Until "the capture exposes '$Id'" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
        $item = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        if ($item -and $item.Current.IsEnabled) { $item }
    }
    $expander.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $expander
}

function Wait-CaptureCombo([string]$Name) {
    $search = @{ scroll = $null }
    Wait-Until "visible capture dropdown '$Name'" {
        $item = Find-Element $Name ([System.Windows.Automation.ControlType]::ComboBox)
        if ($item -and $item.Current.IsEnabled) {
            if (!$item.Current.IsOffscreen) { return $item }
            Move-NativeControlIntoView $item $search
        }
    }
}

function Select-CaptureComboItem([string]$Combo, [string]$Name) {
    $dropdown = (Wait-CaptureCombo $Combo).GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
    $item = Wait-Until "'$Name' is selectable in '$Combo'" {
        if ($dropdown.Current.ExpandCollapseState -eq 'Collapsed') {
            $dropdown.Expand()
        }
        Find-Element $Name ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
}

function Select-ComparisonConversationType([string]$Name) {
    Select-CaptureComboItem 'Conversation type' $Name
}

function Select-ComparisonCaptureSide([string]$Name) {
    Select-CaptureComboItem 'Capture to open in Wireshark' $Name
}

function Udp-PortFilter([int]$Port) {
    'frame.protocols matches "^(?:(?!ip:|ipv6:)[^:]+:)*ip:udp(?::|$)" && ' +
        "((ip.src#1 == 192.0.2.1 && udp.srcport#1 == $Port && ip.dst#1 == 192.0.2.2 && udp.dstport#1 == 443) || " +
        "(ip.src#1 == 192.0.2.2 && udp.srcport#1 == 443 && ip.dst#1 == 192.0.2.1 && udp.dstport#1 == $Port))"
}

function Open-Conversation([string]$Label, [string]$Filter) {
    $row = Wait-Until "the conversation row '$Label' can be selected" {
        Find-Element $Label ([System.Windows.Automation.ControlType]::ListItem)
    }
    $row.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Click 'Open conversation in Wireshark'
    $null = Wait-Until "Wireshark receives the original file and display filter for '$Label'" {
        $filterPath = $conversationCapture + '.filter'
        [IO.File]::Exists($filterPath) -and [IO.File]::ReadAllText($filterPath) -ceq $Filter
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $local = (Sessions | Where-Object label -eq 'Palette PowerShell').id
    Invoke-Button (Get-NativeMenuAction 'Tab menu' 'Diagnostics overview' -Submenu 'Diagnostics')
    $null = Wait-Until 'overview shown alongside local PowerShell' {
        (Find-Element 'Diagnostics overview panel') -and
            (Find-Element 'Select a connected Linux SSH pane to run diagnostics.')
    }
    Click 'Packet capture'
    Select-Capture $olderCapture
    $null = Wait-Until 'an earlier capture opens before any SSH connection' {
        (Find-Element "PCAPNG${dot}292 bytes") -and
            (Find-Element "Packets 3${dot}12 captured bytes${dot}1 truncated") -and
            (Find-Element "Timestamp span 0.500 s${dot}2 of 3 packets timestamped") -and
            (Find-Element "Interfaces 2${dot}Ethernet, Linux cooked v2") -and
            (Find-Element "Interface packets 2${dot}8 captured bytes${dot}1 truncated") -and
            (Find-Element "Reported${dot}Received 12") -and
            (Find-Element "Reported${dot}Interface drops 1")
    }
    $expander = Open-PacketSection 'packet-timeline'
    $null = Wait-Until 'the timeline chart and its earliest peak are visible' {
        (Find-Element 'Packet timeline chart' -Visible) -and
            (Find-Element 'Bucket width 0.008192 s' -Visible) -and
            (Find-Element "Peak 1 packet${dot}4 bytes${dot}+0.000 s from timeline start" -Visible)
    }
    $expander.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Collapse()
    $picker = Wait-CaptureCombo 'Saved capture interfaces'
    $picker.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $wireless = Wait-Until 'the second saved interface can be selected' {
        Find-Element "Section 1${dot}Interface 1${dot}wireless" ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $wireless.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'the selected interface shows its own capture totals and snap length' {
        (Find-Element "wireless${dot}Linux cooked v2${dot}Unlimited snap length") -and
            (Find-Element "Interface packets 1${dot}4 captured bytes${dot}0 truncated") -and
            (Find-Element "Reported${dot}Received 100") -and
            (Find-Element "Reported${dot}Interface drops 0${dot}OS drops 5")
    }
    $summary = & $Cli inspect-packet-capture $olderCapture | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or !$summary.complete -or $summary.format -cne 'pcapng' -or
        $summary.file_bytes -ne 292 -or $summary.packets -ne 3 -or $summary.captured_bytes -ne 12 -or
        $summary.truncated_packets -ne 1 -or $summary.timestamped_packets -ne 2 -or
        $summary.timestamp_span_ns -cne '500000000' -or $summary.interfaces -ne 2 -or
        $summary.link_types.Count -ne 2 -or $summary.link_types[0] -ne 1 -or $summary.link_types[1] -ne 276 -or
        $summary.interface_details[0].name -cne 'wired' -or $summary.interface_details[0].packets -ne 2 -or
        $summary.interface_details[1].name -cne 'wireless' -or $summary.interface_details[1].packets -ne 1 -or
        $summary.interface_details[1].snap_length -ne $null -or
        $summary.interface_details[0].capture_statistics.os_dropped -ne $null -or
        $summary.interface_details[0].capture_statistics.interface_dropped -ne 1 -or
        $summary.interface_details[1].capture_statistics.timestamp_unix_ns -cne '2500000000' -or
        $summary.interface_details[1].capture_statistics.interface_dropped -ne 0 -or
        $summary.interface_details[1].capture_statistics.os_dropped -ne 5) {
        throw 'The offline inspection differs from the saved capture panel'
    }
    $timeline = $summary.timeline
    if ($timeline.start_unix_ns -cne '999424000' -or $timeline.bucket_width_ns -cne '8192000' -or
        $timeline.buckets.Count -ne 62 -or $timeline.buckets[0].packets -ne 1 -or
        $timeline.buckets[1].packets -ne 0 -or $timeline.buckets[-1].packets -ne 1 -or
        ($timeline.buckets | Measure-Object -Property captured_bytes -Sum).Sum -ne 8) {
        throw 'The CLI timeline disagrees with the capture timestamps or counts the untimed packet'
    }
    Select-Capture $invalidCapture
    $null = Wait-Until 'a non-capture file leaves the previous capture selected' {
        (Find-Element 'Choose a PCAP or PCAPNG capture file') -and (Find-Element "PCAPNG${dot}292 bytes")
    }
    Click 'Return to diagnostics overview'
    Click ('Reopen ' + [IO.Path]::GetFileName($olderCapture))
    $null = Wait-Until 'recent packet capture reopens and is validated again' {
        (Find-Element "PCAPNG${dot}292 bytes") -and
        (Find-Element "Interface packets 2${dot}8 captured bytes${dot}1 truncated") -and
        !(Find-Element 'Choose a PCAP or PCAPNG capture file') -and
        !(Find-Element 'Diagnostics overview panel')
    }
    Select-Viewer
    if ([IO.File]::Exists($olderCapture + '.opened')) { throw 'Selecting the local file launched Wireshark' }
    $expander = Open-PacketSection 'packet-timeline'
    $intervals = Wait-CaptureCombo 'Packet timeline intervals'
    $intervals.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $interval = Wait-Until 'the second populated interval is available' {
        Find-Element "+0.499712 s${dot}1 packet" ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $interval.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Click 'Open interval in Wireshark'
    $null = Wait-Until 'the original capture and exact UTC interval reach Wireshark' {
        $displayFilter = $olderCapture + '.filter'
        [IO.File]::Exists($displayFilter) -and [IO.File]::ReadAllText($displayFilter) -ceq
            'frame.time >= "1970-01-01T00:00:01.499136000Z" && frame.time < "1970-01-01T00:00:01.507328000Z"'
    }
    $excerpt = Join-Path $State 'selected wireless interval.pcapng'
    Save-CaptureExcerpt ('Save capture excerpt' + [char]0x2026) $excerpt
    $excerptReport = & $Cli inspect-packet-capture $excerpt | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or !$excerptReport.complete -or $excerptReport.packets -ne 1 -or
        $excerptReport.captured_bytes -ne 4 -or $excerptReport.timeline.start_unix_ns -cne '1500000000' -or
        $excerptReport.interfaces -ne 2 -or $excerptReport.interface_details[1].name -cne 'wireless' -or
        $null -ne $excerptReport.interface_details[0].capture_statistics -or
        $null -ne $excerptReport.interface_details[1].capture_statistics) {
        throw 'The PCAPNG excerpt did not preserve the selected packet and its interface'
    }
    $expander.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Collapse()
    Click 'Open in Wireshark'
    $null = Wait-Until 'the local capture reaches Wireshark without an SSH pane' {
        $opened = $olderCapture + '.opened'
        [IO.File]::Exists($opened) -and [IO.File]::ReadAllText($opened) -ceq '-r'
    }
    Select-Capture $partialCapture
    $null = Wait-Until 'a damaged capture retains its complete packets and can open in Wireshark' {
        $open = Find-Element 'Open in Wireshark' $button
        $open -and $open.Current.IsEnabled -and
            (Find-Element "Partial packet summary${dot}Incomplete or invalid PCAPNG block length") -and
            (Find-Element "Packets 3${dot}12 captured bytes${dot}1 truncated")
    }
    $partial = & $Cli inspect-packet-capture $partialCapture | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $partial.complete -or $partial.packets -ne 3 -or
        $partial.issue -cne 'Incomplete or invalid PCAPNG block length') {
        throw 'CLI inspection did not preserve complete records in the partial capture'
    }
    Select-Capture $conversationCapture
    $null = Wait-Until 'the conversation capture opens offline' {
        Test-CaptureFile $conversationCapture
    }
    $conversationTimeline = Open-PacketSection 'packet-timeline'
    $intervals = Wait-CaptureCombo 'Packet timeline intervals'
    $intervals.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $reverseInterval = Wait-Until 'the interval containing the reverse IPv4 packet can be selected' {
        Find-Element "+1.048576 s${dot}1 packet" ([System.Windows.Automation.ControlType]::ListItem)
    }
    $reverseInterval.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $conversationTimeline.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Collapse()
    $expander = Open-PacketSection 'packet-ip-conversations'
    $null = Wait-Until 'the address pair shows totals and bytes in both directions' {
        (Find-Element "192.0.2.1 $bothDirections 192.0.2.2" -Visible) -and
        (Find-Element "Tracked IP pairs 3${dot}Unclassified packets 1") -and
        (Find-Element "3 packets${dot}146 captured bytes") -and
        (Find-Element "Left to right 108 bytes${dot}Right to left 38 bytes")
    }
    $summary = & $Cli inspect-packet-capture $conversationCapture | ConvertFrom-Json
    $conversation = $summary.ip_conversations.top[0]
    if ($LASTEXITCODE -ne 0 -or $summary.ip_conversations.tracked_pairs -ne 3 -or
        $summary.ip_conversations.unclassified_packets -ne 1 -or $summary.ip_conversations.untracked_packets -ne 0 -or
        $conversation.a -cne '192.0.2.1' -or $conversation.b -cne '192.0.2.2' -or
        $conversation.a_to_b.packets -ne 2 -or $conversation.a_to_b.captured_bytes -ne 108 -or
        $conversation.b_to_a.packets -ne 1 -or $conversation.b_to_a.captured_bytes -ne 38) {
        throw 'The CLI conversation ranking differs from the offline capture panel'
    }
    $null = Wait-Until 'conversation launch needs a selected row' {
        $open = Find-Element 'Open conversation in Wireshark' $button
        $open -and !$open.Current.IsEnabled
    }
    $ipv4Filter = 'frame.protocols matches "^(?:(?!ip:|ipv6:)[^:]+:)*ip(?::|$)" && ' +
        '((ip.src#1 == 192.0.2.1 && ip.dst#1 == 192.0.2.2) || (ip.src#1 == 192.0.2.2 && ip.dst#1 == 192.0.2.1))'
    $ipv6Filter = 'frame.protocols matches "^(?:(?!ip:|ipv6:)[^:]+:)*ipv6(?::|$)" && ' +
        '((ipv6.src#1 == 2001:db8::1 && ipv6.dst#1 == 2001:db8::2) || (ipv6.src#1 == 2001:db8::2 && ipv6.dst#1 == 2001:db8::1))'
    $limitedIpv4Filter = "($ipv4Filter) && " +
        '(frame.time >= "1970-01-01T00:00:01.966080000Z" && frame.time < "1970-01-01T00:00:02.097152000Z")'
    Open-Conversation "192.0.2.1 $bothDirections 192.0.2.2" $ipv4Filter
    Open-Conversation "2001:db8::1 $bothDirections 2001:db8::2" $ipv6Filter
    $scopeLabel = "Limit to timeline interval${dot}+1.048576 to +1.179648 s"
    (Wait-CheckBox $scopeLabel ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    Open-Conversation "192.0.2.1 $bothDirections 192.0.2.2" $limitedIpv4Filter
    (Wait-CheckBox $scopeLabel ([System.Windows.Automation.ToggleState]::On)).Toggle()
    Open-Conversation "192.0.2.1 $bothDirections 192.0.2.2" $ipv4Filter
    $expander.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Collapse()
    Select-Capture $olderCapture $compareCaptureLabel 'Choose after packet capture'
    $null = Wait-Until 'comparison shows both files, signed capture changes, and the changed IP pair' {
        (Find-Element "Before${dot}IP conversations.pcap${dot}PCAP") -and
        (Find-Element "After${dot}older capture.pcapng${dot}PCAPNG") -and
        (Find-Element "Capture change${dot}-4 packets${dot}-330 captured bytes${dot}+1 truncated") -and
        (Find-Element "After average${dot}Unavailable") -and
        (Find-Element 'IP conversation changes 3') -and
        (Find-Element "192.0.2.1 $bothDirections 192.0.2.2") -and
        (Find-Element "Change${dot}-3 packets${dot}-146 captured bytes")
    }
    $null = Wait-Until 'opening a compared conversation requires a selected row' {
        $open = Find-Element 'Open conversation in Wireshark' $button
        $open -and !$open.Current.IsEnabled
    }
    $changedPair = Wait-Until 'the changed IPv4 pair is selectable' {
        Find-Element "192.0.2.1 $bothDirections 192.0.2.2" ([System.Windows.Automation.ControlType]::ListItem)
    }
    $changedPair.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Select-Viewer -FromComparison
    Click 'Open conversation in Wireshark'
    $null = Wait-Until 'the default After file is launched with the compared pair filter' {
        $filterPath = $olderCapture + '.filter'
        [IO.File]::Exists($filterPath) -and [IO.File]::ReadAllText($filterPath) -ceq $ipv4Filter
    }
    Select-ComparisonCaptureSide 'Before capture'
    Remove-Item -LiteralPath ($conversationCapture + '.filter') -ErrorAction Stop
    Click 'Open conversation in Wireshark'
    $null = Wait-Until 'the same filter is sent to the original Before capture' {
        $filterPath = $conversationCapture + '.filter'
        [IO.File]::Exists($filterPath) -and [IO.File]::ReadAllText($filterPath) -ceq $ipv4Filter
    }
    Set-Field 'Filter conversations' '2001:0DB8::/32'
    $null = Wait-Until 'IPv6 network filtering narrows the view before exporting' {
        (Find-Element 'Matching conversations 1 of 3') -and
        (Find-Element "2001:db8::1 $bothDirections 2001:db8::2") -and
        !(Find-Element "192.0.2.1 $bothDirections 192.0.2.2")
    }
    $guiText = Join-Path $State 'packet changes.txt'
    Save-PacketComparison $guiText
    $null = Wait-Until 'the Text comparison export is published' { [IO.File]::Exists($guiText) }
    $textReport = [IO.File]::ReadAllText($guiText)
    if (!$textReport.Contains("Before${dot}IP conversations.pcap${dot}PCAP") -or
        !$textReport.Contains("After${dot}older capture.pcapng${dot}PCAPNG") -or
        !$textReport.Contains("IP conversation changes${dot}3") -or
        !$textReport.Contains("Change${dot}-3 packets${dot}-146 captured bytes") -or $textReport.Contains($State)) {
        throw 'The text export did not preserve the comparison or included a full source path'
    }
    $jsonBase = Join-Path $State 'packet changes structured'
    Save-PacketComparison $jsonBase -Json
    $guiJson = "$jsonBase.json"
    $null = Wait-Until 'the JSON export uses the selected file extension' { [IO.File]::Exists($guiJson) }
    foreach ($format in @('text', 'json')) {
        $guiReport = if ($format -ceq 'text') { $guiText } else { $guiJson }
        $cliReport = Join-Path $State "packet comparison from CLI.$format"
        $null = & $Cli compare-packet-captures $conversationCapture $olderCapture --format $format --output-file $cliReport
        if ($LASTEXITCODE -ne 0 -or [Convert]::ToBase64String([IO.File]::ReadAllBytes($cliReport)) -cne
            [Convert]::ToBase64String([IO.File]::ReadAllBytes($guiReport))) {
            throw "The $format export differs between the packet viewer and CLI"
        }
    }
    Click 'Back to capture'
    $null = Wait-Until 'comparison returns to the original before capture' {
        Test-CaptureFile $conversationCapture
    }
    Select-Capture $slowerCapture $compareCaptureLabel 'Choose after packet capture'
    $null = Wait-Until 'equal packet counts show slower rates across the longer capture window' {
        (Find-Element "Capture change${dot}+0 packets${dot}+0 captured bytes${dot}+0 truncated") -and
        (Find-Element "Before average${dot}1.167 packets/s, 57.000 captured bytes/s") -and
        (Find-Element "After average${dot}0.583 packets/s, 28.500 captured bytes/s") -and
        (Find-Element "Average change${dot}-0.583 packets/s, -28.500 captured bytes/s") -and
        (Find-Element 'IP conversation changes 3') -and
        (Find-Element "Average change${dot}-0.250 packets/s, -12.167 captured bytes/s")
    }
    $expander = Open-PacketSection 'comparison-packet-timeline'
    $null = Wait-Until 'the two elapsed timelines share a bucket width and selected interval' {
        (Find-Element 'Before packet timeline chart') -and
        (Find-Element 'After packet timeline chart') -and
        (Find-Element 'Interval width 0.262144 s') -and
        (Find-Element "Before interval${dot}1 packets${dot}34 captured bytes") -and
        (Find-Element "After interval${dot}1 packets${dot}34 captured bytes")
    }
    Click 'Open Before interval in Wireshark'
    $null = Wait-Until 'the elapsed interval translates to the Before file clock' {
        $filter = $conversationCapture + '.filter'
        [IO.File]::Exists($filter) -and [IO.File]::ReadAllText($filter) -ceq
            'frame.time >= "1970-01-01T00:00:01.000000000Z" && frame.time < "1970-01-01T00:00:01.262144000Z"'
    }
    Click 'Open After interval in Wireshark'
    $null = Wait-Until 'the same elapsed interval translates to the After file clock' {
        $filter = $slowerCapture + '.filter'
        [IO.File]::Exists($filter) -and [IO.File]::ReadAllText($filter) -ceq
            'frame.time >= "1970-01-01T00:00:02.000000000Z" && frame.time < "1970-01-01T00:00:02.262144000Z"'
    }
    $intervals = Wait-CaptureCombo 'Compared timeline intervals'
    $intervals.SetFocus()
    $intervals.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $empty = Wait-Until 'the empty elapsed interval is selectable' {
        Find-Element '+0.262144 to +0.524288 s' ([System.Windows.Automation.ControlType]::ListItem)
    }
    $empty.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'an empty interval has known zero counts and no Wireshark target' {
        $openBefore = Find-Element 'Open Before interval in Wireshark' $button
        $openAfter = Find-Element 'Open After interval in Wireshark' $button
        (Find-Element "Before interval${dot}0 packets${dot}0 captured bytes") -and
        (Find-Element "After interval${dot}0 packets${dot}0 captured bytes") -and
        $openBefore -and !$openBefore.Current.IsEnabled -and $openAfter -and !$openAfter.Current.IsEnabled
    }
    $expander.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Collapse()
    $row = Wait-Until 'a conversation can be selected with the timeline collapsed' {
        Find-Element "192.0.2.1 $bothDirections 192.0.2.2" ([System.Windows.Automation.ControlType]::ListItem)
    }
    $row.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $elapsedLabel = "Limit to elapsed interval${dot}+0.262144 to +0.524288 s"
    (Wait-CheckBox $elapsedLabel ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Wait-CheckBox $elapsedLabel ([System.Windows.Automation.ToggleState]::On)
    Click 'Open conversation in Wireshark'
    $null = Wait-Until 'the IP pair opens in the selected empty interval on the After clock' {
        $filterPath = $slowerCapture + '.filter'
        $expected = "($ipv4Filter) && " +
            '(frame.time >= "1970-01-01T00:00:02.262144000Z" && frame.time < "1970-01-01T00:00:02.524288000Z")'
        [IO.File]::Exists($filterPath) -and [IO.File]::ReadAllText($filterPath) -ceq $expected
    }
    Select-ComparisonCaptureSide 'Before capture'
    Click 'Open conversation in Wireshark'
    $null = Wait-Until 'the same IP pair and elapsed interval open on the Before clock' {
        $filterPath = $conversationCapture + '.filter'
        $expected = "($ipv4Filter) && " +
            '(frame.time >= "1970-01-01T00:00:01.262144000Z" && frame.time < "1970-01-01T00:00:01.524288000Z")'
        [IO.File]::Exists($filterPath) -and [IO.File]::ReadAllText($filterPath) -ceq $expected
    }
    $expander = Open-PacketSection 'comparison-packet-timeline'
    $intervals = Wait-CaptureCombo 'Compared timeline intervals'
    $intervals.SetFocus()
    $intervals.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $interval = Wait-Until 'an interval with one packet on the Before side is selectable' {
        Find-Element '+0.786432 to +1.048576 s' ([System.Windows.Automation.ControlType]::ListItem)
    }
    $interval.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'the timeline selected the requested interval' {
        Find-Element "Before interval${dot}1 packets${dot}38 captured bytes"
    }
    Click 'Compare selected interval'
    $null = Wait-Until 'the comparison counts only packets within the selected interval' {
        (Find-Element "Comparing elapsed interval${dot}+0.786432 to +1.048576 s") -and
        (Find-Element "Capture change${dot}-1 packets${dot}-38 captured bytes${dot}+0 truncated") -and
        (Find-Element 'Average rates over the selected elapsed interval') -and
        (Find-Element 'IP conversation changes 1')
    }
    $scopedBase = Join-Path $State 'elapsed comparison'
    Save-PacketComparison $scopedBase -Json
    $scopedPath = "$scopedBase.json"
    $null = Wait-Until 'the elapsed comparison export was published' { [IO.File]::Exists($scopedPath) }
    $cliScopedPath = Join-Path $State 'elapsed cli.json'
    $null = & $Cli compare-packet-captures $conversationCapture $slowerCapture --elapsed-ns '786432000:1048576000' --output-file $cliScopedPath
    if ($LASTEXITCODE -ne 0 -or [Convert]::ToBase64String([IO.File]::ReadAllBytes($scopedPath)) -cne
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($cliScopedPath))) {
        throw 'The elapsed comparison export differs from the CLI'
    }
    Click 'Compare whole captures'
    $null = Wait-Until 'the full comparison returns without choosing the files again' {
        (Find-Element 'IP conversation changes 3') -and !(Find-Element 'Compare whole captures' $button)
    }
    $ending = Wait-CaptureCombo 'Compared timeline range end'
    $ending.SetFocus()
    $ending.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $boundary = Wait-Until 'an end boundary several buckets later is selectable' {
        Find-Element '+3.145728 s' ([System.Windows.Automation.ControlType]::ListItem)
    }
    $boundary.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'the selected range sums all covered packets on both charts' {
        (Find-Element "Before interval${dot}3 packets${dot}106 captured bytes") -and
        (Find-Element "After interval${dot}1 packets${dot}38 captured bytes")
    }
    Click 'Open Before interval in Wireshark'
    $null = Wait-Until 'Wireshark receives the whole elapsed range on the Before clock' {
        $filterPath = $conversationCapture + '.filter'
        [IO.File]::ReadAllText($filterPath) -ceq
            'frame.time >= "1970-01-01T00:00:01.786432000Z" && frame.time < "1970-01-01T00:00:04.145728000Z"'
    }
    $beforeExcerpt = Join-Path $State 'before range.pcap'
    Save-CaptureExcerpt ('Save Before excerpt' + [char]0x2026) $beforeExcerpt
    $cliExcerpt = Join-Path $State 'before range cli.pcap'
    $null = & $Cli clip-packet-capture $conversationCapture --elapsed-ns '786432000:3145728000' --output-file $cliExcerpt
    if ($LASTEXITCODE -ne 0 -or [Convert]::ToBase64String([IO.File]::ReadAllBytes($beforeExcerpt)) -cne
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($cliExcerpt))) {
        throw 'The saved Before excerpt differs from the CLI'
    }
    Click 'Compare selected interval'
    $null = Wait-Until 'the report compares the complete selected range' {
        (Find-Element "Comparing elapsed interval${dot}+0.786432 to +3.145728 s") -and
        (Find-Element "Capture change${dot}-2 packets${dot}-68 captured bytes${dot}+0 truncated") -and
        (Find-Element 'IP conversation changes 1') -and
        (Find-Element "2001:db8::1 $bothDirections 2001:db8::2")
    }
    $row = Wait-Until 'the scoped IPv6 conversation can be selected for saving' {
        Find-Element "2001:db8::1 $bothDirections 2001:db8::2" ([System.Windows.Automation.ControlType]::ListItem)
    }
    $row.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $conversationExcerpt = Join-Path $State 'before scoped ipv6.pcap'
    Save-CaptureExcerpt ('Save conversation excerpt' + [char]0x2026) $conversationExcerpt
    $cliConversation = Join-Path $State 'before scoped ipv6 cli.pcap'
    $result = & $Cli clip-packet-capture $conversationCapture --ip-pair '2001:db8::1,2001:db8::2' --elapsed-ns '786432000:3145728000' --output-file $cliConversation | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $result.result.packets -ne 1 -or
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($conversationExcerpt)) -cne
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($cliConversation))) {
        throw 'The saved conversation excerpt differs from the scoped CLI selection'
    }
    $rangeBase = Join-Path $State 'elapsed range comparison'
    Save-PacketComparison $rangeBase -Json
    $rangePath = "$rangeBase.json"
    $null = Wait-Until 'the range export was published' { [IO.File]::Exists($rangePath) }
    $cliRangePath = Join-Path $State 'elapsed range cli.json'
    $null = & $Cli compare-packet-captures $conversationCapture $slowerCapture --elapsed-ns '786432000:3145728000' --output-file $cliRangePath
    if ($LASTEXITCODE -ne 0 -or [Convert]::ToBase64String([IO.File]::ReadAllBytes($rangePath)) -cne
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($cliRangePath))) {
        throw 'The elapsed range export differs from the CLI'
    }
    Click 'Compare whole captures'
    Click 'Back to capture'
    Select-Capture $partialCapture $compareCaptureLabel 'Choose after packet capture'
    $null = Wait-Until 'partial comparison preserves the issue and withholds capture deltas' {
        (Find-Element "After${dot}incomplete capture.pcapng${dot}PCAPNG") -and
        (Find-Element "Partial scan${dot}Incomplete or invalid PCAPNG block length") -and
        (Find-Element 'Capture deltas unavailable for a partial scan') -and
        (Find-Element "After average${dot}Unavailable") -and
        (Find-Element "After${dot}Pair coverage unavailable")
    }
    Click 'Back to capture'
    Select-Capture $protocolCapture
    $null = Wait-Until 'the standalone protocol fixture opens' { Test-CaptureFile $protocolCapture }
    $expander = Open-PacketSection 'packet-ip-protocols'
    $null = Wait-Until 'protocol totals and TCP flag coverage are visible' {
        (Find-Element "TCP${dot}5 packets${dot}200 captured bytes") -and
        (Find-Element "UDP${dot}1 packets${dot}28 captured bytes") -and
        (Find-Element "ICMPv6${dot}1 packets${dot}48 captured bytes") -and
        (Find-Element "Unclassified packets 1${dot}Unresolved IP protocols 1${dot}Fragmented IP packets 1") -and
        (Find-Element 'TCP flags available on 4 of 5 TCP packets') -and
        (Find-Element "SYN without ACK 1${dot}SYN-ACK 1${dot}RST 1${dot}FIN 1")
    }
    $summary = & $Cli inspect-packet-capture $protocolCapture | ConvertFrom-Json
    $protocols = $summary.ip_protocols
    if ($LASTEXITCODE -ne 0 -or !$summary.complete -or $protocols.by_number.'6'.packets -ne 5 -or
        $protocols.by_number.'17'.captured_bytes -ne 28 -or $protocols.by_number.'58'.captured_bytes -ne 48 -or
        $protocols.unclassified_packets -ne 1 -or $protocols.unresolved_protocol_packets -ne 1 -or
        $protocols.fragmented_packets -ne 1 -or $protocols.tcp_flags.decoded_headers -ne 4 -or
        $protocols.tcp_flags.syn_without_ack -ne 1 -or $protocols.tcp_flags.syn_ack -ne 1 -or
        $protocols.tcp_flags.rst -ne 1 -or $protocols.tcp_flags.fin -ne 1) {
        throw 'The CLI protocol report differs from the capture panel'
    }
    $expander.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Collapse()
    $expander = Open-PacketSection 'packet-transport-conversations'
    $null = Wait-Until 'TCP and UDP port pairs are displayed separately with directional totals' {
        (Find-Element "TCP${dot}192.0.2.1:49152 $bothDirections 192.0.2.2:443") -and
        (Find-Element "UDP${dot}192.0.2.1:49152 $bothDirections 192.0.2.2:443") -and
        (Find-Element "Tracked transport pairs 2${dot}TCP or UDP packets without ports 1") -and
        (Find-Element "Packets 4${dot}Captured bytes 160") -and
        (Find-Element "Left to right 3 packets, 120 bytes${dot}Right to left 1 packets, 40 bytes")
    }
    $transport = $summary.transport_conversations
    $tcp = $transport.top[0]
    $udp = $transport.top[1]
    if ($transport.tracked_pairs -ne 2 -or $transport.packets_without_ports -ne 1 -or
        $transport.untracked_packets -ne 0 -or $tcp.protocol -ne 6 -or $udp.protocol -ne 17 -or
        $tcp.a -cne '192.0.2.1:49152' -or $tcp.b -cne '192.0.2.2:443' -or
        $udp.a -cne $tcp.a -or $udp.b -cne $tcp.b -or
        $tcp.a_to_b.packets -ne 3 -or $tcp.a_to_b.captured_bytes -ne 120 -or
        $tcp.b_to_a.packets -ne 1 -or $tcp.b_to_a.captured_bytes -ne 40 -or
        $udp.a_to_b.captured_bytes -ne 28 -or $udp.b_to_a.packets -ne 0) {
        throw 'The CLI transport report differs from the capture panel'
    }
    $null = Wait-Until 'launching a transport pair requires a selection' {
        $open = Find-Element 'Open port pair in Wireshark' $button
        $open -and !$open.Current.IsEnabled
    }
    $transportFilterPath = $protocolCapture + '.filter'
    foreach ($kind in @('TCP', 'UDP')) {
        $protocolField = $kind.ToLowerInvariant()
        $expected = ('frame.protocols matches "^(?:(?!ip:|ipv6:)[^:]+:)*ip:{0}(?::|$)" && ' +
            '((ip.src#1 == 192.0.2.1 && {0}.srcport#1 == 49152 && ip.dst#1 == 192.0.2.2 && {0}.dstport#1 == 443) || ' +
            '(ip.src#1 == 192.0.2.2 && {0}.srcport#1 == 443 && ip.dst#1 == 192.0.2.1 && {0}.dstport#1 == 49152))') -f $protocolField
        if ($kind -ceq 'TCP') { $tcpPortFilter = $expected }
        $row = Wait-Until "the $kind port pair can be selected" {
            Find-Element "$kind${dot}192.0.2.1:49152 $bothDirections 192.0.2.2:443" ([System.Windows.Automation.ControlType]::ListItem)
        }
        $row.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
        Click 'Open port pair in Wireshark'
        $null = Wait-Until "Wireshark receives the original file and exact $kind port filter" {
            [IO.File]::Exists($transportFilterPath) -and [IO.File]::ReadAllText($transportFilterPath) -ceq $expected
        }
    }
    $scopeLabel = "Limit port pair to timeline interval${dot}+0.000 to +0.131072 s"
    (Wait-CheckBox $scopeLabel ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Wait-CheckBox $scopeLabel ([System.Windows.Automation.ToggleState]::On)
    Click 'Open port pair in Wireshark'
    $limited = "($expected) && " +
        '(frame.time >= "1970-01-01T00:00:00.917504000Z" && frame.time < "1970-01-01T00:00:01.048576000Z")'
    $null = Wait-Until 'the standalone UDP pair can open with the timeline interval' {
        [IO.File]::ReadAllText($transportFilterPath) -ceq $limited
    }
    (Wait-CheckBox $scopeLabel ([System.Windows.Automation.ToggleState]::On)).Toggle()
    $null = Wait-CheckBox $scopeLabel ([System.Windows.Automation.ToggleState]::Off)
    $udpExcerpt = Join-Path $State 'selected UDP pair.pcap'
    Save-CaptureExcerpt ('Save port pair excerpt' + [char]0x2026) $udpExcerpt
    $report = & $Cli inspect-packet-capture $udpExcerpt | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or !$report.complete -or $report.packets -ne 1 -or
        $report.transport_conversations.top[0].protocol -ne 17 -or
        $report.transport_conversations.top[0].a -cne '192.0.2.1:49152') {
        throw 'The standalone excerpt did not preserve just the selected UDP pair'
    }
    $expander.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Collapse()
    Select-Capture $olderCapture $compareCaptureLabel 'Choose after packet capture'
    $expander = Open-PacketSection 'comparison-ip-protocols'
    $right = [char]0x2192
    $null = Wait-Until 'the comparison shows observed protocol changes and incomplete TCP header coverage' {
        (Find-Element "TCP${dot}5 $right 0 packets${dot}200 $right 0 captured bytes${dot}Change -5 packets, -200 bytes") -and
        (Find-Element "Unresolved IP protocols 1${dot}Fragmented IP packets 1") -and
        (Find-Element "TCP headers decoded${dot}Before 4 of 5${dot}After 0 of 0") -and
        (Find-Element "RST${dot}Before 1${dot}After 0") -and
        (Find-Element 'TCP flag deltas unavailable for this coverage')
    }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $bounds = $root.Current.BoundingRectangle
    $resize = $root.GetCurrentPattern([System.Windows.Automation.TransformPattern]::Pattern)
    $scale = [FixtureMessages]::GetDpiForWindow($window) / 96.0
    $resize.Resize($bounds.Width, [Math]::Round(280 * $scale))
    $scroll = Wait-Until 'the comparison summary can scroll' {
        $combo = Find-Element 'Conversation type' ([System.Windows.Automation.ControlType]::ComboBox)
        if ($combo) { Find-NativeVerticalScroll $combo }
    }
    $scroll.SetScrollPercent(-1, 100)
    Select-ComparisonConversationType 'TCP and UDP ports'
    $null = Wait-Until 'the transport comparison reports both pairs and their coverage' {
        (Find-Element 'TCP and UDP conversation changes 2') -and
        (Find-Element "TCP/UDP packets without ports 1${dot}Untracked TCP/UDP packets 0") -and
        (Find-Element "Before${dot}Left to right 3 packets, 120 bytes${dot}Right to left 1 packets, 40 bytes") -and
        (Find-Element "Change${dot}Unavailable")
    }
    $resize.Resize($bounds.Width, $bounds.Height)
    $row = Wait-Until 'the compared TCP pair can be selected' {
        Find-Element "TCP${dot}192.0.2.1:49152 $bothDirections 192.0.2.2:443" ([System.Windows.Automation.ControlType]::ListItem)
    }
    $row.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Click 'Open conversation in Wireshark'
    $null = Wait-Until 'the After capture opens with the exact TCP pair filter' {
        $filterPath = $olderCapture + '.filter'
        [IO.File]::Exists($filterPath) -and [IO.File]::ReadAllText($filterPath) -ceq $tcpPortFilter
    }
    Select-ComparisonCaptureSide 'Before capture'
    Click 'Open conversation in Wireshark'
    $null = Wait-Until 'the Before capture opens with the same TCP pair filter' {
        [IO.File]::Exists($transportFilterPath) -and [IO.File]::ReadAllText($transportFilterPath) -ceq $tcpPortFilter
    }
    Click 'Back to capture'
    Select-Capture $manyIpCapture
    $null = Open-PacketSection 'packet-ip-conversations'
    $null = Wait-Until 'standalone IP results have more than one page' {
        (Find-Element 'Page 1 of 2') -and (Find-Element 'Next IP page' $button)
    }
    Set-Field 'Filter IP conversations' '192.0.2.16/32'
    $row = Wait-Until 'standalone IP filtering finds a pair outside the top ten' {
        if (Find-Element 'Matching IP pairs 1 of 15') {
            Find-Element "192.0.2.1 $bothDirections 192.0.2.16" ([System.Windows.Automation.ControlType]::ListItem)
        }
    }
    $conversationMatches = & $Cli inspect-packet-capture $manyIpCapture --filter-conversations '192.0.2.16/32' | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $conversationMatches.conversation_results.ip.Count -ne 1 -or
        $conversationMatches.conversation_results.ip[0].b -cne '192.0.2.16') {
        throw 'The CLI and standalone IP search differ'
    }
    $row.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $quietPair = Join-Path $State 'quiet IP pair.pcap'
    Save-CaptureExcerpt ('Save conversation excerpt' + [char]0x2026) $quietPair
    $report = & $Cli inspect-packet-capture $quietPair | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $report.packets -ne 1 -or $report.ip_conversations.top[0].b -cne '192.0.2.16') {
        throw 'The selected lower-ranked IP pair did not export correctly'
    }
    Click 'Clear IP filter'
    $null = Wait-Until 'clearing the standalone IP filter restores page one' {
        (Find-Element 'Page 1 of 2') -and !(Find-Element 'Matching IP pairs 1 of 15')
    }
    Select-Capture $manyPortsCapture
    $null = Open-PacketSection 'packet-transport-conversations'
    Click 'Next port pair page'
    $row = Wait-Until 'the second standalone port page displays lower-ranked results' {
        if (Find-Element 'Page 2 of 2') {
            Find-Element "UDP${dot}192.0.2.1:50011 $bothDirections 192.0.2.2:443" ([System.Windows.Automation.ControlType]::ListItem)
        }
    }
    $row.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Set-Field 'Filter port pairs' 'UDP 192.0.2.0/24 :50015'
    $row = Wait-Until 'a standalone port filter clears the old selection and searches every page' {
        $open = Find-Element 'Open port pair in Wireshark' $button
        if ($open -and !$open.Current.IsEnabled -and (Find-Element 'Matching port pairs 1 of 15')) {
            Find-Element "UDP${dot}192.0.2.1:50015 $bothDirections 192.0.2.2:443" ([System.Windows.Automation.ControlType]::ListItem)
        }
    }
    $conversationMatches = & $Cli inspect-packet-capture $manyPortsCapture --filter-conversations 'UDP 192.0.2.0/24 :50015' | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or $conversationMatches.conversation_results.transport.Count -ne 1 -or
        $conversationMatches.conversation_results.transport[0].a -cne '192.0.2.1:50015' -or
        $conversationMatches.conversation_results.ip.Count -ne 0) {
        throw 'The CLI and standalone port search differ'
    }
    $row.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Click 'Open port pair in Wireshark'
    $expected = Udp-PortFilter 50015
    $null = Wait-Until 'Wireshark receives the lower-ranked standalone port pair' {
        $filterPath = $manyPortsCapture + '.filter'
        [IO.File]::Exists($filterPath) -and [IO.File]::ReadAllText($filterPath) -ceq $expected
    }
    Set-Field 'Filter port pairs' 'tcp 50015'
    $null = Wait-Until 'the standalone port view reports an empty filter result' {
        Find-Element 'No port pairs match the filter'
    }
    Click 'Clear port pair filter'
    $null = Wait-Until 'clearing the standalone port filter restores page one' {
        (Find-Element 'Page 1 of 2') -and !(Find-Element 'Clear port pair filter' $button)
    }
    Select-Capture $olderCapture $compareCaptureLabel 'Choose after packet capture'
    Select-ComparisonConversationType 'TCP and UDP ports'
    $null = Wait-Until 'the unfiltered transport comparison spans two pages' {
        (Find-Element 'TCP and UDP conversation changes 15') -and (Find-Element 'Page 1 of 2')
    }
    Set-Field 'Filter conversations' 'UDP 192.0.2.0/24 :50015'
    $row = Wait-Until 'the filter finds a pair that was off the first page' {
        if ((Find-Element 'Matching conversations 1 of 15') -and !(Find-Element 'Page 1 of 2')) {
            Find-Element "UDP${dot}192.0.2.1:50015 $bothDirections 192.0.2.2:443" ([System.Windows.Automation.ControlType]::ListItem)
        }
    }
    $row.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Click 'Open conversation in Wireshark'
    $expected = Udp-PortFilter 50015
    $null = Wait-Until 'Wireshark opens the filtered transport pair' {
        $filterPath = $olderCapture + '.filter'
        [IO.File]::Exists($filterPath) -and [IO.File]::ReadAllText($filterPath) -ceq $expected
    }
    $elapsedLabel = "Limit to elapsed interval${dot}+0.000 to +0.262144 s"
    (Wait-CheckBox $elapsedLabel ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Wait-CheckBox $elapsedLabel ([System.Windows.Automation.ToggleState]::On)
    Click 'Open conversation in Wireshark'
    $limited = "($expected) && " +
        '(frame.time >= "1970-01-01T00:00:01.000000000Z" && frame.time < "1970-01-01T00:00:01.262144000Z")'
    $null = Wait-Until 'Wireshark receives the filtered UDP pair and its elapsed bounds' {
        [IO.File]::ReadAllText($olderCapture + '.filter') -ceq $limited
    }
    Set-Field 'Filter conversations' 'udp 50014'
    $null = Wait-Until 'a new filter clears selection even when it finds one row again' {
        $open = Find-Element 'Open conversation in Wireshark' $button
        (Find-Element "UDP${dot}192.0.2.1:50014 $bothDirections 192.0.2.2:443") -and $open -and !$open.Current.IsEnabled
    }
    Set-Field 'Filter conversations' 'tcp 50014'
    $null = Wait-Until 'an unmatched filter explains the empty list' {
        (Find-Element 'Matching conversations 0 of 15') -and (Find-Element 'No conversations match the filter')
    }
    Click 'Clear conversation filter'
    $null = Wait-Until 'clearing the filter restores the first page' {
        (Find-Element 'Page 1 of 2') -and !(Find-Element 'Clear conversation filter' $button)
    }
    Click 'Back to capture'
    Select-Capture $emptyCapture $compareCaptureLabel 'Choose after packet capture'
    $row = Wait-Until 'an IP pair is selectable with an untimed After capture' {
        Find-Element "192.0.2.1 $bothDirections 192.0.2.2" ([System.Windows.Automation.ControlType]::ListItem)
    }
    $row.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    (Wait-CheckBox $elapsedLabel ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Wait-Until 'elapsed filtering is unavailable for a capture without a timestamp origin' {
        $open = Find-Element 'Open conversation in Wireshark' $button
        (Find-Element 'The selected capture has no timestamped records to align the interval.') -and
            $open -and !$open.Current.IsEnabled -and ![IO.File]::Exists($emptyCapture + '.filter')
    }
    (Wait-CheckBox $elapsedLabel ([System.Windows.Automation.ToggleState]::On)).Toggle()
    Click 'Open conversation in Wireshark'
    $null = Wait-Until 'clearing the interval allows the whole untimed capture to open again' {
        $filterPath = $emptyCapture + '.filter'
        [IO.File]::Exists($filterPath) -and [IO.File]::ReadAllText($filterPath) -ceq $ipv4Filter
    }
    Click 'Back to capture'
    Click 'Return to diagnostics overview'
    Click 'CPU flame graph'
    $null = Wait-Until 'flame graph can open a saved artifact offline' {
        $openStacks = Find-Element 'Open folded stacks' $button
        $captureSsh = Find-Element 'Capture from SSH' $button
        $openStacks -and $openStacks.Current.IsEnabled -and $captureSsh -and !$captureSsh.Current.IsEnabled -and
            !(Find-Element 'Diagnostics overview panel')
    }
    Click 'Return to diagnostics overview'
    $null = Wait-Until 'returning from the flame graph restores the overview' { Find-Element 'Diagnostics overview panel' }
    Click 'Syscall trace viewer'
    $null = Wait-Until 'opening the offline syscall viewer dismisses the overview' {
        (Find-Element ('Open syscall trace' + [char]0x2026) $button) -and !(Find-Element 'Diagnostics overview panel')
    }
    $null = Control @('focus-pane','--pane',"$local")
    Wait-Focus $local
    Run-Palette 'Diagnostics overview'
    $remote = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $remote
    $null = Control @('wait-ssh-connected','--pane',"$remote",'--timeout','15')
    $sshProfile = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.'palette-ssh'
    $null = Wait-Until 'overview updates to the authenticated SSH endpoint' {
        Find-Element "SSH pane${dot}$($sshProfile.user)@$($sshProfile.host):$($sshProfile.port)"
    }
    Click 'Linux performance triage'
    $null = Wait-Until 'triage offers a separately reviewed run on the selected pane' {
        (Find-Element 'Review selected SSH pane' $button) -and !(Find-Element 'Diagnostics overview panel')
    }
    Click 'Return to diagnostics overview'
    Click 'Packet capture'
    $catalog = Control @('list-packet-interfaces','--pane',"$remote")
    if (($catalog.interfaces.name -join ',') -cne 'eth0,any,lo' -or $catalog.truncated) {
        throw 'The CLI interface list did not match the reviewed SSH pane'
    }
    Click 'List capture interfaces'
    $combo = [System.Windows.Automation.ControlType]::ComboBox
    $null = Wait-Until 'capture interface picker loaded' { Find-Element 'Capture interfaces' $combo }
    if ([IO.File]::Exists($startMarker)) { throw 'Interface discovery started a capture' }
    $null = Control @('focus-pane','--pane',"$local")
    Wait-Focus $local
    $null = Wait-Until 'the SSH interface list is unavailable from PowerShell' {
        $listButton = Find-Element 'List capture interfaces' $button
        $listButton -and !$listButton.Current.IsEnabled -and !(Find-Element 'Capture interfaces' $combo)
    }
    $null = Control @('focus-pane','--pane',"$remote")
    Wait-Focus $remote
    $dropdown = Wait-CaptureCombo 'Capture interfaces'
    $dropdown.SetFocus()
    $dropdown.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $loopback = Wait-Until 'tcpdump loopback device can be chosen' {
        Find-Element "lo${dot}[Up, Running, Loopback]" ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $loopback.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'selecting a device fills the editable interface field' {
        $field = Find-Element 'Linux network interface' $edit
        $field -and $field.GetCurrentPattern($value).Current.Value -ceq 'lo'
    }
    Set-Field 'Maximum capture size in MiB' '1'
    Set-Field 'Packet limit (optional)' '1'
    $ram = Wait-Until 'RAM scratch option' { Find-Element 'Use RAM-backed scratch storage' $check }
    $ram.GetCurrentPattern($toggle).Toggle()
    $null = Wait-Until 'RAM mode selects the default tmpfs path' { Find-Element 'Default scratch directory /dev/shm' }
    Click 'Review packet capture'
    $null = Wait-Until 'verified tmpfs and packet budget shown' {
        (Find-Element '/dev/shm (tmpfs)') -and (Find-Element "15 seconds${dot}1 MiB${dot}Packet ceiling 1")
    }
    if ([IO.File]::Exists($startMarker)) { throw 'Review started a packet capture' }

    Set-Field 'Capture filter (optional)' 'udp and dst port 4321'
    $null = Wait-Until 'editing options invalidates the earlier review' { !(Find-Element $saveLabel $button) }
    Set-Field 'Packet limit (optional)' '3'
    Set-Field 'Rolling capture files (optional)' '2'
    Click 'Review packet capture'
    $null = Wait-Until 'BPF filter and rolling limits are visible in the review' {
        (Find-Element 'tcpdump ["-i","lo","-nn","-p","-s","192","-c","3","-w","-","--","udp and dst port 4321"]') -and
            (Find-Element "Keep up to 2 files${dot}524288 bytes per file. The oldest file is replaced when the buffer fills.")
    }
    $null = Control @('focus-pane','--pane',"$local")
    Wait-Focus $local
    $null = Wait-Until 'the other pane cannot start the reviewed capture' {
        $save = Find-Element $saveLabel $button
        $save -and !$save.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$remote")
    Wait-Focus $remote
    Click $saveLabel
    $dialog = Wait-Until 'owned capture save dialog' { Find-OwnedDialog 'Save packet capture' }
    if ([IO.File]::Exists($startMarker)) { throw 'Capture started before a destination was chosen' }
    $null = Wait-Until 'enter the capture path' { [FixtureDialog]::SetFilename($dialog, $destination, 'packet-capture.pcap') }
    $null = Wait-Until 'save the capture path' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'tcpdump is running on the reviewed SSH pane' { [IO.File]::Exists($startMarker) }
    Click 'Return to diagnostics overview'
    $null = Control @('focus-pane','--pane',"$local")
    Wait-Focus $local
    $null = Wait-Until 'overview follows the active local pane while the capture runs' {
        (Find-Element 'Diagnostics overview panel') -and
            (Find-Element 'Select a connected Linux SSH pane to run diagnostics.') -and
            (Find-Element 'Open Packet capture activity' $button) -and
            (Find-Element "$($sshProfile.user)@$($sshProfile.host):$($sshProfile.port)") -and
            (Find-Element 'Running')
    }
    Click 'Open Packet capture activity'
    Click 'Stop and save'
    $null = Wait-Until 'verified capture published on Windows' {
        (Find-Element 'Saved packet capture') -and (Find-Element 'Packets dropped by kernel 0') -and
            (Find-Element "Stopped by request`nPackets retained 2${dot}64 bytes") -and
            (Find-Element "Rolling capture${dot}3 captured${dot}2 files kept${dot}1 overwritten") -and
            [IO.File]::Exists($destination)
    }

    $null = Control @('close-pane','--pane',"$remote")
    Wait-Focus $local
    Click 'Return to diagnostics overview'
    Wait-DiagnosticArtifact 'Saved packet capture' $destination 'Stopped by request'
    $null = Wait-Until 'saved packet capture appears in recent files' {
        Find-Element ('Reopen ' + [IO.Path]::GetFileName($destination)) $button
    }
    Click 'Open Packet capture activity'
    Select-Viewer
    if ([IO.File]::Exists($destination + '.opened')) { throw 'Selecting a viewer opened it without activation' }
    Click 'Open in Wireshark'
    $null = Wait-Until 'viewer receives the saved path and -r flag' {
        $opened = $destination + '.opened'
        [IO.File]::Exists($opened) -and [IO.File]::ReadAllText($opened) -ceq '-r'
    }
} finally {
    Close-FixtureApp $app
}
