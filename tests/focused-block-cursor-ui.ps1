[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/cursor-cell.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_EXPORT_SMOKE_DIR = $State
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"font_family":"Consolas","font_size":24,"shortcuts":{"command_palette":"F7","split_right":"F8","focus_left":"F6","focus_right":"F9"}}')
$profiles = Join-Path $State 'profiles.json'
$esc = [string][char]27
$bell = [string][char]7

function Send-Sequence([int]$Pane, [string]$Text) {
    $bytes = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($Text))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$bytes)
}
function Hidden-Cell([int]$Pane, [string]$Marker, [string]$Color) {
    Send-Sequence $Pane ($esc + ']10;#ffffff' + $bell + $esc + ']11;#000000' + $bell +
        $esc + ']12;' + $Color + $bell + $esc + '[?25l' + $esc + '[2J' +
        $esc + '[6;4H' + $Marker + $esc + '[6;4H' + $esc + '[2 q')
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15',$Marker)
    $cell = Get-CursorCell 'palette-ssh' $Marker
    # Capture the exact original H as a mask, including this display's rasterization.
    $reference = Wait-Until "white $Marker glyph painted on black" {
        $pixels = [CursorCellProbe]::ReadPixels($window,$window,$cell.X,$cell.Y,$cell.Width,$cell.Height)
        $bright = 0
        foreach ($pixel in $pixels) {
            if (($pixel -band 255) -gt 175 -and (($pixel -shr 8) -band 255) -gt 175 -and
                (($pixel -shr 16) -band 255) -gt 175) { $bright++ }
        }
        if ($bright -ge 5) { return ,$pixels }
    }
    return @{ Bounds=$cell; Reference=$reference; Color=$Color; Name=$Marker }
}
function Measure-Paint($Probe, [IntPtr]$Foreground) {
    $cell = $Probe.Bounds
    $pixels = [CursorCellProbe]::ReadPixels($window,$Foreground,$cell.X,$cell.Y,$cell.Width,$cell.Height)
    $counts = @{ Area=$pixels.Length; Color=0; LastColumn=-1; Core=0; ColorInCore=0; Ink=0; DarkInk=0; WhiteInk=0 }
    for ($i=0; $i -lt $pixels.Length; $i++) {
        $x = $i % $cell.Width
        $y = [Math]::Floor($i / $cell.Width)
        $core = $x -ge $cell.Width * 0.2 -and $x -lt $cell.Width * 0.8 -and
                $y -ge $cell.Height * 0.2 -and $y -lt $cell.Height * 0.8
        if ($core) { $counts.Core++ }
        $r = ($pixels[$i] -shr 16) -band 255
        $g = ($pixels[$i] -shr 8) -band 255
        $b = $pixels[$i] -band 255
        $colored = if ($Probe.Color -eq '#ff00ff') { $r -gt 160 -and $b -gt 160 -and $g -lt 110 }
                   else { $g -gt 160 -and $r -lt 110 -and $b -lt 110 }
        if ($colored) {
            $counts.Color++
            $counts.LastColumn = [Math]::Max($counts.LastColumn,$x)
            if ($core) { $counts.ColorInCore++ }
        }
        $old = $Probe.Reference[$i]
        if ($core -and (($old -shr 16) -band 255) -gt 175 -and (($old -shr 8) -band 255) -gt 175 -and ($old -band 255) -gt 175) {
            $counts.Ink++
            if ($r -lt 90 -and $g -lt 90 -and $b -lt 90) { $counts.DarkInk++ }
            if ($r -gt 175 -and $g -gt 175 -and $b -gt 175) { $counts.WhiteInk++ }
        }
    }
    return $counts
}
function Expect-Style($MarkedCell, [string]$Style, [IntPtr]$Foreground = $window) {
    $name = "$($MarkedCell.Name) $Style cursor"
    try {
        $null = Wait-Until $name {
            $m = Measure-Paint $MarkedCell $Foreground
            if ($m.Ink -lt 3 -or $m.Color -lt 4) { return $false }
            switch ($Style) {
                'filled' { return $m.ColorInCore -gt $m.Core * 0.3 -and $m.DarkInk -ge $m.Ink * 0.65 }
                'hollow' { return $m.ColorInCore -eq 0 -and $m.Color -lt $m.Area * 0.4 -and
                                  $m.WhiteInk -ge $m.Ink * 0.7 -and $m.LastColumn -gt $MarkedCell.Bounds.Width * 0.6 }
                'bar'    { return $m.LastColumn -lt $MarkedCell.Bounds.Width * 0.38 -and $m.WhiteInk -ge $m.Ink * 0.7 }
            }
        }
    } catch {
        throw "$_ ($name). Pixel counts: $(Measure-Paint $MarkedCell $Foreground | ConvertTo-Json -Compress)"
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','80','--rows','16') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $left = ((Control @('list')).sessions | Where-Object active).id
    $null = Control @('wait-for-text','--pane',"$left",'--timeout','15','ready')
    Show-CursorWindow
    Press-Key 0x77
    $right = Wait-Until 'active right SSH split' {
        $sessions = @((Control @('list')).sessions)
        $pane = $sessions | Where-Object id -ne $left
        if ($sessions.Count -eq 2 -and $pane.active -and $sessions[0].tab_id -eq $sessions[1].tab_id) { $pane.id }
    }
    $null = Control @('wait-for-text','--pane',"$right",'--timeout','15','ready')
    $green = Hidden-Cell $right 'HRightProof' '#00ff00'
    Press-Key 0x75
    Wait-Focus $left
    $magenta = Hidden-Cell $left 'HLeftProof' '#ff00ff'
    if ($magenta.Bounds.X -ge $green.Bounds.X) { throw 'Markers did not resolve to two distinct side-by-side pane surfaces' }
    Send-Sequence $left ($esc + '[?25h')
    Send-Sequence $right ($esc + '[?25h')
    Expect-Style $magenta filled
    Expect-Style $green hollow

    Press-Key 0x78
    Wait-Focus $right
    Expect-Style $magenta hollow
    Expect-Style $green filled
    Send-Sequence $right ($esc + '[6 q')
    Expect-Style $green bar
    Send-Sequence $right ($esc + '[2 q')
    Expect-Style $green filled

    Run-Palette 'Find in scrollback    Ctrl+Shift+F'
    $null = Wait-Until 'inline Find to own keyboard focus' {
        $box = Find-Element 'Find in scrollback'
        $box -and $box.Current.HasKeyboardFocus
    }
    Expect-Style $magenta hollow
    Expect-Style $green hollow
    Invoke-Button (Find-Element 'Close find (Escape)')
    Expect-Style $green filled

    # Give a verified fixture-owned native dialog foreground and keep both
    # terminal cells unobscured, without activating someone else's application.
    $label = 'Save scrollback as text' + [char]0x2026
    $field = Open-Palette $label
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($label)
    Invoke-Button (Wait-Until 'enabled export palette action' {
        $action = Find-Element $label ([System.Windows.Automation.ControlType]::Button)
        if ($action -and $action.Current.IsEnabled) { $action }
    })
    $dialog = Wait-Until 'fixture-owned save dialog' { Find-OwnedDialog 'Save scrollback as text' }
    $null = Wait-Until 'foreground save dialog' {
        if ([FixtureMessages]::GetForegroundWindow() -ne $dialog) {
            $null = [CursorCellProbe]::SetForegroundWindow($dialog)
        }
        [FixtureMessages]::GetForegroundWindow() -eq $dialog
    }
    $below = [Math]::Max($green.Bounds.Y + $green.Bounds.Height,$magenta.Bounds.Y + $magenta.Bounds.Height) + 50
    if (![CursorCellProbe]::PositionDialog($dialog,$magenta.Bounds.X,$below)) { throw 'Could not position fixture dialog below cursor cells' }
    Expect-Style $magenta hollow $dialog
    Expect-Style $green hollow $dialog
    $null = Wait-Until 'cancel fixture save dialog' { [FixtureDialog]::ClickButton($dialog,2) }
    $null = Wait-Until 'restore terminal foreground' { [FixtureMessages]::GetForegroundWindow() -eq $window }
    Wait-Focus $right
    Expect-Style $green filled
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
