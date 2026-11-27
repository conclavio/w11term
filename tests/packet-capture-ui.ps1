param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$destination = Join-Path $State 'reviewed capture.pcap'
$olderCapture = Join-Path $State 'older capture.pcapng'
$partialCapture = Join-Path $State 'incomplete capture.pcapng'
$conversationCapture = Join-Path $State 'IP conversations.pcap'
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
$writer = [IO.BinaryWriter]::new([IO.File]::Create($conversationCapture))
try {
    $writer.Write([byte[]]@(212,195,178,161, 2,0,4,0, 0,0,0,0, 0,0,0,0, 128,0,0,0, 1,0,0,0))
    $number = 0
    foreach ($frame in @(
        ,([byte[]]($eth4 + $ipv4))
        ,([byte[]]($eth4 + $reverse))
        ,([byte[]](@(0) * 12 + @(8,6)))
        ,([byte[]]($eth6 + $ipv6))
        ,([byte[]]($eth4 + $ipv4Tunnel + $ipv6))
        ,([byte[]]($eth6 + $ipv6Tunnel + $ipv4))
        ,([byte[]]($eth4 + $sameVersionTunnel + $ipv4))
    )) {
        $number++
        $writer.Write([uint32]$number)
        $writer.Write([uint32]0)
        $writer.Write([uint32]$frame.Length)
        $writer.Write([uint32]$frame.Length)
        $writer.Write($frame)
    }
} finally {
    $writer.Dispose()
}
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

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
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

function Select-Viewer {
    Click $chooseViewerLabel
    $dialog = Wait-Until 'owned executable picker' { Find-OwnedDialog 'Choose Wireshark executable' }
    $null = Wait-Until 'enter the fixture viewer path' { [FixtureDialog]::SetFilename($dialog, $viewer, '') }
    $null = Wait-Until 'select the fixture viewer' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'viewer picker closed' { !(Find-OwnedDialog 'Choose Wireshark executable') }
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
    Wait-Until "enabled capture dropdown '$Name'" {
        $item = Find-Element $Name ([System.Windows.Automation.ControlType]::ComboBox)
        if ($item -and $item.Current.IsEnabled) { $item }
    }
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
        Find-Element $conversationCapture
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
        (Find-Element 'IP conversation changes 3') -and
        (Find-Element "192.0.2.1 $bothDirections 192.0.2.2") -and
        (Find-Element "Change${dot}-3 packets${dot}-146 captured bytes")
    }
    Click 'Back to capture'
    $null = Wait-Until 'comparison returns to the original before capture' {
        Find-Element $conversationCapture
    }
    Select-Capture $partialCapture $compareCaptureLabel 'Choose after packet capture'
    $null = Wait-Until 'partial comparison preserves the issue and withholds capture deltas' {
        (Find-Element "After${dot}incomplete capture.pcapng${dot}PCAPNG") -and
        (Find-Element "Partial scan${dot}Incomplete or invalid PCAPNG block length") -and
        (Find-Element 'Capture deltas unavailable for a partial scan') -and
        (Find-Element "After${dot}Pair coverage unavailable")
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
