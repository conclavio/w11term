Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class FixtureMessages {
    [StructLayout(LayoutKind.Sequential)]
    struct GuiThreadInfo {
        public int Size, Flags;
        public IntPtr Active, Focus, Capture, MenuOwner, MoveSize, Caret;
        public int Left, Top, Right, Bottom;
    }
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool GetGUIThreadInfo(uint thread, ref GuiThreadInfo info);
    public static IntPtr FocusedWindow(IntPtr root, uint expectedPid) {
        uint pid;
        uint thread = GetWindowThreadProcessId(root, out pid);
        var info = new GuiThreadInfo { Size = Marshal.SizeOf(typeof(GuiThreadInfo)) };
        if (pid != expectedPid || !GetGUIThreadInfo(thread, ref info) || info.Focus == IntPtr.Zero)
            return IntPtr.Zero;
        GetWindowThreadProcessId(info.Focus, out pid);
        return pid == expectedPid ? info.Focus : IntPtr.Zero;
    }
    [DllImport("user32.dll", SetLastError=true)] public static extern bool PostMessage(
        IntPtr hwnd, uint msg, UIntPtr key, IntPtr detail);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] private static extern uint MapVirtualKeyW(uint key, uint mapping);
    public static bool PostKey(IntPtr hwnd, uint msg, uint key) {
        uint detail = 0;
        if (msg == 0x100 || msg == 0x101) { // WM_KEYDOWN / WM_KEYUP
            uint scan = MapVirtualKeyW(key, 4); // MAPVK_VK_TO_VSC_EX, including the E0 prefix
            detail = 1 | ((scan & 0xff) << 16);
            // Windows can report the shared keypad scan for arrows/navigation. The
            // generic fixtures press the dedicated cluster; keypad fixtures encode their own.
            if ((scan >> 8) == 0xe0 || (key >= 0x21 && key <= 0x28) || key == 0x2d || key == 0x2e)
                detail |= 1u << 24;
            // TranslateMessage treats a release with the transition bit clear as another press.
            if (msg == 0x101) detail |= 0xc0000000u;
        }
        return PostMessage(hwnd, msg, new UIntPtr(key), new IntPtr(unchecked((int)detail)));
    }
}
'@

# The SSH fixture opens this file for append on each DATA packet. Keep the
# reader open across delivery so every packet can be traced even during a poll.
function Open-PeerTraceReader([string]$Path) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    return [IO.BinaryReader]::new($stream)
}
function Read-PeerTrace([IO.BinaryReader]$Reader) {
    $Reader.BaseStream.Position = 0
    return ,$Reader.ReadBytes([int]$Reader.BaseStream.Length)
}

# Give the app permission to finish its write while the fixture polls.
function Read-ClipboardFixture {
    try {
        $stream = [IO.File]::Open($env:W11TERM_TEST_COPY_FILE, [IO.FileMode]::Open,
            [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    } catch [IO.FileNotFoundException] {
        return $null
    } catch [IO.IOException] {
        if (($_.Exception.HResult -band 0xffff) -in 32,33) { return $null }
        throw
    }
    $reader = [IO.StreamReader]::new($stream)
    try { $reader.ReadToEnd() } finally { $reader.Dispose() }
}

function Wait-Until([string]$Description, [scriptblock]$Probe) {
    $until = [DateTime]::UtcNow.AddSeconds(15)
    do {
        if ($app.HasExited) { throw "w11term exited while waiting for $Description" }
        $answer = & $Probe
        if ($answer) { return $answer }
        Start-Sleep -Milliseconds 80
    } while ([DateTime]::UtcNow -lt $until)
    throw "Timed out waiting for $Description"
}
function Find-OwnedDialog([string]$Title) {
    foreach ($hwnd in [FixtureDialog]::Windows($app.Id)) {
        if ([FixtureDialog]::GetWindow($hwnd, 4) -ne $window) { continue }
        $element = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        if ($element.Current.ClassName -eq '#32770' -and $element.Current.Name -eq $Title) { return $hwnd }
    }
}
function Select-DialogItem([IntPtr]$Dialog, [string]$Name, [switch]$Add) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($Dialog)
    $byName = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $selectable = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::IsSelectionItemPatternAvailableProperty, $true)
    $match = [System.Windows.Automation.AndCondition]::new($byName, $selectable)
    $null = Wait-Until "native picker selection '$Name'" {
        $item = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $match)
        if ($item) {
            $selection = $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
            if (!$selection.Current.IsSelected) {
                if ($Add) { $selection.AddToSelection() } else { $selection.Select() }
            }
            return $selection.Current.IsSelected
        }
    }
}
function Wait-InputWindow {
    # The GUI replaces its bootstrap title only after installing the HWND's input route.
    $prefix = 'w11term ' + [char]0xb7 + ' '
    return Wait-Until 'the fixture window to bind its keyboard' {
        $app.Refresh()
        if ($app.MainWindowHandle -ne [IntPtr]::Zero -and ([string]$app.MainWindowTitle).StartsWith($prefix)) {
            $app.MainWindowHandle
        }
    }
}
function Control([string[]]$Command) {
    $reply = & $Cli --pid $app.Id @Command | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or -not $reply.ok) { throw "Control request failed: $Command $($reply.error)" }
    return $reply.result
}
function Copy-ModeOpen {
    $dot = ' ' + [char]0xb7 + ' '
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $text = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    foreach ($label in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text)) {
        if ($label.Current.Name.StartsWith("Copy mode$dot")) { return $true }
    }
    return $false
}
function Find-Element([string]$Name, [System.Windows.Automation.ControlType]$Type) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    if ($Type) {
        $byType = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $Type)
        $condition = [System.Windows.Automation.AndCondition]::new($condition, $byType)
    }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $found = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if ($found) { return $found }
    # WinUI may expose a flyout as a sibling of the main HWND.
    $processCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $app.Id)
    $popups = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
        [System.Windows.Automation.TreeScope]::Children, $processCondition)
    foreach ($popup in $popups) {
        $found = $popup.FindFirst([System.Windows.Automation.TreeScope]::Subtree, $condition)
        if ($found) { return $found }
    }
}
function Invoke-Button($Element) {
    $Element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
}
function Wait-CheckBox([string]$Name, [System.Windows.Automation.ToggleState]$State) {
    Wait-Until "enabled checkbox '$Name' in state $State" {
        $element = Find-Element $Name ([System.Windows.Automation.ControlType]::CheckBox)
        if ($element -and $element.Current.IsEnabled) {
            $pattern = $element.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
            if ($pattern.Current.ToggleState -eq $State) { $pattern }
        }
    }
}
function Post-Key([uint32]$Message, [uint32]$Key) {
    # Address this process's window. This never types into the foreground application.
    if (-not [FixtureMessages]::PostKey($window, $Message, $Key)) {
        throw "Could not post key event to the fixture window"
    }
}
function Press-Key([uint32]$Key) {
    Post-Key 0x100 $Key
    Post-Key 0x101 $Key
}
function Press-FocusedKey([uint32]$Key) {
    # WinUI routes ordinary typing through the focused child HWND. Inspect the
    # fixture's own thread and keep both messages addressed to that process.
    $target = Wait-Until 'focused input HWND owned by the fixture' {
        $hwnd = [FixtureMessages]::FocusedWindow($window, $app.Id)
        if ($hwnd -ne [IntPtr]::Zero) { $hwnd }
    }
    foreach ($message in 0x100,0x101) {
        if (![FixtureMessages]::PostKey($target, $message, $Key)) { throw "Could not post $Key to HWND $target" }
    }
    return $target
}
function Open-Palette([string]$Command) {
    Press-Key 0x76 # F7 in the fixture's keymap
    try {
        return Wait-Until "F7 to focus command search for '$Command'" {
            $entry = Find-Element 'Search commands' ([System.Windows.Automation.ControlType]::Edit)
            if ($entry -and $entry.Current.HasKeyboardFocus) { $entry }
        }
    } catch {
        $mounted = $null -ne (Find-Element 'Search commands' ([System.Windows.Automation.ControlType]::Edit))
        $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
        $focus = if ($focused -and $focused.Current.ProcessId -eq $app.Id) { $focused.Current.Name } else { '(outside fixture)' }
        throw "$_ Search edit mounted: $mounted. Foreground HWND: $([FixtureMessages]::GetForegroundWindow()), fixture HWND: $window. Focus: $focus"
    }
}
function Run-Palette([string]$Command) {
    $field = Open-Palette $Command
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Command)
    # UI Automation can expose the button name while WinUI is replacing its
    # provider after filtering. Reacquire the row until it is invokable.
    $invoke = Wait-Until "invoke provider for palette command '$Command'" {
        try {
            $row = Find-Element $Command ([System.Windows.Automation.ControlType]::Button)
            $provider = $null
            if ($row -and $row.Current.IsEnabled -and
                $row.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$provider)) {
                $provider
            }
        } catch [System.Windows.Automation.ElementNotAvailableException] {
            # A subsequent probe can read the replacement row.
        }
    }
    $invoke.Invoke()
    $null = Wait-Until 'picker to close' { !(Find-Element 'Search commands') }
}
function Wait-Focus([int]$Pane) {
    try {
        $null = Wait-Until "pane $Pane to receive focus" {
            $active = @((Control @('list')).sessions | Where-Object active)
            $active.Count -eq 1 -and $active[0].id -eq $Pane
        }
    } catch {
        throw "$_ Current panes: $((Control @('list')).sessions | Select-Object id,label,title,active,visible | ConvertTo-Json -Compress)"
    }
}
function Open-PickerQuery([string]$Query) {
    Write-Output "Searching for $Query"
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    Invoke-Button (Wait-Until 'Find pane menu item' { Find-Element ('Find pane' + [char]0x2026 + '    Ctrl+Shift+P') })
    $edit = Wait-Until 'keyboard focus in the pane search field' {
        $field = Find-Element 'Find pane'
        if ($field -and $field.Current.HasKeyboardFocus) { return $field }
    }
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Query)
}
function Get-PickerRows {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $buttons = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button)
    $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $buttons) |
        ForEach-Object { $_.Current.Name } | Where-Object { $_ -match 'Tab [0-9]+ / pane [0-9]+' }
}
function Picker-Query([string]$Query) {
    Open-PickerQuery $Query
    $null = Wait-Until "a single rendered result matching '$Query'" {
        $rows = @(Get-PickerRows)
        $rows.Count -eq 1 -and $rows[0].IndexOf($Query, [StringComparison]::OrdinalIgnoreCase) -ge 0
    }
}
