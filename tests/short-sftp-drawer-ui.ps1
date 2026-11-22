param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$scroll = [System.Windows.Automation.ScrollPattern]::Pattern
$walker = [System.Windows.Automation.TreeWalker]::RawViewWalker
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class DrawerPointer {
    [StructLayout(LayoutKind.Sequential)] struct Point { public int X, Y; }
    [DllImport("user32.dll")] static extern IntPtr WindowFromPhysicalPoint(Point point);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr hwnd, uint flag);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern void mouse_event(uint flags, uint x, uint y, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    public static void ClickHosted(IntPtr window, int x, int y) {
        if (Environment.GetEnvironmentVariable("GITHUB_ACTIONS") != "true" ||
            Environment.GetEnvironmentVariable("RUNNER_ENVIRONMENT") != "github-hosted")
            throw new InvalidOperationException("A physical click is restricted to the disposable hosted runner");
        var pt = new Point { X=x, Y=y };
        if (GetForegroundWindow() != window || GetAncestor(WindowFromPhysicalPoint(pt), 2) != window ||
            !SetCursorPos(x, y) || GetForegroundWindow() != window || GetAncestor(WindowFromPhysicalPoint(pt), 2) != window)
            throw new InvalidOperationException("File row did not hit the fixture window");
        mouse_event(2, 0, 0, 0, UIntPtr.Zero);
        mouse_event(4, 0, 0, 0, UIntPtr.Zero);
    }
}
'@
function Control-Point($Element, $Viewport) {
    if (!$Element -or $Element.Current.IsOffscreen) { return }
    $rect = $Element.Current.BoundingRectangle
    $rect.Intersect($Viewport.Current.BoundingRectangle)
    $rect.Intersect($root.Current.BoundingRectangle)
    if (!$rect.IsEmpty -and $rect.Width -gt 2 -and $rect.Height -gt 2) {
        return [System.Windows.Point]::new($rect.X + $rect.Width / 2, $rect.Y + $rect.Height / 2)
    }
}
function Is-Hit($Element, [System.Windows.Point]$Point) {
    $hit = [System.Windows.Automation.AutomationElement]::FromPoint($Point)
    while ($hit -and $hit.Current.ProcessId -eq $app.Id) {
        if ([System.Windows.Automation.Automation]::Compare($Element,$hit) -or
            $hit.Current.ClassName -eq 'Microsoft.UI.Content.DesktopChildSiteBridge') { return $true }
        $hit = $walker.GetParent($hit)
    }
    return $false
}
function Is-Enabled([string]$Label) {
    $item = Find-Element $Label $button
    return $item -and $item.Current.IsEnabled
}

function Drawer-Action($Actions, [string]$Label) {
    $match = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Label)
    return $Actions.FindFirst([System.Windows.Automation.TreeScope]::Descendants,$match)
}
function Check-Actions {
    $actions = Find-Element 'SFTP actions'
    $actionsScroll = $actions.GetCurrentPattern($scroll)
    $remaining = [Collections.Generic.HashSet[string]]::new()
    $ellipsis = [char]0x2026
    foreach ($label in 'Close','Up','Refresh',"Bookmarks$ellipsis","Upload$ellipsis",'New folder',"Rename$ellipsis","Delete$ellipsis",
        'Open folder',"Download$ellipsis",'Open shell here','Copy directory path','Copy selected path','Follow shell directory',
        'Replace existing files',"Filter filenames$ellipsis",'Clear filter','Previous','Next') { $null = $remaining.Add($label) }
    $horizontal = if ($actionsScroll.Current.HorizontallyScrollable) { 0,100 } else { -1 }
    foreach ($percent in 0,20,40,60,80,100) {
        foreach ($across in $horizontal) {
            $down = if ($actionsScroll.Current.VerticallyScrollable) { $percent } else { -1 }
            $actionsScroll.SetScrollPercent($across, $down)
            $null = Wait-Until "actions scrolled to $across, $down percent" {
                ($down -lt 0 -or [Math]::Abs($actionsScroll.Current.VerticalScrollPercent - $down) -lt 2) -and
                ($across -lt 0 -or [Math]::Abs($actionsScroll.Current.HorizontalScrollPercent - $across) -lt 2)
            }
            foreach ($label in @($remaining)) {
                $item = Drawer-Action $actions $label
                if (Control-Point $item $actions) { $null = $remaining.Remove($label) }
            }
        }
        if (!$remaining.Count) { return }
    }
    if ($remaining.Count) { throw "Unreachable SFTP actions at minimum height: $($remaining -join ', ')" }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    # UI Automation reports physical screen coordinates on scaled displays.
    $null = [DrawerPointer]::SetThreadDpiAwarenessContext([IntPtr](-4))
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $resize = $root.GetCurrentPattern([System.Windows.Automation.TransformPattern]::Pattern)
    $resize.Resize(900,940)
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    Run-Palette 'SFTP files'
    $folder = Wait-Until 'first remote directory' { Find-Element 'projects/' $button }
    $entries = Wait-Until 'SFTP entry viewport' { Find-Element 'SFTP entries' }
    $tallHeight = $entries.Current.BoundingRectangle.Height
    $rowHeight = $folder.Current.BoundingRectangle.Height
    if ($rowHeight -lt 10 -or $tallHeight -lt 3 * $rowHeight) { throw 'Expected at least three visible rows in the tall window' }

    # WinUI clamps this request to the current monitor's native minimum.
    $resize.Resize(900,200)
    try { $point = Wait-Until 'one complete, hit-testable file row in the short window' {
        $entries = Find-Element 'SFTP entries'
        $folder = Find-Element 'projects/' $button
        if (!$entries -or !$folder) { return }
        $height = $entries.Current.BoundingRectangle.Height
        $pt = Control-Point $folder $entries
        if ($pt -and $height -lt $tallHeight -and $folder.Current.BoundingRectangle.Height -ge $rowHeight-1 -and
            $height -ge $rowHeight -and (Is-Hit $folder $pt)) { $pt }
    } } catch {
        $folder = Find-Element 'projects/' $button
        $entries = Find-Element 'SFTP entries'
        $pt = Control-Point $folder $entries
        $hit = if ($pt) { [System.Windows.Automation.AutomationElement]::FromPoint($pt) }
        throw "$_ Window=$($root.Current.BoundingRectangle), entries=$($entries.Current.BoundingRectangle), row=$($folder.Current.BoundingRectangle), tall=$tallHeight, first row=$rowHeight, hit=$($hit.Current.ClassName) '$($hit.Current.Name)' pid=$($hit.Current.ProcessId), fixture=$($app.Id)"
    }
    Check-Actions
    if (Is-Enabled 'Open folder') { throw 'Unexpected selection before activating the visible directory' }
    if ($env:GITHUB_ACTIONS -ceq 'true' -and $env:RUNNER_ENVIRONMENT -ceq 'github-hosted') {
        [DrawerPointer]::ClickHosted($window, [int]$point.X, [int]$point.Y)
    } else {
        Invoke-Button (Find-Element 'projects/' $button)
    }
    $null = Wait-Until 'first selectable folder activated' { (Is-Enabled 'Open folder') -and !(Is-Enabled ('Download' + [char]0x2026)) }
    $resize.Resize(900,940)
    $null = Wait-Until 'file list expands again' { (Find-Element 'SFTP entries').Current.BoundingRectangle.Height -ge $tallHeight-2 }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
