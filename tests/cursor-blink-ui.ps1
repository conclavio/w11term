[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_TEST_CARET_BLINK_MS = '200'
$profiles = Join-Path $State 'profiles.json'

# Sample only the marked cell on the fixture's foreground surface. Return a
# fingerprint in memory so the test can distinguish cursor phases on the GPU canvas.
Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Drawing;
using System.Runtime.InteropServices;
public static class CursorCellProbe {
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr window, int command);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr window);
    [DllImport("user32.dll")] static extern IntPtr GetAncestor(IntPtr hwnd, uint flags);
    [DllImport("user32.dll")] static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    public static long Sample(IntPtr window, int x, int y, int width, int height) {
        if (width < 1 || width > 64 || height < 1 || height > 128)
            throw new ArgumentOutOfRangeException("cell dimensions");
        IntPtr prior = SetThreadDpiAwarenessContext(new IntPtr(-4));
        try {
            if (GetForegroundWindow() != window) throw new InvalidOperationException("Fixture lost foreground");
            foreach (int col in new[] { x, x + width - 1 })
                foreach (int row in new[] { y, y + height - 1 })
                    if (GetAncestor(WindowFromPoint(new Point(col, row)), 2) != window)
                        throw new InvalidOperationException("Fixture cursor cell is obscured");
            using (var image = new Bitmap(width, height)) {
                using (var g = Graphics.FromImage(image))
                    g.CopyFromScreen(x, y, 0, 0, image.Size, CopyPixelOperation.SourceCopy);
                unchecked {
                    long value = 17;
                    for (int row = 0; row < height; row++)
                        for (int col = 0; col < width; col++)
                            value = value * 31 + image.GetPixel(col, row).ToArgb();
                    return value;
                }
            }
        } finally { SetThreadDpiAwarenessContext(prior); }
    }
}
'@

function Sample-Cell {
    [CursorCellProbe]::Sample($window, $x, $y, $cw, $ch)
}
function Read-Generation {
    (Control @('get-text','--pane',"$pane")).generation
}
function Send-Sequence([string]$Suffix) {
    $before = Read-Generation
    $bytes = [Text.Encoding]::ASCII.GetBytes(([string][char]27) + $Suffix)
    $null = Control @('send-text','--pane',"$pane",'--no-paste','--base64',[Convert]::ToBase64String($bytes))
    Wait-Until "SSH peer to echo $Suffix" { $g = Read-Generation; if ($g -gt $before) { $g } }
}

function Assert-Blinking {
    $last = $null
    $transitions = 0
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($transitions -lt 3 -and $timer.ElapsedMilliseconds -lt 6000) {
        $sample = Sample-Cell
        if ($sample -ne $shown -and $sample -ne $hidden) { throw 'Glyph or cell content changed during blink' }
        if ($null -ne $last -and $sample -ne $last) { $transitions++ }
        $last = $sample
        Start-Sleep -Milliseconds 35
    }
    if ($transitions -lt 3) { throw "Saw only $transitions blinking cursor transitions on an idle pane" }
}
function Assert-Steady {
    $null = Wait-Until 'steady bar restored' { (Sample-Cell) -eq $shown }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.ElapsedMilliseconds -lt 850) {
        if ((Sample-Cell) -ne $shown) { throw 'Steady cursor blinked' }
        Start-Sleep -Milliseconds 35
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','60','--rows','20') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $pane = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    if ([FixtureMessages]::GetForegroundWindow() -ne $window -and
        ![CursorCellProbe]::SetForegroundWindow($window)) {
        # A caller running in the background can be refused activation. Restore
        # the fixture HWND on its own UI thread before resolving physical cell bounds.
        $null = [CursorCellProbe]::ShowWindowAsync($window, 7)
        $null = Wait-Until 'fixture window to minimize' { [CursorCellProbe]::IsIconic($window) }
        $null = [CursorCellProbe]::ShowWindowAsync($window, 9)
    }
    $null = Wait-Until 'the fixture to own foreground before sampling its cursor' {
        [FixtureMessages]::GetForegroundWindow() -eq $window -and ![CursorCellProbe]::IsIconic($window)
    }
    # Position the cursor after the restore, before measuring the live cell.
    $esc = [char]27
    $null = Send-Sequence "[2J$esc[4;4HBlinkProbe$esc[4;4H$esc[6 q$esc[?25l"
    $bounds = Wait-Until 'UI Automation bounds for the marked cursor cell' {
        $doc = Find-Element ('Terminal ' + [char]0xb7 + ' palette-ssh')
        if (!$doc) { return }
        $pattern = $doc.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
        $range = $pattern.DocumentRange.FindText('BlinkProbe',$false,$false)
        if ($range) {
            $rects = @($range.GetBoundingRectangles())
            if ($rects.Count -eq 1 -and $rects[0].Width -gt 0) { $rects[0] }
        }
    }
    $x = [int][Math]::Floor($bounds.X)
    $y = [int][Math]::Floor($bounds.Y)
    $cw = [int][Math]::Ceiling($bounds.Width / 'BlinkProbe'.Length)
    $ch = [int][Math]::Ceiling($bounds.Height)
    $hidden = Sample-Cell
    $null = Send-Sequence '[?25h'
    $shown = Wait-Until 'steady bar visible in the native canvas' {
        $sample = Sample-Cell
        if ($sample -ne $hidden) { $sample }
    }
    $null = Send-Sequence '[?25l'
    $null = Wait-Until 'the same native cell with its bar hidden again' { (Sample-Cell) -eq $hidden }
    $null = Send-Sequence '[?25h'
    $null = Wait-Until 'the original bar pixels restored' { (Sample-Cell) -eq $shown }

    $generation = Send-Sequence '[5 q'
    $beforePng = Join-Path $State 'blink-before.png'
    $afterPng = Join-Path $State 'blink-after.png'
    $null = Control @('screenshot','--pane',"$pane",$beforePng)
    Assert-Blinking
    $null = Control @('screenshot','--pane',"$pane",$afterPng)
    $samePng = [Collections.StructuralComparisons]::StructuralEqualityComparer.Equals(
        [IO.File]::ReadAllBytes($beforePng), [IO.File]::ReadAllBytes($afterPng))
    if ((Read-Generation) -ne $generation -or !$samePng) {
        throw 'Blinking changed the terminal generation or deterministic PNG capture'
    }

    $null = Send-Sequence '[6 q'
    Assert-Steady
    $null = Send-Sequence '[?12h'
    Assert-Blinking
    $null = Send-Sequence '[?12l'
    Assert-Steady
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
