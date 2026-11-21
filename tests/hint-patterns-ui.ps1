# The clipboard assertion is opt-in for disposable CI desktops.
[CmdletBinding()]
param([string]$Gui, [string]$Cli, [switch]$VerifyClipboard)
$ErrorActionPreference = 'Stop'
if (!$Gui) { $Gui = Join-Path $PSScriptRoot '..\target\debug\w11term.exe' }
if (!$Cli) { $Cli = Join-Path $PSScriptRoot '..\target\debug\w11term-cli.exe' }
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
if ($VerifyClipboard) {
    # Read the same CF_UNICODETEXT handle the GUI publishes. Windows PowerShell's
    # Get-Clipboard uses OLE and can fail to marshal data on a hosted desktop.
    Add-Type @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class HintClipboard {
    [DllImport("user32.dll")] public static extern uint GetClipboardSequenceNumber();
    [DllImport("user32.dll", SetLastError=true)] static extern bool OpenClipboard(IntPtr owner);
    [DllImport("user32.dll")] static extern bool CloseClipboard();
    [DllImport("user32.dll")] static extern IntPtr GetClipboardData(uint format);
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr GlobalLock(IntPtr handle);
    [DllImport("kernel32.dll")] static extern bool GlobalUnlock(IntPtr handle);
    public static bool ContainsNewText(uint before, string expected) {
        if (GetClipboardSequenceNumber() == before) return false;
        if (!OpenClipboard(IntPtr.Zero)) {
            int error = Marshal.GetLastWin32Error();
            if (error == 5 || error == 170) return false; // in use, retry within Wait-Until
            throw new Win32Exception(error);
        }
        try {
            IntPtr handle = GetClipboardData(13); // CF_UNICODETEXT
            if (handle == IntPtr.Zero) return false;
            IntPtr text = GlobalLock(handle);
            if (text == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
            try { return String.Equals(Marshal.PtrToStringUni(text), expected, StringComparison.Ordinal); }
            finally { GlobalUnlock(handle); }
        } finally { CloseClipboard(); }
    }
}
'@
}
$state = (New-Item -ItemType Directory -Force -Path (Join-Path $PSScriptRoot ('..\.local\hint-pattern-smoke\' + [guid]::NewGuid().ToString('N')))).FullName
$profiles = Join-Path $state 'profiles.json'
$replay = Join-Path $state 'pattern-fixture.vt'
[IO.File]::WriteAllText($profiles, '{"profiles":{}}')
[IO.File]::WriteAllText($replay, 'OPS-104 BUG-203')
$oldData = $env:W11TERM_DATA_DIR
$oldSettings = $env:W11TERM_SETTINGS_PATH
$app = $null

function Click([string]$Name) {
    Invoke-Button (Wait-Until $Name {
        $button = Find-Element $Name ([System.Windows.Automation.ControlType]::Button)
        if ($button -and $button.Current.IsEnabled) { $button }
    })
}
function Edit-Value([string]$Name, [string]$Value) {
    $edit = Wait-Until $Name { Find-Element $Name ([System.Windows.Automation.ControlType]::Edit) }
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value)
}
function Replace-RequiredValue([string]$Name, [string]$Value) {
    # SetValue changes the native textbox before Reactor receives TextChanged.
    # When the other field is populated, Save's state acknowledges each edit.
    Edit-Value $Name ''
    $null = Wait-Until "editor to recognize an empty $Name" {
        $save = Find-Element 'Save pattern'
        $save -and -not $save.Current.IsEnabled
    }
    Edit-Value $Name $Value
    $null = Wait-Until "editor to accept $Name '$Value'" {
        $save = Find-Element 'Save pattern'
        $save -and $save.Current.IsEnabled
    }
}
function Save-Rule([string]$Name, [string]$Expression) {
    Click 'Save pattern'
    $null = Wait-Until 'saved pattern list' { Find-Element 'Add pattern' }
    $rules = @((Get-Content -Raw -LiteralPath $env:W11TERM_SETTINGS_PATH | ConvertFrom-Json).hint_patterns)
    if ($rules.Count -ne 1 -or $rules[0].name -cne $Name -or $rules[0].regex -cne $Expression) {
        throw "Expected saved rule '$Name' / '$Expression'. Found: $($rules | ConvertTo-Json -Compress)"
    }
    $null = Wait-Until "rendered rule '$Name' / '$Expression'" {
        (Find-Element "Edit pattern $Name") -and (Find-Element $Expression ([System.Windows.Automation.ControlType]::Text))
    }
}
function Show-Patterns {
    Click 'Tab menu'
    Invoke-Button (Wait-Until 'settings menu' { Find-Element 'Settings' ([System.Windows.Automation.ControlType]::MenuItem) })
    Click 'Hint patterns'
}
function Show-Pane {
    Click 'Back to settings'
    Picker-Query 'pattern-fixture.vt'
    Press-Key 13
    $null = Wait-Until 'closed pane picker' { -not (Find-Element 'Find pane' ([System.Windows.Automation.ControlType]::Edit)) }
    Wait-Focus $pane
}
function Hint-Message {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $texts = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $texts) |
        ForEach-Object { $_.Current.Name } | Where-Object { $_ -match 'Esc (cancel|close)' } | Select-Object -First 1
}
function Open-Hints {
    # The replay can reach the control pipe before WinUI has drawn or retitled
    # the window. KeyboardBridge only accepts messages for the terminal HWND.
    $script:window = Wait-Until 'rendered replay window ready for keyboard input' {
        $app.Refresh()
        if ($app.MainWindowTitle -like 'w11term*pattern-fixture.vt' -and
            $app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle }
    }
    # The fixture starts with an 80x24 PTY and a 1120x760 DIP window. Its
    # first canvas-sized grid replaces the startup grid and cancels any hints
    # frozen on the old grid. Wait for the measured viewport to be published.
    $null = Wait-Until 'canvas-sized replay frame' {
        $box = Control @('get-text','--pane',"$pane")
        $box.cols -ne 80 -or $box.rows -ne 24
    }
    Press-Key 0x77 # the remapped F8 shortcut
}
function Test-Match([string]$Name, [string]$Match) {
    Open-Hints
    $label = $Name + ' ' + [char]0xb7 + ' Enter copy ' + [char]0xb7 + ' Esc cancel   ' + $Match
    try {
        $null = Wait-Until "keyboard hints ready for $Match" { (Hint-Message) -like 'Type a hint label*' }
        Press-Key 0x41 # label a
        $null = Wait-Until "hint for $Match" { Find-Element $label }
    } catch {
        $saved = (Get-Content -Raw -LiteralPath $env:W11TERM_SETTINGS_PATH | ConvertFrom-Json).hint_patterns
        throw "$_ Rendered hint: '$(Hint-Message)'. Saved rules: $($saved | ConvertTo-Json -Compress)"
    }
    if ($VerifyClipboard) {
        $before = [HintClipboard]::GetClipboardSequenceNumber()
        Press-Key 13
    } else {
        Press-Key 27
    }
    # The hint closes after the clipboard writer has released its Win32 handle.
    $null = Wait-Until 'dismissed hints' { -not (Find-Element $label) }
    if ($VerifyClipboard) {
        $null = Wait-Until 'copying the exact match' { [HintClipboard]::ContainsNewText($before, $Match) }
    }
}
function Stop-Fixture {
    if ($app -and -not $app.HasExited) {
        $null = $app.CloseMainWindow()
        if (-not $app.WaitForExit(3000)) { $app.Kill(); $app.WaitForExit() }
    }
}

try {
    $env:W11TERM_DATA_DIR = $state
    $env:W11TERM_SETTINGS_PATH = Join-Path $state 'settings.json'
    [IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"keyboard_hints":"F8"}}')
    for ($launch = 1; $launch -le 2; $launch++) {
        $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--replay', ('"' + $replay + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $state "run-$launch.log")
        $window = Wait-Until 'window handle' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
        $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
        $pane = (Control @('list')).sessions[0].id
        $null = Control @('wait-for-text','--pane',"$pane",'--timeout','10','OPS-104 BUG-203')
        if ($launch -eq 1) {
            Show-Patterns
            Click 'Add pattern'
            Edit-Value 'Pattern name' 'Issue'
            Edit-Value 'Regular expression' 'x*'
            Click 'Save pattern'
            $null = Wait-Until 'validation of empty matches' { Find-Element 'The expression must match at least one character every time.' }
            Replace-RequiredValue 'Regular expression' '\bOPS-[0-9]+\b'
            Save-Rule 'Issue' '\bOPS-[0-9]+\b'
            Show-Pane
        }
        Test-Match 'Issue' 'OPS-104'
        if ($launch -eq 2) {
            Show-Patterns
            Click 'Edit pattern Issue'
            Replace-RequiredValue 'Pattern name' 'Bug'
            Replace-RequiredValue 'Regular expression' 'BUG-[0-9]+'
            Save-Rule 'Bug' 'BUG-[0-9]+'
            Show-Pane
            Test-Match 'Bug' 'BUG-203'
            Show-Patterns
            Click 'Edit pattern Bug'
            Click 'Remove pattern'
            $null = Wait-Until 'rule removal' { Find-Element 'Add pattern' }
            Show-Pane
            Open-Hints
            $null = Wait-Until 'no matches after removal' { Find-Element ('No matching hints in this pane ' + [char]0xb7 + ' Esc close') }
            $settings = Get-Content -Raw -LiteralPath $env:W11TERM_SETTINGS_PATH | ConvertFrom-Json
            if ($settings.hint_patterns) { throw 'Removed pattern was still saved' }
            Press-Key 27
        }
        Stop-Fixture
    }
    Write-Output 'Named hint patterns passed native add, edit, selection, restart and removal.'
} finally {
    Stop-Fixture
    $env:W11TERM_DATA_DIR = $oldData
    $env:W11TERM_SETTINGS_PATH = $oldSettings
}
