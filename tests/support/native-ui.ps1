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
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr window);
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

function Close-FixtureApp([Diagnostics.Process]$Process) {
    try {
        if ($Process.HasExited) { return }
        $null = $Process.CloseMainWindow()
        if ($Process.WaitForExit(5000)) { return }
        try {
            $Process.Kill()
        } catch {
            # Windows can reject termination after the child has already exited.
            if (!$Process.WaitForExit(1000)) { throw }
        }
        if (!$Process.WaitForExit(5000)) {
            throw "Fixture process $($Process.Id) did not exit after termination"
        }
    } finally { $Process.Dispose() }
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
function Get-ControlManifestPath([int]$ProcessId) {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        return Join-Path $env:ProgramData "w11term\elevated\$($identity.User.Value)\instances\$ProcessId.json"
    }
    return Join-Path $env:LOCALAPPDATA "w11term\instances\$ProcessId.json"
}
function Control([string[]]$Command) {
    $reply = & $Cli --pid $app.Id @Command | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or -not $reply.ok) { throw "Control request failed: $Command $($reply.error)" }
    return $reply.result
}
function Get-PaneCellHeight([int]$Pane) {
    $geometry = Control @('inspect','--pane',"$Pane")
    if ($geometry.dpi -gt 0 -and $geometry.rows -gt 0 -and $geometry.pixel_height -gt 0) {
        $geometry.pixel_height / $geometry.rows
    }
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
function Find-Element([string]$Name, [System.Windows.Automation.ControlType]$Type, [switch]$Visible) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    if ($Type) {
        $byType = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $Type)
        $condition = [System.Windows.Automation.AndCondition]::new($condition, $byType)
    }
    if ($Visible) {
        $onScreen = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::IsOffscreenProperty, $false)
        $condition = [System.Windows.Automation.AndCondition]::new($condition, $onScreen)
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
function Get-NativeMenuAction([string]$TriggerName, [string]$Name, [string]$Submenu, [switch]$AllowDisabled, [scriptblock]$Reveal) {
    $button = [System.Windows.Automation.ControlType]::Button
    $menu = [System.Windows.Automation.ControlType]::MenuItem
    try {
        Invoke-Button (Wait-Until "'$TriggerName' button" {
            $trigger = Find-Element $TriggerName $button -Visible
            if ($trigger -and $trigger.Current.IsEnabled) { return $trigger }
            if ($Reveal) { & $Reveal }
        })
    } catch {
        $trigger = Find-Element $TriggerName $button
        throw "$_ Trigger=$($trigger.Current.Name) Enabled=$($trigger.Current.IsEnabled) Offscreen=$($trigger.Current.IsOffscreen) Bounds=$($trigger.Current.BoundingRectangle)"
    }
    if ($Submenu) {
        $category = Wait-Until "'$Submenu' submenu" { Find-Element $Submenu $menu }
        $category.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    }
    try { Wait-Until "'$Name' in '$TriggerName'" {
        $item = Find-Element $Name $menu
        if ($item -and ($AllowDisabled -or $item.Current.IsEnabled)) { $item }
    } } catch {
        $item = Find-Element $Name $menu
        $category = if ($Submenu) { Find-Element $Submenu $menu }
        $trigger = Find-Element $TriggerName $button
        throw "$_ Item=$($item.Current.Name) Enabled=$($item.Current.IsEnabled) Submenu=$($category.Current.Name) Menu trigger enabled=$($trigger.Current.IsEnabled)"
    }
}
function Get-SftpFolderAction([string]$Name, [switch]$Comparison, [switch]$AllowDisabled) {
    $submenu = if ($Comparison) { 'Compare folders' }
    Get-NativeMenuAction 'More folder actions' $Name -Submenu $submenu -AllowDisabled:$AllowDisabled
}
function Invoke-SftpFolderAction([string]$Name, [switch]$Comparison) {
    Invoke-Button (Get-SftpFolderAction $Name -Comparison:$Comparison)
}
function Get-SftpEntryAction([string]$Name, [switch]$AllowDisabled) {
    Get-NativeMenuAction 'Selected entry actions' $Name -AllowDisabled:$AllowDisabled
}
function Invoke-SftpEntryAction([string]$Name) {
    Invoke-Button (Get-SftpEntryAction $Name)
}
function Invoke-NativeMenuAction([string]$Trigger, [string]$Name) {
    Invoke-Button (Get-NativeMenuAction $Trigger $Name)
}
function Get-RunAction([string]$Name) {
    $button = [System.Windows.Automation.ControlType]::Button
    $walker = [System.Windows.Automation.TreeWalker]::RawViewWalker
    $search = @{ scroll = $null }
    Get-NativeMenuAction 'Run actions' $Name -Reveal {
        $trigger = Find-Element 'Run actions' $button
        if (!$trigger -or !$trigger.Current.IsEnabled -or !$trigger.Current.IsOffscreen) { return }
        if ($search.scroll) {
            if ($search.scroll.Current.VerticalScrollPercent -lt 100) {
                $search.scroll.Scroll([System.Windows.Automation.ScrollAmount]::NoAmount,
                    [System.Windows.Automation.ScrollAmount]::SmallIncrement)
            }
            return
        }
        $parent = $walker.GetParent($trigger)
        while ($parent -and $parent.Current.ProcessId -eq $app.Id) {
            $pattern = $null
            if ($parent.TryGetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern, [ref]$pattern) -and
                $pattern.Current.VerticallyScrollable) {
                $search.scroll = $pattern
                $pattern.SetScrollPercent(-1, 0)
                return
            }
            $parent = $walker.GetParent($parent)
        }
        throw 'The run toolbar is offscreen without a scrollable ancestor'
    }
}
function Invoke-RunAction([string]$Name) {
    Invoke-Button (Get-RunAction $Name)
    $menu = [System.Windows.Automation.ControlType]::MenuItem
    $alwaysPresent = 'Compare results' + [char]0x2026
    $null = Wait-Until "the run actions flyout closes after '$Name'" {
        !(Find-Element $alwaysPresent $menu -Visible)
    }
}
function Close-NativeMenu([string]$ItemName) {
    $null = Press-FocusedKey 27
    $null = Wait-Until 'menu closes after Escape' {
        !(Find-Element $ItemName ([System.Windows.Automation.ControlType]::MenuItem) -Visible)
    }
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
function Get-NativeExpander([string]$Name) {
    $label = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $expandable = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::IsExpandCollapsePatternAvailableProperty, $true)
    $query = [System.Windows.Automation.AndCondition]::new($label, $expandable)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $element = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $query)
    if ($element -and $element.Current.IsEnabled) {
        $element.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
    }
}
function Set-NativeExpansion([string]$Name, [bool]$Expanded) {
    $expected = if ($Expanded) { 'Expanded' } else { 'Collapsed' }
    $null = Wait-Until "'$Name' is $expected" {
        $options = Get-NativeExpander $Name
        if (!$options) { return }
        $state = $options.Current.ExpandCollapseState
        if ($state -eq $expected) { return $true }
        if ($Expanded) { $options.Expand() } else { $options.Collapse() }
    }
}
function Get-SftpOptionsExpander {
    Get-NativeExpander 'File options'
}
function Set-SftpOptionsExpansion([bool]$Expanded) {
    Set-NativeExpansion 'File options' $Expanded
}
function Enable-SftpFileOption([string]$Name) {
    Set-SftpOptionsExpansion $true
    (Wait-CheckBox $Name ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Wait-CheckBox $Name ([System.Windows.Automation.ToggleState]::On)
    Set-SftpOptionsExpansion $false
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
function Wait-SftpSearchFocus([string]$Description = 'focused recursive filename field') {
    $edit = [System.Windows.Automation.ControlType]::Edit
    try {
        Wait-Until $Description {
            $field = Find-Element 'Filename contains' $edit
            if ($field -and $field.Current.HasKeyboardFocus) { $field }
        }
    } catch {
        $failure = $_
        $field = Find-Element 'Filename contains' $edit
        $state = if ($field) {
            "enabled=$($field.Current.IsEnabled), offscreen=$($field.Current.IsOffscreen), bounds=$($field.Current.BoundingRectangle)"
        } else { 'field not found' }
        $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
        $focus = if ($focused -and $focused.Current.ProcessId -eq $app.Id) {
            "$($focused.Current.ClassName) '$($focused.Current.Name)'"
        } else { 'outside the fixture window' }
        $search = $null -ne (Find-Element 'SFTP search actions')
        throw "$($failure.Exception.Message). Search visible to UIA=$search. Field: $state. Focus: $focus"
    }
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
function Wait-PickerEditor([string]$Label, [string]$Action) {
    $type = [System.Windows.Automation.ControlType]::Edit
    try {
        return Wait-Until "$Action to focus '$Label'" {
            $entry = Find-Element $Label $type
            if ($entry -and $entry.Current.HasKeyboardFocus) { $entry }
        }
    } catch {
        $mounted = $null -ne (Find-Element $Label $type)
        $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
        $focus = if ($focused -and $focused.Current.ProcessId -eq $app.Id) { $focused.Current.Name } else { '(outside fixture)' }
        throw "$_ Search edit mounted: $mounted. Foreground HWND: $([FixtureMessages]::GetForegroundWindow()), fixture HWND: $window. Focus: $focus"
    }
}
function Open-Palette([string]$Command) {
    Press-Key 0x76 # F7 in the fixture's keymap
    Wait-PickerEditor 'Search commands' "F7 for '$Command'"
}
function Run-Palette([string]$Command, [switch]$Enter) {
    $field = Open-Palette $Command
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Command)
    # UI Automation can expose the button name while WinUI is replacing its
    # provider after filtering. Reacquire the row until it is invokable.
    $null = Wait-Until "invoke palette command '$Command'" {
        try {
            $row = Find-Element $Command ([System.Windows.Automation.ControlType]::Button)
            $provider = $null
            if ($row -and $row.Current.IsEnabled -and
                $row.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$provider)) {
                if ($Enter) { Press-Key 13 } else { $provider.Invoke() }
                return $true
            }
        } catch {
            # Invoke on a retired WinUI provider can return either error.
            $cause = $_.Exception.GetBaseException()
            $retired = $cause -is [System.Windows.Automation.ElementNotAvailableException] -or
                       ($cause -is [Runtime.InteropServices.COMException] -and $cause.HResult -eq -2147467259) # E_FAIL
            if (!$retired) { throw }
        }
    }
    $null = Wait-Until "command palette to close after '$Command'" { !(Find-Element 'Search commands') }
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
function Open-PanePicker([string]$Query) {
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    Invoke-Button (Wait-Until 'Find pane menu item' { Find-Element ('Find pane' + [char]0x2026 + '    Ctrl+Shift+P') })
    Wait-PickerEditor 'Find pane' "the tab menu item for '$Query'"
}
function Open-PickerQuery([string]$Query) {
    Write-Output "Searching for $Query"
    $edit = Open-PanePicker $Query
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
