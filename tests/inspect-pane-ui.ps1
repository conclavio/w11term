[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$esc = [char]27
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class InspectDpi {
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
}
'@

function Inspect { Control @('inspect','--pane',"$remote") }
function Set-RemoteModes([string]$Bytes) {
    $data = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($Bytes))
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64',$data)
}
function Type-Line([string]$Line) {
    foreach ($ch in $Line.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    Press-Key 13
}
function Is-Default($Mode) {
    foreach ($name in 'alternate_screen','bracketed_paste','synchronized_output','in_band_resize') {
        if ($Mode.$name -isnot [bool] -or $Mode.$name) { return $false }
    }
    foreach ($name in 'click','drag','motion') {
        if ($Mode.mouse.$name -isnot [bool] -or $Mode.mouse.$name) { return $false }
    }
    return $Mode.mouse.encoding -ceq 'x10' -and $Mode.keyboard.encoding -ceq 'xterm' -and $Mode.keyboard.flags -eq 0
}

$wire = Open-PeerTraceReader ([IO.File]::ReadAllText((Join-Path $State 'ssh-wire')))
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','132','--rows','32') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    $ssh = (Control @('list')).sessions | Where-Object id -eq $remote
    $initial = Wait-Until 'default parser state and final window geometry' {
        $m = Inspect
        if ((Is-Default $m) -and $m.pane -eq $remote -and $m.connection_generation -eq $ssh.connection_generation -and
            $m.cols -eq $ssh.cols -and $m.rows -eq $ssh.rows -and $m.pixel_width -gt $m.cols -and
            $m.pixel_height -gt $m.rows -and $m.dpi -eq [InspectDpi]::GetDpiForWindow($window)) { $m }
    }
    Invoke-Button (Wait-Until 'menu' { Find-Element 'Tab menu' $button })
    Invoke-Button (Wait-Until 'open inspector' { Find-Element 'Terminal inspector: Off' })
    $area = "Text area   $($initial.pixel_width) $([char]0xd7) $($initial.pixel_height) px at $($initial.dpi) DPI"
    $null = Wait-Until 'inspector geometry and resize mode match the CLI' { (Find-Element $area) -and (Find-Element 'In-band resize   Off') }
    Set-RemoteModes "$esc[?2048h"
    $enabled = Wait-Until 'inspect and GUI show negotiated resize notifications' {
        $m = Inspect
        if ($m.in_band_resize -and (Find-Element 'In-band resize   On')) { $m }
    }
    $report = "$esc[48;$($enabled.rows);$($enabled.cols);$($enabled.pixel_height);$($enabled.pixel_width)t"
    $null = Wait-Until 'actual SSH resize report agrees with CLI cell and pixel geometry' {
        [Text.Encoding]::ASCII.GetString((Read-PeerTrace $wire)).Contains($report)
    }
    Set-RemoteModes "$esc[?2048l"
    $null = Wait-Until 'inspector observes resize reporting disabled' { Find-Element 'In-band resize   Off' }
    Invoke-Button (Wait-Until 'close inspector' { Find-Element 'Close terminal inspector' $button })
    $null = Wait-Until 'GUI inspection closed before inspecting the hidden tab' { !(Find-Element 'Close terminal inspector') }

    $localPane = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','20','PS ')
    Type-Line '$inspectPid = $PID; "local-inspect-" + "ready"'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','local-inspect-ready')
    $before = (Control @('get-text','--pane',"$remote")).text
    $foreground = [FixtureMessages]::GetForegroundWindow()
    Set-RemoteModes "$esc[?1000;1002;1003;1016;2004;2048h$esc[>5u"
    $null = Wait-Until 'hidden SSH parser modes without character output' {
        $m = Inspect
        $m.pane -eq $remote -and $m.connection_generation -eq $ssh.connection_generation -and
            !$m.alternate_screen -and !$m.synchronized_output -and $m.bracketed_paste -and $m.in_band_resize -and
            $m.mouse.click -and $m.mouse.drag -and $m.mouse.motion -and $m.mouse.encoding -ceq 'sgr-pixels' -and
            $m.keyboard.encoding -ceq 'kitty' -and $m.keyboard.flags -eq 5
    }
    if ((Control @('get-text','--pane',"$remote")).text -cne $before) { throw 'Mode-only transitions changed the SSH screen' }
    Set-RemoteModes "$esc[?1049h"
    $null = Wait-Until 'inspect observes the hidden alternate screen' { (Inspect).alternate_screen }
    Set-RemoteModes "$esc[?1049l$esc[?1000;1002;1003;1016;2004;2048l$esc[<u"
    $null = Wait-Until 'all hidden SSH flags and encodings return to their defaults' { Is-Default (Inspect) }
    if ((Control @('get-text','--pane',"$remote")).text -cne $before) { throw 'The original primary screen was lost' }
    if ($foreground -ne $window -and [FixtureMessages]::GetForegroundWindow() -eq $window) { throw 'Inspect activated the fixture window' }
    $sessions = @((Control @('list')).sessions)
    if ($sessions.Count -ne 2 -or ($sessions | Where-Object id -eq $remote).visible -or ($sessions | Where-Object active).id -ne $localPane) {
        throw 'Inspect changed the active pane, tab visibility, or open sessions'
    }
    Type-Line 'if ($inspectPid -eq $PID) { "local-inspect-" + "still-live" }'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','local-inspect-still-live')
    if ([Text.Encoding]::ASCII.GetString((Read-PeerTrace $wire)).Contains('local-inspect')) { throw 'PowerShell input landed in SSH' }
} finally {
    $wire.Dispose()
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
