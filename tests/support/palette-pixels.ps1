Add-Type -AssemblyName System.Drawing
$esc = [string][char]27
$bell = [string][char]7
$newline = [string][char]13 + [char]10
$legacy = @('#dbe0e8','#0e1014','#cc5555')
$light = @('#383a42','#fafafa','#e45649')
$dark = @('#dcdfe4','#282c34','#e06c75')
$sample = $esc + '[0m' + $esc + '[?25l' + $esc + '[2J' + $esc + '[H' +
    'HHHHHHHH' + $newline + $esc + '[7m        ' + $esc + '[0m inverse' + $newline +
    $esc + '[41m        ' + $esc + '[0m indexed red' + $newline + 'palette-witness' + $newline

function Check-Pixels([int]$Pane, [string]$Stage, [string[]]$Colors) {
    $frame = Control @('get-text','--pane',"$Pane")
    $path = Join-Path $State "$Stage.png"
    $capture = Control @('screenshot','--pane',"$Pane",$path)
    $image = [Drawing.Bitmap]::new($path)
    try {
        if ($capture.pane -ne $Pane -or $image.Width -ne $capture.width -or $image.Height -ne $capture.height) {
            throw "Invalid screenshot geometry for $Stage"
        }
        $cw = $image.Width / $frame.cols
        $ch = $image.Height / $frame.rows
        # SGR 7 puts the default foreground in a solid block. Blank SGR 41
        # cells independently witness palette index 1. Both avoid glyph anti-aliasing.
        foreach ($test in @(@(1,0),@(6,1),@(2,2))) {
            $pixel = $image.GetPixel([int][Math]::Floor(2.5 * $cw),[int][Math]::Floor(($test[0] + 0.5) * $ch))
            $actual = '#{0:x2}{1:x2}{2:x2}' -f $pixel.R,$pixel.G,$pixel.B
            if ($actual -cne $Colors[$test[1]]) { throw "$Stage row $($test[0]) pixel $actual, expected $($Colors[$test[1]])" }
        }
        $ink = [Drawing.ColorTranslator]::FromHtml($Colors[0])
        $found = $false
        for ($y=0; $y -lt [Math]::Floor($ch) -and !$found; $y++) {
            for ($x=0; $x -lt [Math]::Floor(8 * $cw); $x++) {
                $p = $image.GetPixel($x,$y)
                if ([Math]::Abs([int]$p.R-$ink.R) -lt 40 -and [Math]::Abs([int]$p.G-$ink.G) -lt 40 -and
                    [Math]::Abs([int]$p.B-$ink.B) -lt 40) { $found = $true; break }
            }
        }
        if (!$found) { throw "$Stage existing H glyphs lacked foreground $($Colors[0])" }
    } finally { $image.Dispose() }
}
