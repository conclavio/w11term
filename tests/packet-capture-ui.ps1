param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$destination = Join-Path $State 'reviewed capture.pcap'
$viewer = Join-Path $State 'Fixture Wireshark.exe'
$startMarker = [IO.File]::ReadAllText((Join-Path $State 'pcap-start-marker'))
$saveLabel = 'Capture and save' + [char]0x2026
$chooseViewerLabel = 'Choose Wireshark executable' + [char]0x2026
$edit = [System.Windows.Automation.ControlType]::Edit
$check = [System.Windows.Automation.ControlType]::CheckBox
$value = [System.Windows.Automation.ValuePattern]::Pattern
$toggle = [System.Windows.Automation.TogglePattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '
Add-Type -OutputType WindowsApplication -OutputAssembly $viewer -TypeDefinition @'
using System.IO;
public static class Viewer {
    public static void Main(string[] args) {
        if (args.Length == 2 && args[0] == "-r" && File.Exists(args[1])) {
            File.WriteAllText(args[1] + ".opened", args[0]);
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

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $local = (Sessions | Where-Object label -eq 'Palette PowerShell').id
    $remote = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $remote
    $null = Control @('wait-ssh-connected','--pane',"$remote",'--timeout','15')
    Run-Palette 'Packet capture'
    Set-Field 'Linux network interface' 'lo'
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
    Click 'Review packet capture'
    $null = Wait-Until 'BPF filter is visible in the reviewed command' {
        Find-Element 'tcpdump ["-i","lo","-nn","-p","-s","192","-c","1","-w","-","--","udp and dst port 4321"]'
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
    $null = Control @('focus-pane','--pane',"$local")
    Wait-Focus $local
    Click 'Stop and save'
    $null = Wait-Until 'verified capture published on Windows' {
        (Find-Element 'Saved packet capture') -and (Find-Element 'Packets dropped by kernel 0') -and
            (Find-Element "Stopped by request`nPackets captured 1${dot}44 bytes") -and
            [IO.File]::Exists($destination)
    }

    $null = Control @('close-pane','--pane',"$remote")
    Wait-Focus $local
    Click $chooseViewerLabel
    $dialog = Wait-Until 'owned executable picker' { Find-OwnedDialog 'Choose Wireshark executable' }
    $null = Wait-Until 'enter the fixture viewer path' { [FixtureDialog]::SetFilename($dialog, $viewer, '') }
    $null = Wait-Until 'select the fixture viewer' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'viewer picker closed' { !(Find-OwnedDialog 'Choose Wireshark executable') }
    if ([IO.File]::Exists($destination + '.opened')) { throw 'Selecting a viewer opened it without activation' }
    Click 'Open in Wireshark'
    $null = Wait-Until 'viewer receives the saved path and -r flag' {
        $opened = $destination + '.opened'
        [IO.File]::Exists($opened) -and [IO.File]::ReadAllText($opened) -ceq '-r'
    }
} finally {
    Close-FixtureApp $app
}
