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
    $null = Wait-Until "BEL journal event on pane $Pane" {
        (Control @('events','--pane',"$Pane",'--since',"$Since",'--timeout','0')).journal.events |
            Where-Object kind -eq 'bell'
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
       Height = [int][Math]::Ceiling(4 * $scale) }
}
function Has-Outline($Edge, [switch]$Green) {
    $pixels = [CursorCellProbe]::ReadPixels($window,$window,$Edge.X,$Edge.Y,8,$Edge.Height)
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
    Send-Sequence $left $bell
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.ElapsedMilliseconds -lt 650) {
        if ((Has-Outline $leftEdge) -or (Has-Outline $rightEdge)) { throw 'Visual bell drew with its setting disabled' }
        Start-Sleep -Milliseconds 20
    }
    Wait-BellEvent $left $since

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
    $before = (Control @('get-text','--pane',"$left")).generation
    $since = Bell-Cursor $left
    $timer.Restart()
    Send-Sequence $left $bell
    $null = Wait-Until 'gold outline on the left pane only' {
        if (Has-Outline $rightEdge) { throw 'Bell drew on the focused neighbor' }
        Has-Outline $leftEdge
    }
    $null = Wait-Until 'outline expires while the SSH pane is idle' { !(Has-Outline $leftEdge) }
    if (Has-Outline $rightEdge) { throw 'Outline appeared on the neighboring pane' }
    Wait-BellEvent $left $since
    Wait-Focus $right
    if ((Control @('get-text','--pane',"$left")).generation -ne $before) {
        throw 'Visual bell changed committed terminal contents'
    }
    $null = Wait-Until 'the per-pane bell cooldown has elapsed' { $timer.ElapsedMilliseconds -ge 1050 }
    Send-Sequence $left ($esc + ']21;visual_bell=#00e850' + $esc + '\' + $bell)
    $null = Wait-Until 'application-set green outline on the left pane only' {
        if (Has-Outline $rightEdge -Green) { throw 'Custom bell drew on the focused neighbor' }
        Has-Outline $leftEdge -Green
    }
    $null = Wait-Until 'custom outline expires with idle output' { !(Has-Outline $leftEdge -Green) }
    Wait-Focus $right
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
