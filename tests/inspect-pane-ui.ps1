[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$peer = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.'palette-ssh'
$button = [System.Windows.Automation.ControlType]::Button
$esc = [char]27
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class InspectDpi {
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr hwnd, int command);
    [DllImport("user32.dll")] static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern IntPtr GetWindowDpiAwarenessContext(IntPtr hwnd);
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    public static uint WindowDpi(IntPtr hwnd) {
        var previous = SetThreadDpiAwarenessContext(GetWindowDpiAwarenessContext(hwnd));
        try { return GetDpiForWindow(hwnd); }
        finally { if (previous != IntPtr.Zero) SetThreadDpiAwarenessContext(previous); }
    }
}
'@

$observed = [ordered]@{}
function Inspect {
    $observed.inspect = Control @('inspect','--pane',"$remote")
    return $observed.inspect
}
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
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','80','--rows','24') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    if (![InspectDpi]::IsWindowVisible($window) -or [InspectDpi]::IsIconic($window)) {
        # The first canvas paint replaces provisional startup dimensions with the measured DPI.
        $null = [InspectDpi]::ShowWindowAsync($window, 4) # SW_SHOWNOACTIVATE
    }
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = Wait-Until 'the SSH pane to appear' { ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id }
    Invoke-NativeMenuAction 'Tab menu' 'Terminal inspector: Off'
    $null = Wait-Until 'inspector and CLI show the SSH step while the server withholds the shell' {
        $m = Inspect
        $m.ssh_connection.phase -ceq 'connecting' -and $m.ssh_progress.step -ceq 'requesting-terminal' -and
            $m.ssh_progress.host -ceq $peer.host -and $m.ssh_progress.port -eq $peer.port -and
            !$m.ssh -and (Find-Element 'SSH connection   Connecting') -and (Find-Element 'Requesting PTY and shell')
    }
    $hold = [IO.File]::ReadAllText((Join-Path $State 'hold-shell-request'))
    [IO.File]::Delete($hold)
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    $null = Wait-Until 'default SSH parser modes' { Is-Default (Inspect) }
    $initial = Wait-Until 'measured geometry matching the live pane list and window DPI' {
        # Startup may clamp the requested grid to this desktop's work area.
        $observed.list = (Control @('list')).sessions | Where-Object id -eq $remote
        $m = Inspect
        $observed.window_dpi = [InspectDpi]::WindowDpi($window)
        if ($m.pane -eq $remote -and $m.connection_generation -eq $observed.list.connection_generation -and
            $m.cols -eq $observed.list.cols -and $m.rows -eq $observed.list.rows -and $m.cols -gt 0 -and $m.rows -gt 0 -and
            $m.pixel_width -gt $m.cols -and $m.pixel_height -gt $m.rows -and $m.dpi -gt 0 -and $m.dpi -eq $observed.window_dpi) { $m }
    }
    $null = Wait-Until 'the connected inspector replaces progress with the verified state' {
        $m = Inspect
        $m.ssh_connection.phase -ceq 'connected' -and !$m.ssh_progress -and (Find-Element 'SSH connection   Connected')
    }
    Set-NativeExpansion 'Verified SSH route' $true
    $null = Wait-Until 'the verified SSH key and destination match the control reply' {
        $route = (Inspect).ssh
        $destination = $route.destination
        $algorithms = $destination.initial_algorithms
        $strictKex = if ($algorithms.strict_key_exchange) { 'On' } else { 'Off' }
        $destination.host -ceq $peer.host -and $destination.user -ceq $peer.user -and
            $destination.port -eq $peer.port -and $destination.key_fingerprint -ceq $peer.host_key -and
            $algorithms.host_key -ceq 'ssh-ed25519' -and $algorithms.cipher -and $algorithms.key_exchange -and
            $algorithms.strict_key_exchange -is [bool] -and
            @($route.jump_hosts).Count -eq 0 -and
            (Find-Element "Destination   $($peer.user)@$($peer.host):$($peer.port)") -and
            (Find-Element "Host key   $($peer.host_key)") -and
            (Find-Element "Key exchange   $($algorithms.key_exchange)") -and
            (Find-Element "Host-key algorithm   ssh-ed25519") -and
            (Find-Element "Cipher   $($algorithms.cipher)") -and
            (Find-Element "Strict key exchange   $strictKex")
    }
    Set-NativeExpansion 'Verified SSH route' $false
    $null = Wait-Until 'inspector geometry and resize mode match the CLI' {
        $m = Inspect
        $area = "Text area   $($m.pixel_width) $([char]0xd7) $($m.pixel_height) px at $($m.dpi) DPI"
        (Find-Element $area) -and !$m.in_band_resize -and (Find-Element 'In-band resize   Off')
    }
    Set-RemoteModes "$esc[?2048h"
    $null = Wait-Until 'SSH resize report matches inspector and CLI geometry' {
        $m = Inspect
        $report = "$esc[48;$($m.rows);$($m.cols);$($m.pixel_height);$($m.pixel_width)t"
        $m.in_band_resize -and (Find-Element 'In-band resize   On') -and [Text.Encoding]::ASCII.GetString((Read-PeerTrace $wire)).Contains($report)
    }
    Set-RemoteModes "$esc[?2048l"
    $null = Wait-Until 'inspector observes resize reporting disabled' { Find-Element 'In-band resize   Off' }
    Invoke-Button (Wait-Until 'close inspector' { Find-Element 'Close terminal inspector' $button })
    $null = Wait-Until 'GUI inspection closed before inspecting the hidden tab' { !(Find-Element 'Close terminal inspector') }

    $localPane = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','20','PS ')
    $localInspect = Control @('inspect','--pane',"$localPane")
    if ($localInspect.ssh -or $localInspect.ssh_progress -or $localInspect.ssh_connection) {
        throw 'PowerShell reported SSH diagnostics'
    }
    Type-Line '$inspectPid = $PID; "local-inspect-" + "ready"'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','local-inspect-ready')
    $before = (Control @('get-text','--pane',"$remote")).text
    $foreground = [FixtureMessages]::GetForegroundWindow()
    Set-RemoteModes "$esc[?1000;1002;1003;1016;2004;2048h$esc[>5u"
    $null = Wait-Until 'hidden SSH parser modes without character output' {
        $m = Inspect
        $m.pane -eq $remote -and $m.connection_generation -eq $initial.connection_generation -and
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
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    Invoke-NativeMenuAction 'Tab menu' 'Terminal inspector: Off'
    $null = Wait-Until 'the SSH route appears when reopening the inspector' { Get-NativeExpander 'Verified SSH route' }
    Set-RemoteModes ([string][char]4)
    $null = Wait-Until 'a lost SSH transport retires the verified route and shows the failure' {
        $m = Inspect
        (Find-Element 'Close terminal inspector' $button) -and
            !(Find-Element 'Verified SSH route') -and !$m.ssh -and !$m.ssh_progress -and
            $m.ssh_connection.phase -ceq 'failed' -and $m.ssh_connection.reason -and
            (Find-Element 'SSH connection   Failed') -and (Find-Element $m.ssh_connection.reason)
    }
} catch {
    $failure = $_.Exception.Message
    try {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $observed.window_visible = [InspectDpi]::IsWindowVisible($window)
        $observed.window_minimized = [InspectDpi]::IsIconic($window)
        $observed.window_bounds = $root.Current.BoundingRectangle.ToString()
        $text = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
        $observed.inspector_rows = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$text) |
            ForEach-Object { $_.Current.Name } | Where-Object { $_ -like 'Text area *' -or $_ -like 'In-band resize *' -or $_ -like '*SSH*' })
    } catch { $observed.uia_error = $_.Exception.Message }
    throw "$failure. Last pane observations: $($observed | ConvertTo-Json -Compress -Depth 5)"
} finally {
    $wire.Dispose()
    Close-FixtureApp $app
}
