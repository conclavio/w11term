[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/cursor-cell.ps1')
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class PointerProbe {
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)]
    struct CursorInfo { public int Size, Flags; public IntPtr Cursor; public Point Position; }
    [DllImport("user32.dll")] static extern bool GetCursorInfo(ref CursorInfo info);
    [DllImport("user32.dll", ExactSpelling = true)] static extern IntPtr LoadCursorW(IntPtr module, IntPtr resource);
    [DllImport("user32.dll")] static extern bool GetPhysicalCursorPos(out Point point);
    [DllImport("user32.dll")] static extern IntPtr WindowFromPhysicalPoint(Point point);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr window, uint flag);
    [StructLayout(LayoutKind.Sequential)]
    struct MouseInput { public int X, Y; public uint Data, Flags, Time; public UIntPtr Extra; }
    [StructLayout(LayoutKind.Sequential)]
    struct Input { public uint Type; public MouseInput Mouse; }
    [DllImport("user32.dll", SetLastError = true)]
    static extern uint SendInput(uint count, ref Input input, int size);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int index);
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool SetPhysicalCursorPos(int x, int y);
    public static Point Position() {
        Point point;
        if (!GetPhysicalCursorPos(out point)) throw new InvalidOperationException("GetPhysicalCursorPos failed");
        return point;
    }
    public static bool IsShape(int resource) {
        var info = new CursorInfo { Size = Marshal.SizeOf(typeof(CursorInfo)) };
        return GetCursorInfo(ref info) &&
               info.Cursor == LoadCursorW(IntPtr.Zero, new IntPtr(resource));
    }
    public static string Describe(int resource) {
        var info = new CursorInfo { Size = Marshal.SizeOf(typeof(CursorInfo)) };
        bool ok = GetCursorInfo(ref info);
        Point actual = Position();
        return String.Format("GetCursorInfo={0} cursor={1:x} expected={2:x} flags={3} at ({4},{5})", ok,
            info.Cursor.ToInt64(), LoadCursorW(IntPtr.Zero, new IntPtr(resource)).ToInt64(),
            info.Flags, actual.X, actual.Y);
    }
    public static bool MoveInWindow(IntPtr window, int x, int y) {
        IntPtr prior = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            var point = new Point { X = x, Y = y };
            if (GetAncestor(WindowFromPhysicalPoint(point), 2) != window) return false;
            int left = GetSystemMetrics(76), top = GetSystemMetrics(77);
            int width = GetSystemMetrics(78), height = GetSystemMetrics(79);
            if (x < left || y < top || x >= left + width || y >= top + height) return false;
            // Aim at the center of the target pixel, in absolute virtual-desktop coordinates.
            var input = new Input { Mouse = new MouseInput {
                X = (int)((2L * (x - left) + 1) * 32768 / width),
                Y = (int)((2L * (y - top) + 1) * 32768 / height),
                Flags = 0xe001, // MOVE | MOVE_NOCOALESCE | VIRTUALDESK | ABSOLUTE
            }};
            return SendInput(1, ref input, Marshal.SizeOf(typeof(Input))) == 1;
        } finally { SetThreadDpiAwarenessContext(prior); }
    }
}
'@
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"font_family":"Consolas","shortcuts":{"split_right":"F8","focus_left":"F6","focus_right":"F9"}}')
$profiles = Join-Path $State 'profiles.json'
$esc = [string][char]27
$bell = [string][char]7

function Send-Vt([int]$Pane, [string]$Sequence) {
    $data = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($Sequence))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$data)
}
function Pane-Cell([int]$Pane, [string]$Marker) {
    Send-Vt $Pane ($esc + '[2J' + $esc + '[5;8H' + $Marker)
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15',$Marker)
    Get-CursorCell 'palette-ssh' $Marker
}
function Expect-Shape([int]$Id, [string]$Name) {
    try { $null = Wait-Until "native $Name pointer" { [PointerProbe]::IsShape($Id) } }
    catch { throw "$_ $([PointerProbe]::Describe($Id))" }
}
function Move-Pointer([int]$X, [int]$Y) {
    if ([FixtureMessages]::GetForegroundWindow() -ne $window) { throw 'Fixture lost foreground before pointer move' }
    if (![PointerProbe]::MoveInWindow($window, $X, $Y)) { throw "Could not inject mouse input at ($X,$Y) inside the fixture window" }
    $null = Wait-Until "pointer to reach ($X,$Y)" {
        $actual = [PointerProbe]::Position()
        [Math]::Abs($actual.X - $X) -le 1 -and [Math]::Abs($actual.Y - $Y) -le 1
    }
}

$previous = [PointerProbe]::Position()
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','80','--rows','16') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $left = ((Control @('list')).sessions | Where-Object active).id
    $null = Control @('wait-for-text','--pane',"$left",'--timeout','15','ready')
    Show-CursorWindow
    Press-Key 0x77
    $right = Wait-Until 'focused right SSH split' { ((Control @('list')).sessions | Where-Object { $_.id -ne $left -and $_.active }).id }
    $null = Control @('wait-for-text','--pane',"$right",'--timeout','15','ready')
    $rightCell = Pane-Cell $right 'RightPointer'
    Press-Key 0x75
    Wait-Focus $left
    $leftCell = Pane-Cell $left 'LeftPointer'
    Press-Key 0x78
    Wait-Focus $right
    $menu = Wait-Until 'native tab menu' { Find-Element 'Tab menu' -Visible }
    $header = $menu.Current.BoundingRectangle
    Move-Pointer ($header.X + $header.Width / 2) ($header.Y + $header.Height / 2)
    Send-Vt $left ($esc + ']22;pointer' + $bell + 'left-shape-ready')
    $null = Control @('wait-for-text','--pane',"$left",'--timeout','15','left-shape-ready')
    Move-Pointer ($leftCell.X + $leftCell.Width / 2) ($leftCell.Y + $leftCell.Height / 2)
    Expect-Shape 32649 'hand after entering the left pane'
    $stationary = [PointerProbe]::Position()
    Send-Vt $left ($esc + ']22;crosshair' + $bell)
    Expect-Shape 32515 'crosshair in the unfocused left pane'
    Send-Vt $right ($esc + ']22;wait' + $bell + 'right-shape-ready')
    $null = Control @('wait-for-text','--pane',"$right",'--timeout','15','right-shape-ready')
    if (![PointerProbe]::IsShape(32515)) { throw 'A neighboring SSH pane changed the pointer' }

    Send-Vt $left ($esc + ']22;>help,ew-resize' + $bell)
    Expect-Shape 32644 'horizontal resize at the stationary pointer'
    Send-Vt $left ($esc + ']22;<' + $bell)
    Expect-Shape 32651 'help restored by pop'
    Send-Vt $left ($esc + '[?1049h' + $esc + ']22;text' + $bell)
    Expect-Shape 32513 'alternate-screen text'
    Send-Vt $left ($esc + '[?1049l')
    Expect-Shape 32651 'primary-screen help restored'
    Send-Vt $left ($esc + ']22;' + $bell)
    Expect-Shape 32512 'default after OSC 22 reset'
    $actual = [PointerProbe]::Position()
    if ($actual.X -ne $stationary.X -or $actual.Y -ne $stationary.Y) { throw 'Pointer moved during stationary checks' }

    Move-Pointer ($rightCell.X + $rightCell.Width / 2) ($rightCell.Y + $rightCell.Height / 2)
    Expect-Shape 32514 'right pane wait'
    Move-Pointer ($header.X + $header.Width / 2) ($header.Y + $header.Height / 2)
    Expect-Shape 32512 'default on the tab menu'
    Wait-Focus $right
} finally {
    $null = [PointerProbe]::SetPhysicalCursorPos($previous.X, $previous.Y)
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
