[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$realClipboard = $env:W11TERM_VERIFY_HOSTED_PASTE -ceq '1'
if ($realClipboard) {
    if ($env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_ENVIRONMENT -cne 'github-hosted') {
        throw 'Real clipboard and desktop input are restricted to a disposable GitHub-hosted runner'
    }
    Remove-Item Env:W11TERM_TEST_PASTE_FILE -ErrorAction SilentlyContinue
    Add-Type -AssemblyName System.Windows.Forms
    . (Join-Path $PSScriptRoot 'support/clipboard.ps1')
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class PastePointer {
    [StructLayout(LayoutKind.Sequential)] struct Point { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)] struct Rect { public int L, T, R, B; }
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] static extern bool ClientToScreen(IntPtr hwnd, ref Point point);
    [DllImport("user32.dll")] static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr hwnd, uint flag);
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern void mouse_event(uint flag, uint x, uint y, uint data, UIntPtr extra);
    public static void RightClick(IntPtr hwnd) {
        Rect rect;
        if (GetForegroundWindow() != hwnd || !GetClientRect(hwnd, out rect))
            throw new InvalidOperationException("Fixture window is not foreground");
        var point = new Point { X = rect.R / 2, Y = rect.B / 2 };
        if (!ClientToScreen(hwnd, ref point) || GetAncestor(WindowFromPoint(point), 2) != hwnd ||
            !SetCursorPos(point.X, point.Y) || GetForegroundWindow() != hwnd)
            throw new InvalidOperationException("Fixture terminal does not own the pointer target");
        mouse_event(8, 0, 0, 0, UIntPtr.Zero);
        mouse_event(16, 0, 0, 0, UIntPtr.Zero);
    }
}
'@
} else {
    $env:W11TERM_TEST_PASTE_FILE = Join-Path $State 'private-clipboard.txt'
}
$trace = [IO.File]::ReadAllText((Join-Path $State 'peer-trace-path.txt'))
$expected = [Collections.Generic.List[byte]]::new()
$utf8 = [Text.UTF8Encoding]::new($false,$true)
$esc = [string][char]27
$snow = [string][char]0x96ea
$button = [System.Windows.Automation.ControlType]::Button
$profiles = Join-Path $State 'profiles.json'
$account = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'palette-ssh'

function Set-FixtureClipboard([string]$Value) {
    if (!$realClipboard) { [IO.File]::WriteAllText($env:W11TERM_TEST_PASTE_FILE,$Value,$utf8); return }
    $before = [FixtureClipboard]::GetClipboardSequenceNumber()
    $null = Wait-Until 'set clipboard text on the hosted runner' {
        try {
            [Windows.Forms.Clipboard]::SetText($Value, [Windows.Forms.TextDataFormat]::UnicodeText)
            [FixtureClipboard]::ContainsNewText($before,$Value)
        } catch [Runtime.InteropServices.ExternalException] { $false }
    }
}
function Expect-Bytes([string]$Append) {
    $expected.AddRange($utf8.GetBytes($Append))
    $want = [Convert]::ToBase64String($expected.ToArray())
    try {
        $null = Wait-Until "exactly $($expected.Count) SSH input bytes" {
            [Convert]::ToBase64String((Read-PeerTrace $traceReader)) -ceq $want
        }
    } catch {
        $got = Read-PeerTrace $traceReader
        $i = 0
        while ($i -lt $expected.Count -and $i -lt $got.Length -and $expected[$i] -eq $got[$i]) { $i++ }
        throw "$_ Received $($got.Length) bytes; first difference at $i."
    }
}
function Fence([string]$Text) {
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64',[Convert]::ToBase64String($utf8.GetBytes($Text)))
    Expect-Bytes $Text
}
function Terminal-Marker([string]$Text) {
    foreach ($char in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$char) }
    Expect-Bytes $Text
}
function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" { $b = Find-Element $Name $button; if ($b -and $b.Current.IsEnabled) { $b } })
}
function Start-Paste([string]$Route) {
    if ($realClipboard -and $Route -eq 'Right') { [PastePointer]::RightClick($window); return }
    if ($realClipboard -and $Route -in 'CtrlV','CtrlAltV') {
        if ([FixtureMessages]::GetForegroundWindow() -ne $window) { throw 'Fixture does not own keyboard input' }
        $chord = if ($Route -eq 'CtrlAltV') { '^%v' } else { '^v' }
        [Windows.Forms.SendKeys]::SendWait($chord)
        return
    }
    $raw = $Route -in 'Raw','CtrlAltV'
    $name = if ($raw) { 'Paste raw (no bracketed paste)' } else { 'Paste' }
    Invoke-NativeMenuAction 'Tab menu' $name
    $null = Wait-Until 'the paste flyout closes' {
        !(Find-Element 'Settings' ([System.Windows.Automation.ControlType]::MenuItem) -Visible)
    }
}
function Wait-Prompt([string]$Primary, [string]$Reason) {
    $notice = switch ($Reason) {
        'multiple lines' { 'The clipboard contains multiple lines.' }
        '5,120' { 'The clipboard exceeds 5,120 UTF-16 code units.' }
        default { throw "Unknown confirmation reason '$Reason'" }
    }
    $null = Wait-Until 'native paste confirmation for the source SSH pane' {
        (Find-Element 'Confirm paste') -and (Find-Element $Primary $button) -and
            (Find-Element "Paste into $($account.user)@$($account.host):$($account.port) (pane $remote).") -and
            (Find-Element $notice)
    }
}
function Wait-NoPrompt {
    $null = Wait-Until 'paste confirmation dismissed' { !(Find-Element 'Confirm paste') }
}
function Wait-TerminalInput {
    Wait-NoPrompt
    # The dialog can disappear from UI Automation before WinUI delivers Closed
    # and restores the terminal key handler. Probe it with a local shortcut.
    $edit = [System.Windows.Automation.ControlType]::Edit
    $null = Wait-Until 'terminal shortcuts restored after paste confirmation' {
        if (Find-Element 'Search commands' $edit) { return $true }
        Press-Key 0x76
    }
    $null = Wait-PickerEditor 'Search commands' 'terminal input probe'
    Press-Key 27
    $null = Wait-Until 'terminal input probe closed' { !(Find-Element 'Search commands' $edit) }
}
function Finish-ObsoletePrompt($OldButton) {
    if (Find-Element 'Confirm paste') {
        try { if ($OldButton.Current.IsEnabled) { Invoke-Button $OldButton } }
        catch [System.Windows.Automation.ElementNotAvailableException] { } # removed concurrently
    }
    Wait-NoPrompt
}
function Get-MultilinePasteChoice {
    Find-Element 'Multiline paste warning' ([System.Windows.Automation.ControlType]::ComboBox)
}
function Get-SelectedMultilinePasteChoice {
    $field = Get-MultilinePasteChoice
    if ($field) {
        $selection = @($field.GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection())
        if ($selection.Count -eq 1) { $selection[0].Current.Name }
    }
}
function Choose-Setting([string]$Name) {
    $field = Wait-Until 'multiline paste warning dropdown' { Get-MultilinePasteChoice }
    $field.SetFocus()
    $field.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $choice = Wait-Until "visible multiline paste option '$Name'" {
        Find-Element $Name ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until "multiline paste option '$Name' selected" {
        (Get-SelectedMultilinePasteChoice) -ceq $Name
    }
    $dropdown = (Get-MultilinePasteChoice).GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
    if ($dropdown.Current.ExpandCollapseState -eq 'Expanded') { $dropdown.Collapse() }
}
function Read-InteractionSettings {
    $share = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
    $stream = [IO.File]::Open($env:W11TERM_SETTINGS_PATH, [IO.FileMode]::Open, [IO.FileAccess]::Read, $share)
    $reader = [IO.StreamReader]::new($stream)
    try { ($reader.ReadToEnd() | ConvertFrom-Json).interaction } finally { $reader.Dispose() }
}
function Back-ToSsh {
    $tab = Wait-Until 'SSH terminal tab' { Find-Element 'palette-ssh' ([System.Windows.Automation.ControlType]::TabItem) }
    $tab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Wait-Focus $remote
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $traceReader = Open-PeerTraceReader $trace
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    Set-FixtureClipboard 'first/'
    Start-Paste 'Right'
    Expect-Bytes 'first/'
    $plain = "two$([char]10)lines"
    Set-FixtureClipboard $plain
    Start-Paste 'CtrlV'
    Wait-Prompt 'Paste' 'multiple lines'
    Click 'Cancel'
    Wait-TerminalInput
    Terminal-Marker 'after-cancel/'
    Start-Paste 'CtrlV'
    Wait-Prompt 'Paste' 'multiple lines'
    Set-FixtureClipboard 'discard-replacement'
    Click 'Paste'
    Wait-NoPrompt
    Expect-Bytes ("two" + [char]13 + 'lines')

    $mode = $esc + '[?2004hbrackets-on'
    Fence $mode
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','brackets-on')
    $crlf = [Environment]::NewLine
    $wrapped = "wrapped $snow$crlf"
    Set-FixtureClipboard $wrapped
    Start-Paste 'Normal'
    Expect-Bytes ($esc + '[200~' + $wrapped + $esc + '[201~')
    $raw = "raw $snow$crlf" + [char]9 + " $crlf"
    Set-FixtureClipboard $raw
    Start-Paste 'CtrlAltV'
    Wait-Prompt 'Paste raw' 'multiple lines'
    Click 'Paste raw'
    Wait-NoPrompt
    Expect-Bytes $raw

    # UTF-16 measurement has a different boundary from both UTF-8 bytes and scalars.
    $atLimit = [char]::ConvertFromUtf32(0x1f600) * 2560
    Set-FixtureClipboard $atLimit
    Start-Paste 'Raw'
    Expect-Bytes $atLimit
    Set-FixtureClipboard ($atLimit + '!')
    Start-Paste 'Raw'
    Wait-Prompt 'Paste raw' '5,120'
    Click 'Cancel'
    Wait-TerminalInput
    Terminal-Marker 'after-large/'

    Run-Palette 'Settings'
    $null = Wait-Until 'Automatic multiline paste option selected by default' {
        (Get-SelectedMultilinePasteChoice) -ceq 'Automatic'
    }
    Choose-Setting 'Always'
    $null = Wait-Until 'saved Always multiline preference' {
        (Read-InteractionSettings).multiLinePasteWarning -ceq 'always'
    }
    Back-ToSsh
    Set-FixtureClipboard $wrapped
    Start-Paste 'Normal'
    Wait-Prompt 'Paste' 'multiple lines'
    Click 'Cancel'
    Wait-TerminalInput

    # A reconnect may close the dialog immediately or invalidate its ticket
    # on acceptance. Either path must leave the new transport untouched.
    Set-FixtureClipboard ("discard-stale" + $crlf + 'generation')
    Start-Paste 'Raw'
    Wait-Prompt 'Paste raw' 'multiple lines'
    $oldButton = Find-Element 'Paste raw' $button
    $before = (Control @('list')).sessions[0].connection_generation
    Fence ([string][char]4)
    $null = Wait-Until 'new SSH generation after fixture drops the link' {
        (Control @('list')).sessions[0].connection_generation -gt $before
    }
    Finish-ObsoletePrompt $oldButton
    Fence 'after-reconnect/'

    Set-FixtureClipboard ("discard-focus" + $crlf + 'wrong-pane')
    Start-Paste 'Raw'
    Wait-Prompt 'Paste raw' 'multiple lines'
    $oldButton = Find-Element 'Paste raw' $button
    $local = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $local
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    Finish-ObsoletePrompt $oldButton
    if ((Control @('get-text','--pane',"$local")).text.Contains('discard-focus')) { throw 'Pending SSH text reached PowerShell' }
    Back-ToSsh
    Run-Palette 'Settings'
    $null = Wait-Until 'saved Always multiline paste option restored in Settings' {
        (Get-SelectedMultilinePasteChoice) -ceq 'Always'
    }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $byName = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty,'Warn about large pastes')
    $canToggle = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::IsTogglePatternAvailableProperty,$true)
    $condition = [System.Windows.Automation.AndCondition]::new($byName,$canToggle)
    $toggle = Wait-Until 'large paste warning switch' { $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,$condition) }
    $toggle.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
    $null = Wait-Until 'saved large paste warning is off' {
        (Read-InteractionSettings).largePasteWarning -eq $false
    }
    Choose-Setting 'Never'
    $null = Wait-Until 'both warning preferences saved' {
        $settings = Read-InteractionSettings
        $settings.multiLinePasteWarning -ceq 'never' -and $settings.largePasteWarning -eq $false
    }
    Back-ToSsh
    $unprompted = $atLimit + $crlf + 'disabled'
    Set-FixtureClipboard $unprompted
    Start-Paste 'Raw'
    Expect-Bytes $unprompted
    Fence 'ssh-still-live/'
} catch {
    throw "$_ at $($_.ScriptStackTrace). Peer received $((Get-Item -LiteralPath $trace).Length) bytes."
} finally {
    if ($traceReader) { $traceReader.Dispose() }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
