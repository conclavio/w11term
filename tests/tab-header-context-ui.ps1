param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$tab = [System.Windows.Automation.ControlType]::TabItem
$menu = [System.Windows.Automation.ControlType]::MenuItem
$edit = [System.Windows.Automation.ControlType]::Edit
$button = [System.Windows.Automation.ControlType]::Button
$value = [System.Windows.Automation.ValuePattern]::Pattern

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
    public static IntPtr TargetRoot(Point point) { return GetAncestor(WindowFromPhysicalPoint(point), 2); }
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
        if (TargetRoot(point) != root || !SetPhysicalCursorPos(point.X, point.Y) ||
            !GetPhysicalCursorPos(out current) || current.X != point.X || current.Y != point.Y ||
            TargetRoot(current) != root)
            return false;
        mouse_event(8, 0, 0, 0, UIntPtr.Zero);
        mouse_event(16, 0, 0, 0, UIntPtr.Zero);
        return true;
    }
}
'@

function Open-Context([string]$Title, [string]$Action) {
    $header = Wait-Until "visible tab '$Title'" { Find-Element $Title $tab -Visible }
    $bounds = $header.Current.BoundingRectangle
    $point = New-Object TabContextPointer+Point
    $point.X = [int]($bounds.Left + $bounds.Width * 0.35)
    $point.Y = [int]($bounds.Top + $bounds.Height / 2)
    $previous = New-Object TabContextPointer+Point
    $savedCursor = [TabContextPointer]::GetPhysicalCursorPos([ref]$previous)
    $priorHit = [TabContextPointer]::DescribeHit($point)
    try {
        $null = Wait-Until "right-click on '$Title' is inside the fixture window" { [TabContextPointer]::RightClick($window, $point) }
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

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile',
    'palette-ssh', '--cols', '115', '--rows', '26') -WindowStyle Normal -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $ssh = (Wait-Until 'SSH session' { (Control @('list')).sessions | Where-Object label -eq 'palette-ssh' }).id
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','ready')
    $shell = (Control @('open','--profile','Palette PowerShell')).pane
    Wait-Focus $ssh
    $renamed = 'Background shell'
    Invoke-Button (Open-Context 'Palette PowerShell' ('Rename tab' + [char]0x2026))
    $field = Wait-Until 'tab name editor' { Find-Element 'Tab name' $edit -Visible }
    $field.GetCurrentPattern($value).SetValue($renamed)
    Invoke-Button (Wait-Until 'Save the background tab name' { Find-Element 'Save' $button -Visible })
    $null = Wait-Until 'only the background shell was renamed' {
        $sessions = (Control @('list')).sessions
        ($sessions | Where-Object id -eq $shell).tab_title -ceq $renamed -and
            !($sessions | Where-Object id -eq $ssh).tab_title -and
            !(Find-Element 'Tab name' $edit -Visible)
    }
    Wait-Focus $ssh
    Invoke-Button (Open-Context $renamed 'Pin tab')
    $pinned = "Pinned: $renamed"
    $null = Wait-Until 'the background tab is pinned' { Find-Element $pinned $tab -Visible }
    Wait-Focus $ssh
    $close = Open-Context $pinned 'Close tab'
    if ($close.Current.IsEnabled) { throw 'Pinned tab could be closed' }
    Invoke-Button (Wait-Until 'Unpin tab' { Find-Element 'Unpin tab' $menu -Visible })
    $null = Wait-Until 'the background tab was unpinned' { Find-Element $renamed $tab -Visible }
    Wait-Focus $ssh
    $colors = Open-Context $renamed 'Tab color'
    $colors.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    Invoke-Button (Wait-Until 'blue tab color' { Find-Element 'Set tab color to Blue' $menu -Visible })
    $colored = "$renamed (Blue tab)"
    $null = Wait-Until 'the background tab changed color' { Find-Element $colored $tab -Visible }
    Wait-Focus $ssh
    foreach ($ch in 'still-in-ssh'.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','12','still-in-ssh')

    Invoke-Button (Open-Context $colored 'Duplicate tab layout')
    $copy = Wait-Until 'a new local session from the clicked tab is active' {
        $session = (Control @('list')).sessions | Where-Object {
            $_.id -ne $shell -and $_.id -ne $ssh -and $_.active -and $_.tab_title -ceq $renamed
        }
        if ($session) { $session.id }
    }
    $null = Control @('wait-for-text','--pane',"$copy",'--timeout','15','PS ')
    $null = Control @('close-pane','--pane',"$copy")
    $sshHeader = Wait-Until 'SSH tab header' { Find-Element 'palette-ssh' $tab -Visible }
    $sshHeader.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Wait-Focus $ssh
    Invoke-Button (Open-Context $colored 'Close tab')
    $null = Wait-Until 'the clicked shell closed and the original SSH remains active' {
        $sessions = @((Control @('list')).sessions)
        $sessions.Count -eq 1 -and $sessions[0].id -eq $ssh -and $sessions[0].active
    }
} finally {
    Close-FixtureApp $app
}
