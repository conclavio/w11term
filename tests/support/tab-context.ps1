Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class TabContextPointer {
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr hwnd, uint flag);
    [DllImport("user32.dll")] static extern IntPtr WindowFromPhysicalPoint(Point point);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr window, StringBuilder name, int capacity);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr window, out uint process);
    [DllImport("user32.dll")] public static extern bool GetPhysicalCursorPos(out Point point);
    [DllImport("user32.dll")] public static extern bool SetPhysicalCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern void mouse_event(uint flags, uint x, uint y, uint data, UIntPtr extra);
    static bool IsClientTarget(IntPtr root, Point point) {
        var target = WindowFromPhysicalPoint(point);
        if (GetAncestor(target, 2) != root) return false;
        var name = new StringBuilder(256);
        return GetClassName(target, name, name.Capacity) > 0 && name.ToString() != "InputNonClientPointerSource";
    }
    public static string DescribeHit(Point point) {
        var hit = WindowFromPhysicalPoint(point);
        var root = GetAncestor(hit, 2);
        uint process;
        GetWindowThreadProcessId(root, out process);
        var name = new StringBuilder(256);
        GetClassName(hit, name, name.Capacity);
        return hit + " " + name + " root=" + root + " pid=" + process;
    }
    public static bool RightClick(IntPtr root, Point point) {
        Point current;
        if (!IsClientTarget(root, point) || !SetPhysicalCursorPos(point.X, point.Y) ||
            !GetPhysicalCursorPos(out current) || current.X != point.X || current.Y != point.Y ||
            !IsClientTarget(root, current))
            return false;
        mouse_event(8, 0, 0, 0, UIntPtr.Zero);
        mouse_event(16, 0, 0, 0, UIntPtr.Zero);
        return true;
    }
}
'@

function Open-Context([string]$Title, [string]$Action) {
    $tab = [System.Windows.Automation.ControlType]::TabItem
    $menu = [System.Windows.Automation.ControlType]::MenuItem
    $header = Wait-Until "visible tab '$Title'" { Find-Element $Title $tab -Visible }
    $bounds = $header.Current.BoundingRectangle
    $point = New-Object TabContextPointer+Point
    $point.X = [int]($bounds.Left + $bounds.Width * 0.35)
    $point.Y = [int]($bounds.Top + $bounds.Height / 2)
    $previous = New-Object TabContextPointer+Point
    $savedCursor = [TabContextPointer]::GetPhysicalCursorPos([ref]$previous)
    $priorHit = [TabContextPointer]::DescribeHit($point)
    try {
        $null = Wait-Until "right-click on '$Title' reaches the fixture client area" {
            $currentHeader = Find-Element $Title $tab -Visible
            if (!$currentHeader) { return }
            $currentBounds = $currentHeader.Current.BoundingRectangle
            $point.X = [int]($currentBounds.Left + $currentBounds.Width * 0.35)
            $point.Y = [int]($currentBounds.Top + $currentBounds.Height / 2)
            [TabContextPointer]::RightClick($window, $point)
        }
        Wait-Until "tab context menu item '$Action'" { Find-Element $Action $menu -Visible }
    } catch {
        $failure = $_
        $roots = [System.Windows.Automation.AutomationElement]::RootElement
        $owner = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $app.Id)
        $names = foreach ($root in $roots.FindAll([System.Windows.Automation.TreeScope]::Children, $owner)) {
            $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.Condition]::TrueCondition) |
                ForEach-Object { "$($_.Current.LocalizedControlType): $($_.Current.Name)" }
        }
        throw "$failure Point: $($point.X),$($point.Y). Foreground: $([FixtureMessages]::GetForegroundWindow()). Hit: $priorHit -> $([TabContextPointer]::DescribeHit($point)). Fixture: $window PID=$($app.Id). Controls: $($names -join ' | ')."
    } finally {
        $current = New-Object TabContextPointer+Point
        if ($savedCursor -and [TabContextPointer]::GetPhysicalCursorPos([ref]$current) -and
            $current.X -eq $point.X -and $current.Y -eq $point.Y) {
            $null = [TabContextPointer]::SetPhysicalCursorPos($previous.X, $previous.Y)
        }
    }
}
