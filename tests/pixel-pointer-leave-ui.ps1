[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_UI_SMOKE = '1'
$env:W11TERM_SCROLLBAR_SMOKE = $State
$profiles = Join-Path $State 'profiles.json'
$trace = Get-Content -Raw -LiteralPath (Join-Path $State 'trace-path')
$esc = [char]27

function Gesture([int]$Pane, [string]$Kind) {
    $pending = Join-Path $State 'staged-pointer'
    $request = Join-Path $State 'rail-pointer'
    [IO.File]::WriteAllText($pending, "$Pane $Kind")
    [IO.File]::Move($pending, $request)
    $null = Wait-Until "$Kind on pane $Pane" { ![IO.File]::Exists($request) }
}
function Reports {
    $text = [IO.File]::ReadAllText($trace)
    [regex]::Matches($text, '\x1b\[<([0-9]+);([0-9]+);([0-9]+)([Mm])')
}
function Send-Bytes([int]$Pane, [string]$Value) {
    $bytes = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Value))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$bytes)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $ssh = ((Control @('list')).sessions | Where-Object label -CEQ 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','pixel-ready')
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -CEQ 'Palette PowerShell').id }
    Wait-Focus $local
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    $baseline = @(Reports).Count

    Gesture $ssh enter
    Gesture $ssh move
    Gesture $local enter
    Gesture $ssh leave  # The exit can arrive after the next pane's enter.
    Wait-Focus $local
    Gesture $ssh enter
    Gesture $ssh move   # Same position as the first move.
    Wait-Focus $local

    Gesture $ssh press
    Wait-Focus $ssh
    Gesture $ssh leave
    Gesture $ssh release
    $null = Wait-Until 'both pointer exits and the captured release' { @(Reports).Count -ge ($baseline + 6) }

    Run-Palette 'Enable read-only pane input'
    $null = Wait-Until 'SSH input is read-only' {
        ((Control @('list')).sessions | Where-Object id -eq $ssh).read_only
    }
    Gesture $ssh enter
    Gesture $ssh leave
    Run-Palette 'Disable read-only pane input'
    Send-Bytes $ssh ($esc + '[?1016l' + $esc + '[?1006hlegacy-ready')
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','10','legacy-ready')
    Gesture $ssh enter
    Gesture $ssh leave
    Send-Bytes $ssh 'pointer-test-done'
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','10','pointer-test-done')

    $reports = @(Reports | Select-Object -Skip $baseline)
    $expected = @(35, 288, 35, 0, 0, 288)
    $flags = @($reports | ForEach-Object { [int]$_.Groups[1].Value -band (-bnot 28) })
    $suffix = -join @($reports | ForEach-Object { $_.Groups[4].Value })
    if (($flags -join ',') -cne ($expected -join ',') -or $suffix -cne 'MMMMmM' -or
        $reports[0].Value -cne $reports[2].Value) {
        throw "Unexpected mouse reports (flags $($flags -join ','), suffix $suffix)"
    }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
