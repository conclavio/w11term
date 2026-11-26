[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/cursor-cell.ps1')
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class BellFixtureDpi {
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr window);
}
'@
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"font_family":"Consolas","shortcuts":{"command_palette":"F7","split_right":"F8","focus_left":"F6","focus_right":"F9"}}')
$profiles = Join-Path $State 'profiles.json'
$esc = [string][char]27
$bell = [string][char]7

function Send-Sequence([int]$Pane, [string]$Text) {
    $bytes = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($Text))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$bytes)
}
function Bell-Cursor([int]$Pane) {
    (Control @('events','--pane',"$Pane",'--since','0','--timeout','0')).journal.next
}
function Wait-BellEvent([int]$Pane, [uint64]$Since) {
    $journal = (Control @('events','--pane',"$Pane",'--since',"$Since",'--kind','bell','--timeout','15')).journal
    if ($journal.lost -or !$journal.events.Count -or @($journal.events | Where-Object kind -ne 'bell').Count) {
        throw "Expected only new BEL journal events on pane $Pane"
    }
}
function Measure-TopEdge([int]$Pane, [string]$Marker) {
    Send-Sequence $Pane "$esc[?25l$esc[2J$esc[H$Marker"
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15',$Marker)
    $cell = Get-CursorCell 'palette-ssh' $Marker
    $scale = [BellFixtureDpi]::GetDpiForWindow($window) / 96.0
    # The terminal starts its first cell 12 DIPs inside the canvas. The visual
    # bell's gold stroke is 1–3 DIPs from the top edge, inside that padding.
    @{ X = $cell.X + [int][Math]::Round(10 * $scale)
       Y = $cell.Y - [int][Math]::Round(11 * $scale)
       Height = [int][Math]::Ceiling(4 * $scale)
       Step = [int][Math]::Ceiling(16 * $scale) }
}
function Read-Outline($Edge, [switch]$Green) {
    # Either pane's top stroke is continuous. Try several nearby spots when
    # a transient desktop window or a flyout covers the first one.
    $pixels = $null
    foreach ($offset in 0,1,2,3) {
        $pixels = [CursorCellProbe]::TryReadPixels($window, ($Edge.X + $offset * $Edge.Step), $Edge.Y, 8, $Edge.Height)
        if ($null -ne $pixels) { break }
    }
    if ($null -eq $pixels) { return $null }
    foreach ($pixel in $pixels) {
        $r = ($pixel -shr 16) -band 255
        $g = ($pixel -shr 8) -band 255
        $b = $pixel -band 255
        if ($Green) {
            if ($r -lt 80 -and $g -gt 190 -and $b -lt 125) { return $true }
        } elseif ($r -gt 185 -and $g -gt 105 -and $g -lt 220 -and $b -lt 100) { return $true }
    }
    return $false
}
function Read-Outlines([switch]$Green) {
    $rightOutline = Read-Outline $rightEdge -Green:$Green
    $leftOutline = Read-Outline $leftEdge -Green:$Green
    if ($null -ne $rightOutline -and $null -ne $leftOutline) {
        @{ Left = $leftOutline; Right = $rightOutline }
    }
}
function Get-BellSetting {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $name = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty,'Show a visual bell')
    $toggle = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::IsTogglePatternAvailableProperty,$true)
    $element = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.AndCondition]::new($name,$toggle))
    if ($element) { $element.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern) }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','80','--rows','16') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $left = ((Control @('list')).sessions | Where-Object active).id
    $null = Control @('wait-for-text','--pane',"$left",'--timeout','15','ready')
    Show-CursorWindow
    Press-Key 0x77 # F8, split right on the same connection.
    $right = Wait-Until 'active right SSH split' {
        $sessions = @((Control @('list')).sessions)
        $pane = $sessions | Where-Object id -ne $left
        if ($sessions.Count -eq 2 -and $pane.active -and $sessions[0].tab_id -eq $sessions[1].tab_id) { $pane.id }
    }
    $null = Control @('wait-for-text','--pane',"$right",'--timeout','15','ready')
    $rightEdge = Measure-TopEdge $right 'RightBellProof'
    Press-Key 0x75 # F6, focus left long enough to resolve its native bounds.
    Wait-Focus $left
    $leftEdge = Measure-TopEdge $left 'LeftBellProof'
    if ($leftEdge.X -ge $rightEdge.X) { throw 'Expected two separate pane canvases' }
    Press-Key 0x78 # F9, keep focus in the right pane for testing.
    Wait-Focus $right

    $since = Bell-Cursor $left
    Send-Sequence $left "$esc]2;UnrelatedTitle$esc\"
    $title = (Control @('events','--pane',"$left",'--since',"$since",'--kind','title','--timeout','15')).journal
    if ($title.lost -or !($title.events | Where-Object { $_.data.title -eq 'UnrelatedTitle' })) {
        throw 'The background SSH pane did not publish its title event'
    }
    $withoutBell = (Control @('events','--pane',"$left",'--since',"$since",'--kind','bell','--timeout','1')).journal
    if ($withoutBell.lost -or $withoutBell.events.Count -or $withoutBell.next -lt $title.next) {
        throw 'The filtered bell wait returned a title event or failed to advance its cursor'
    }
    $since = $withoutBell.next
    $null = Wait-Until 'both pane edges to be visible before the disabled bell' { Read-Outlines }
    Send-Sequence $left $bell
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.ElapsedMilliseconds -lt 650) {
        $outlines = Read-Outlines
        if (!$outlines) { throw 'Could not observe both pane edges while the visual bell was disabled' }
        if ($outlines.Left -or $outlines.Right) { throw 'Visual bell drew with its setting disabled' }
        Start-Sleep -Milliseconds 20
    }
    Wait-BellEvent $left $since
    $sound = (Control @('events','--pane',"$left",'--since',"$since",'--kind','sound','--timeout','15')).journal
    if ($sound.lost -or !($sound.events | Where-Object { $_.data.outcome -eq 'muted' })) {
        throw 'The muted bell did not publish its sound outcome'
    }

    Run-Palette 'Settings'
    $setting = Wait-Until 'visual bell setting defaults to off' {
        $switch = Get-BellSetting
        if ($switch -and $switch.Current.ToggleState -eq 'Off') { $switch }
    }
    $setting.Toggle()
    $null = Wait-Until 'visual bell setting is on and persisted' {
        (Get-BellSetting).Current.ToggleState -eq 'On' -and
            (Get-Content -LiteralPath $env:W11TERM_SETTINGS_PATH -Raw | ConvertFrom-Json).interaction.visualBell -eq $true
    }
    $tab = Wait-Until 'SSH split tab' { Find-Element 'palette-ssh' ([System.Windows.Automation.ControlType]::TabItem) }
    $tab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Wait-Focus $right
    Show-CursorWindow
    $null = Wait-Until 'both pane edges to be visible after leaving Settings' { Read-Outlines }
    $before = (Control @('get-text','--pane',"$left")).generation
    $since = Bell-Cursor $left
    Send-Sequence $left $bell
    $null = Wait-Until 'gold outline on the left pane only' {
        $outlines = Read-Outlines
        if ($outlines.Right) { throw 'Bell drew on the focused neighbor' }
        $outlines.Left
    }
    # The renderer starts the cooldown when it receives the bell, after the
    # CLI has sent it. Anchor this interval to the observed stroke.
    $timer.Restart()
    $null = Wait-Until 'outline expires while the SSH pane is idle' {
        $outlines = Read-Outlines
        if ($outlines.Right) { throw 'Outline appeared on the neighboring pane' }
        $outlines -and !$outlines.Left
    }
    Wait-BellEvent $left $since
    Wait-Focus $right
    if ((Control @('get-text','--pane',"$left")).generation -ne $before) {
        throw 'Visual bell changed committed terminal contents'
    }
    $null = Wait-Until 'the per-pane bell cooldown has elapsed' { $timer.ElapsedMilliseconds -ge 1050 }
    $since = Bell-Cursor $left
    Send-Sequence $left ($esc + ']21;visual_bell=#00e850' + $esc + '\' + $bell)
    $null = Wait-Until 'application-set green outline on the left pane only' {
        $outlines = Read-Outlines -Green
        if ($outlines.Right) { throw 'Custom bell drew on the focused neighbor' }
        $outlines.Left
    }
    $null = Wait-Until 'custom outline expires with idle output' {
        $outlines = Read-Outlines -Green
        if ($outlines.Right) { throw 'Custom outline appeared on the neighboring pane' }
        $outlines -and !$outlines.Left
    }
    Wait-BellEvent $left $since
    Wait-Focus $right
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
