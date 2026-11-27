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
    Invoke-Button (Get-NativeMenuAction 'Tab menu' 'Diagnostics overview' -Submenu 'Diagnostics')
    $null = Wait-Until 'overview shown alongside local PowerShell' {
        (Find-Element 'Diagnostics overview panel') -and
            (Find-Element 'Select a connected Linux SSH pane to run diagnostics.')
    }
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
    $dropdown = Wait-Until 'returning to the same SSH connection restores the picker' { Find-Element 'Capture interfaces' $combo }
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
    $null = Wait-Until 'offline activity keeps the stop reason and local artifact' {
        Find-Element ('Saved packet capture' + [char]10 + 'Stopped by request' + [char]10 + $destination)
    }
    Click 'Open Packet capture activity'
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
