# Read one marked cell on the fixture's surface after checking its visibility.
Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
public static class CursorCellProbe {
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr window, int command);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr hwnd, uint flags);
    [DllImport("user32.dll")] static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr hwnd, IntPtr insertAfter, int x, int y, int cx, int cy, uint flags);
    public static bool PositionDialog(IntPtr dialog, int x, int y) {
        IntPtr prior = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try { return SetWindowPos(dialog, IntPtr.Zero, x, y, 0, 0, 0x15); }
        finally { SetThreadDpiAwarenessContext(prior); }
    }
    public static int[] ReadPixels(IntPtr window, IntPtr foreground, int x, int y, int width, int height) {
        if (width < 1 || width > 64 || height < 1 || height > 128)
            throw new ArgumentOutOfRangeException("cell dimensions");
        IntPtr prior = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            if (GetForegroundWindow() != foreground) throw new InvalidOperationException("Fixture lost foreground");
            foreach (int col in new[] { x, x + width - 1 })
                foreach (int row in new[] { y, y + height - 1 })
                    if (GetAncestor(WindowFromPoint(new Point(col, row)), 2) != window)
                        throw new InvalidOperationException("Fixture cursor cell is obscured");
            using (var image = new Bitmap(width, height)) {
                using (var g = Graphics.FromImage(image))
                    g.CopyFromScreen(x, y, 0, 0, image.Size, CopyPixelOperation.SourceCopy);
                var result = new int[width * height];
                for (int row = 0; row < height; row++)
                    for (int col = 0; col < width; col++)
                        result[row * width + col] = image.GetPixel(col, row).ToArgb();
                return result;
            }
        } finally { SetThreadDpiAwarenessContext(prior); }
    }
    public static long Sample(IntPtr window, int x, int y, int width, int height) {
        unchecked {
            long value = 17;
            foreach (int pixel in ReadPixels(window, window, x, y, width, height))
                value = value * 31 + pixel;
            return value;
        }
    }
}
'@

function Show-CursorWindow {
    if ([FixtureMessages]::GetForegroundWindow() -ne $window -and
        ![CursorCellProbe]::SetForegroundWindow($window)) {
        # A caller running in the background can be refused activation. Restore
        # the fixture HWND on its own UI thread before resolving physical cell bounds.
        $null = [CursorCellProbe]::ShowWindowAsync($window, 7)
        $null = Wait-Until 'fixture window to minimize' { [CursorCellProbe]::IsIconic($window) }
        $null = [CursorCellProbe]::ShowWindowAsync($window, 9)
    }
    $null = Wait-Until 'the fixture to own foreground before sampling its cursor' {
        [FixtureMessages]::GetForegroundWindow() -eq $window -and ![CursorCellProbe]::IsIconic($window)
    }
}

function Get-CursorCell([string]$Name, [string]$Marker) {
    $bounds = Wait-Until 'UI Automation bounds for the marked cursor cell' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $byName = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty, ('Terminal ' + [char]0xb7 + ' ' + $Name))
        foreach ($doc in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$byName)) {
            $pattern = $doc.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
            $range = $pattern.DocumentRange.FindText($Marker,$false,$false)
            if ($range) {
                $rects = @($range.GetBoundingRectangles())
                if ($rects.Count -eq 1 -and $rects[0].Width -gt 0) { return $rects[0] }
            }
        }
    }
    return [pscustomobject]@{
        X = [int][Math]::Floor($bounds.X)
        Y = [int][Math]::Floor($bounds.Y)
        Width = [int][Math]::Ceiling($bounds.Width / $Marker.Length)
        Height = [int][Math]::Ceiling($bounds.Height)
    }
}
