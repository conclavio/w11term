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
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr window);
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
function Check-NarrowNavigation {
    $resize.Resize([Math]::Round(540 * $scale), $root.Current.BoundingRectangle.Height)
    try { $null = Wait-Until 'complete navigation and transfer rows in a drawer narrower than 280 DIPs' {
        $entries = Find-Element 'SFTP entries'
        if (!$entries) { return }
        $viewport = $entries.Current.BoundingRectangle
        if ($viewport.IsEmpty -or $viewport.Width -ge 280 * $scale) { return }
        $bounds = @{}
        $find = 'Find files' + [char]0x2026
        $upload = 'Upload' + [char]0x2026
        $download = 'Download' + [char]0x2026
        foreach ($label in 'Back','Forward','Up','Refresh',$find,$upload,$download,'Selected entry actions','More folder actions') {
            $item = Find-Element $label $button -Visible
            if (!$item -or $item.Current.IsOffscreen) { return }
            $rect = $item.Current.BoundingRectangle
            if ($rect.IsEmpty -or $rect.Width -lt 20 * $scale -or $rect.Height -lt 20 * $scale -or
                $rect.Left -lt $viewport.Left - 1 -or $rect.Right -gt $viewport.Right + 1 -or
                $rect.Bottom -gt $viewport.Top + 1) { return }
            $bounds[$label] = $rect
        }
        [Math]::Abs($bounds.Back.Top - $bounds.Forward.Top) -le 1 -and $bounds.Back.Right -le $bounds.Forward.Left -and
            [Math]::Abs($bounds.Back.Top - $bounds.Refresh.Top) -le 1 -and $bounds.Up.Right -le $bounds.Refresh.Left -and
            [Math]::Abs($bounds[$find].Top - $bounds['More folder actions'].Top) -le 1 -and
            $bounds.Refresh.Bottom -le $bounds[$find].Top
    } } catch {
        $entries = Find-Element 'SFTP entries'
        $bounds = foreach ($label in 'Back','Forward','Up','Refresh','More folder actions') {
            $element = Find-Element $label $button
            "$label=$($element.Current.BoundingRectangle)"
        }
        throw "$_ Scale=$scale Window=$($root.Current.BoundingRectangle) Entries=$($entries.Current.BoundingRectangle) $($bounds -join ' ')"
    }
}
function Check-Actions {
    $actions = Find-Element 'SFTP actions'
    $actionsScroll = $actions.GetCurrentPattern($scroll)
    $remaining = [Collections.Generic.HashSet[string]]::new()
    $ellipsis = [char]0x2026
    foreach ($label in 'Open shell here','Follow shell directory',
        'Replace existing files','Sort by name','Sort newest first','Sort largest first',"Filter filenames$ellipsis",'Clear filter','Previous','Next') { $null = $remaining.Add($label) }
    $horizontal = if ($actionsScroll.Current.HorizontallyScrollable) { 0,100 } else { -1 }
    $step = [Math]::Max(1, [Math]::Min(20, [Math]::Floor($actionsScroll.Current.VerticalViewSize / 2)))
    for ($percent = 0; $percent -le 100; $percent += $step) {
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
        if ($percent -lt 100 -and $percent + $step -gt 100) { $percent = 100 - $step }
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
    $scale = [DrawerPointer]::GetDpiForWindow($window) / 96.0
    $tallWidth = [Math]::Round(900 * $scale)
    $tallWindowHeight = [Math]::Round(940 * $scale)
    $resize.Resize($tallWidth,$tallWindowHeight)
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    Run-Palette 'SFTP files'
    $tall = Wait-Until 'at least three visible file rows in the tall window' {
        $folder = Find-Element 'projects/' $button
        $entries = Find-Element 'SFTP entries'
        if (!$folder -or !$entries) { return }
        $row = $folder.Current.BoundingRectangle.Height
        $body = $entries.Current.BoundingRectangle.Height
        if ($row -ge 10 -and $body -ge 3 * $row) {
            @{ Row = $row; Entries = $body; Window = $root.Current.BoundingRectangle.Height }
        }
    }
    $tallOuterHeight = $tall.Window
    $tallHeight = $tall.Entries
    $rowHeight = $tall.Row

    Check-NarrowNavigation
    # WinUI clamps this request to the current monitor's native minimum.
    $resize.Resize(900,200)
    try { $point = Wait-Until 'one complete, hit-testable file row in the short window' {
        $entries = Find-Element 'SFTP entries'
        $folder = Find-Element 'projects/' $button
        if (!$entries -or !$folder) { return }
        $height = $entries.Current.BoundingRectangle.Height
        $pt = Control-Point $folder $entries
        # The native minimum can be tall enough at some DPIs to retain the full
        # entry viewport. Verify that the window shrank and the row still fits.
        if ($pt -and $root.Current.BoundingRectangle.Height -lt $tallOuterHeight -and
            $folder.Current.BoundingRectangle.Height -ge $rowHeight-1 -and
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
    $verify = Get-SftpEntryAction ('Download and verify' + [char]0x2026) -AllowDisabled
    if ($verify.Current.IsEnabled) { throw 'Folder selection offered verified file download' }
    $menu = [System.Windows.Automation.ControlType]::MenuItem
    Invoke-Button (Wait-Until 'selected folder Properties in the short drawer' {
        $item = Find-Element ('Properties' + [char]0x2026) $menu
        if ($item -and $item.Current.IsEnabled) { $item }
    })
    $null = Wait-Until 'properties described the selected directory' {
        $item = Find-Element 'File properties'
        if ($item) {
            $body = $item.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1)
            $body.Contains('Path  /projects') -and $body.Contains('Type  Directory')
        }
    }
    Invoke-Button (Wait-Until 'Close properties' { Find-Element 'Close properties' $button })
    $resize.Resize($tallWidth,$tallWindowHeight)
    $null = Wait-Until 'the tall window restores its entry viewport' {
        $root.Current.BoundingRectangle.Height -ge $tallOuterHeight-2 -and
            (Find-Element 'SFTP entries').Current.BoundingRectangle.Height -ge $tallHeight-2
    }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
