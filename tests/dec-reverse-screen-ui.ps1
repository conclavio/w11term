[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"split_right":"F8"}}')
$profiles = Join-Path $State 'profiles.json'
$esc = [string][char]27
$bell = [string][char]7

function Send-Sequence([int]$Pane, [string]$Sequence) {
    $bytes = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Sequence))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$bytes)
}
function Paint([int]$Pane, [string]$Fg, [string]$Bg, [string]$Label) {
    $drawing = $esc + ']10;' + $Fg + $bell + $esc + ']11;' + $Bg + $bell +
        $esc + '[?25l' + $esc + '[0m' + $esc + '[2J' + $esc + '[H' +
        'HHHHHHHH' + $Label + "`r`n" +
        $esc + '[7m        ' + $esc + '[0m inverse' + "`r`n" +
        $esc + '[48;2;255;0;255m        ' + $esc + '[0m RGB' + "`r`n" + $Label + '-ready'
    Send-Sequence $Pane $drawing
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15',"$Label-ready")
}
function Screenshot([int]$Pane, [string]$Name) {
    $path = Join-Path $State ($Name + '.png')
    $reply = Control @('screenshot','--pane',"$Pane",$path)
    if ($reply.pane -ne $Pane -or $reply.width -le 0 -or $reply.height -le 0) { throw "Invalid screenshot reply for $Name" }
}
function Mode-Change([string]$Sequence) {
    $before = Control @('get-text','--pane',"$source")
    Send-Sequence $source $Sequence
    $after = Wait-Until 'published a frame for a mode change without new text' {
        $frame = Control @('get-text','--pane',"$source")
        if ($frame.generation -gt $before.generation) { $frame }
    }
    if ($after.text -cne $before.text) { throw 'The remote mode switch changed previously written text' }
    Wait-Focus $neighbor
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','108','--rows','26') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $source = ((Control @('list')).sessions | Where-Object active).id
    $null = Control @('wait-for-text','--pane',"$source",'--timeout','15','ready')
    Press-Key 0x77
    $neighbor = Wait-Until 'second SSH pane active in the same tab' {
        $sessions = @((Control @('list')).sessions)
        $next = $sessions | Where-Object id -ne $source
        if ($sessions.Count -eq 2 -and $next.active -and $sessions[0].tab_id -eq $sessions[1].tab_id) { $next.id }
    }
    $null = Control @('wait-for-text','--pane',"$neighbor",'--timeout','15','ready')
    Paint $source '#ffff00' '#0000ff' 'source'
    Paint $neighbor '#00ff00' '#ff0000' 'neighbor'
    $a = Control @('get-text','--pane',"$source")
    $b = Control @('get-text','--pane',"$neighbor")
    $sizes = @{source=@{rows=$a.rows;cols=$a.cols};neighbor=@{rows=$b.rows;cols=$b.cols}}
    [IO.File]::WriteAllText((Join-Path $State 'sizes.json'), ($sizes | ConvertTo-Json -Depth 4))
    Screenshot $source 'normal'
    Screenshot $neighbor 'neighbor-before'
    Mode-Change ($esc + '[?5h')
    Screenshot $source 'reverse'
    Screenshot $neighbor 'neighbor-during'
    Mode-Change ($esc + '[?2026h' + $esc + '[?5l' + $esc + '[?2026l')
    Screenshot $source 'restored'
    Screenshot $neighbor 'neighbor-after'
    Wait-Focus $neighbor
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
