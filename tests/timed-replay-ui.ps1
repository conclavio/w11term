[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$gate = [IO.File]::ReadAllText((Join-Path $State 'output-gate'))
$timed = Join-Path $State 'session.vt'
$plain = Join-Path $State 'plain.vt'
$esc = [char]27
$app = $null

function Start-Fixture([string]$Name, [string[]]$Arguments) {
    $script:phase = $Name
    $script:app = Start-Process -FilePath $Gui -ArgumentList (@('--profiles',('"' + $profiles + '"')) + $Arguments) -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State "$Name.log")
    $null = Wait-Until "control pipe for $Name" { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
}
function Stop-Fixture {
    if (!$app) { return }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
    $script:app = $null
}
function Send-Bytes([string]$Bytes) {
    $data = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($Bytes))
    $null = Control @('send-text','--pane','1','--no-paste','--base64',$data)
}
function Wait-Text([string]$Text) {
    $null = Control @('wait-for-text','--pane','1','--timeout','20',$Text)
}
function Geometry {
    $m = Control @('inspect','--pane','1')
    if ($m.pixel_width -le $m.cols -or $m.pixel_height -le $m.rows -or $m.dpi -le 0) {
        throw "Missing measured text area: $($m | ConvertTo-Json -Compress -Depth 3)"
    }
    return "$($m.cols)x$($m.rows) cells $($m.pixel_width)x$($m.pixel_height) px at $($m.dpi) DPI"
}
function Frame-Interval {
    $journal = (Control @('events','--pane','1','--since','0','--timeout','0')).journal
    $titles = @($journal.events | Where-Object { $_.kind -ceq 'title' -and $_.data.title -in 'timed-first','timed-last' })
    if ($journal.lost -or $titles.Count -ne 2 -or $titles[0].data.title -cne 'timed-first' -or $titles[1].data.title -cne 'timed-last') {
        throw "Unexpected timed frame events: $($titles | ConvertTo-Json -Compress -Depth 3)"
    }
    return [int64]$titles[1].timestamp_ms - [int64]$titles[0].timestamp_ms
}

try {
    Start-Fixture 'gui' @('--profile','palette-ssh','--cols','64','--rows','18')
    Wait-Text 'ready'
    $null = Control @('resize','--pane','1','--cols','64','--rows','18')
    $initial = Geometry
    $null = Control @('record','--pane','1','--timed',$timed)
    [IO.File]::Delete($gate) # Release the SSH server's unsolicited first frame.
    Wait-Text 'FIRST-SSH-FRAME'

    # Create a deliberate interval in the recording. Both title events persist in
    # the pane journal, even if the CLI connects after playback has already ended.
    Start-Sleep -Milliseconds 3500
    $null = Control @('resize','--pane','1','--cols','72','--rows','20')
    $red = '/wAA' * 64
    Send-Bytes ("${esc}]2;timed-last" + [char]7 + "${esc}[2;4H${esc}_Ga=T,i=49,f=24,s=8,v=8,c=1,r=1,C=1,q=2;$red${esc}\${esc}[3;1HLAST-SSH-FRAME")
    Wait-Text 'LAST-SSH-FRAME'
    $record = Control @('record','--pane','1','--stop')
    $final = Geometry
    if ($initial -notlike '64x18 cells *' -or $final -notlike '72x20 cells *') { throw "Live resize failed: $initial -> $final" }
    $live = Control @('get-text','--pane','1')
    $null = Control @('screenshot','--pane','1',(Join-Path $State 'live.png'))
    $recordedGap = Frame-Interval
    if ($recordedGap -lt 3000 -or $record.bytes -ne (Get-Item -LiteralPath $timed).Length) {
        throw "Unexpected live recording: gap=$recordedGap ms, bytes=$($record.bytes)"
    }

    $null = Control @('record','--pane','1',$plain)
    Send-Bytes "${esc}[2J${esc}[HLEGACY-RECORDING"
    Wait-Text 'LEGACY-RECORDING'
    $null = Control @('record','--pane','1','--stop')
    Stop-Fixture

    Start-Fixture 'timed' @('--replay-timed',('"' + $timed + '"'))
    Wait-Text 'FIRST-SSH-FRAME'
    Wait-Text 'LAST-SSH-FRAME'
    $null = Wait-Until 'timed playback to finish' {
        (Control @('list')).sessions[0].status -like 'Timed replay complete*'
    }
    $replayGap = Frame-Interval
    if ($replayGap -lt 1500) { throw "Timed replay collapsed two frames: recorded=$recordedGap ms, replayed=$replayGap ms" }
    $replayed = Control @('get-text','--pane','1')
    $replayGrid = Geometry
    if ($replayGrid -cne $final -or $live.text -cne $replayed.text -or $replayed.title -cne 'timed-last') {
        throw "Timed replay differed from live SSH: expected $final, got $replayGrid ($($replayed.title))"
    }
    $null = Control @('screenshot','--pane','1',(Join-Path $State 'replayed.png'))
    Stop-Fixture

    Start-Fixture 'legacy' @('--replay',('"' + $plain + '"'))
    Wait-Text 'LEGACY-RECORDING'
    $legacy = Control @('get-text','--pane','1')
    if ($legacy.cols -ne 72 -or $legacy.rows -ne 20 -or $legacy.text.Trim() -cne 'LEGACY-RECORDING') {
        throw "Legacy recording did not replay at its original size: $($legacy.cols)x$($legacy.rows)"
    }
} catch {
    $failure = $_.Exception.Message
    $log = Get-Content -LiteralPath (Join-Path $State "$phase.log") -ErrorAction SilentlyContinue -Tail 8
    throw "$phase failed: $failure. $($log -join ' ')"
} finally {
    Stop-Fixture
}
