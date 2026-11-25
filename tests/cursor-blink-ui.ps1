[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/cursor-cell.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_TEST_CARET_BLINK_MS = '200'
$profiles = Join-Path $State 'profiles.json'

function Sample-Cell {
    [CursorCellProbe]::Sample($window, $bounds.X, $bounds.Y, $bounds.Width, $bounds.Height)
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
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    Show-CursorWindow
    # Position the cursor after the restore, before measuring the live cell.
    $esc = [char]27
    $null = Send-Sequence "[2J$esc[4;4HBlinkProbe$esc[4;4H$esc[6 q$esc[?25l"
    $bounds = Get-CursorCell 'palette-ssh' 'BlinkProbe'
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
