[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/cursor-cell.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"split_right":"F8"}}')
$profiles = Join-Path $State 'profiles.json'
$peer = [IO.File]::ReadAllText((Join-Path $State 'peer-root'))
$traceDir = Join-Path $peer 'split-inputs'
$esc = [string][char]27
$st = $esc + '\'
$nl = [Environment]::NewLine
$button = [System.Windows.Automation.ControlType]::Button
$text = [System.Windows.Automation.ControlType]::Text
$dot = " $([char]0xb7) "

function Send-Output([int]$Pane, [string]$Output) {
    $data = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("notification-fixture:$Output"))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$data)
}
function Notice([string]$Meta, [string]$Body) { "$esc]99;$Meta;$Body$st" }
function Read-Trace([string]$Path) {
    $reader = Open-PeerTraceReader $Path
    try { [Text.Encoding]::UTF8.GetString((Read-PeerTrace $reader)) }
    finally { $reader.Dispose() }
}
function Has-Reply([string]$Path,[string]$Expected) {
    (Read-Trace $Path).Contains($Expected)
}
function Find-Trace([string]$Marker) {
    Wait-Until "SSH channel for $Marker" {
        Get-ChildItem -LiteralPath $traceDir -File | Where-Object {
            (Read-Trace $_.FullName).Contains("notification-fixture:$Marker")
        } | Select-Object -First 1 -ExpandProperty FullName
    }
}
function Open-Inbox([int]$Count) {
    $inbox = Wait-Until "inbox with $Count unread" { Find-Element "Attention inbox, $Count unread" $button }
    Invoke-Button $inbox
    $null = Wait-Until 'native Attention inbox' { Find-Element 'Attention inbox' $text -Visible }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $left = ((Control @('list')).sessions | Where-Object active).id
    $null = Control @('wait-for-text','--pane',"$left",'--timeout','15','ready')
    Show-CursorWindow
    Press-Key 0x77
    $right = Wait-Until 'focused right SSH split' { ((Control @('list')).sessions | Where-Object { $_.id -ne $left -and $_.active }).id }
    $null = Control @('wait-for-text','--pane',"$right",'--timeout','15','ready')
    Send-Output $left "left-channel$nl"
    Send-Output $right "right-channel$nl"
    $leftTrace = Find-Trace 'left-channel'
    $rightTrace = Find-Trace 'right-channel'
    if ($leftTrace -eq $rightTrace) { throw 'The two panes share a trace file' }

    Send-Output $left (Notice 'i=probe:p=?' '')
    $capability = Notice 'i=probe:p=?' 'a=focus,report:c=1:o=always,unfocused,invisible:p=title,body,buttons,close,alive,?:w=1'
    $null = Wait-Until 'OSC 99 capability reply on the left SSH channel' { Has-Reply $leftTrace $capability }

    Send-Output $left ((Notice 'i=overflow:d=0' 'Never commit') + (Notice 'i=overflow:p=body' ('x' * 4097)) +
        (Notice 'i=overflow:p=close' '') + "overflow-marker$nl")
    $null = Control @('wait-for-text','--pane',"$left",'--timeout','15','overflow-marker')
    Send-Output $left ((Notice 'i=job:d=0:a=-focus,report:c=1' 'Left; build') + "partial-marker$nl")
    $null = Control @('wait-for-text','--pane',"$left",'--timeout','15','partial-marker')
    $null = Wait-Until 'draft has no unread alert' { Find-Element 'Attention inbox, 0 unread' $button }
    Send-Output $right (Notice 'i=job:a=report:c=1' 'Right build')
    $sep = [string][char]0x2028
    $snow = [string][char]0x96ea
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("Done $snow"))
    Send-Output $left ((Notice 'i=job:p=body:d=0:e=1' $encoded.Substring(0,5)) +
        (Notice 'i=job:p=body:d=0:e=1' $encoded.Substring(5).TrimEnd('=')) +
        (Notice 'i=job:p=buttons' "Open$($sep)Archive job"))
    Open-Inbox 2
    $leftLabel = @('Unread','palette-ssh',"Pane $left",'Left; build',"Done $snow") -join $dot
    $null = Wait-Until 'assembled left notification' { Find-Element $leftLabel $button -Visible }
    $archive = Wait-Until 'second notification button' { Find-Element "Archive job for Pane $left" $button }
    Invoke-Button $archive
    $leftWire = (Notice 'i=job' '2') + (Notice 'i=job:p=close' '')
    $null = Wait-Until 'activation and close on the left SSH channel' { Has-Reply $leftTrace $leftWire }
    $null = Wait-Until 'inbox stays open with -focus' { Find-Element 'Attention inbox' $text -Visible }
    if (Has-Reply $rightTrace $leftWire) { throw 'Left action reached right shell' }

    $rightLabel = @('Unread','palette-ssh',"Pane $right",'Right build') -join $dot
    $rightRow = Wait-Until 'right notification with the same identifier' { Find-Element $rightLabel $button -Visible }
    Invoke-Button $rightRow
    Wait-Focus $right
    $rightWire = (Notice 'i=job' '') + (Notice 'i=job:p=close' '')
    $null = Wait-Until 'activation and close on the right SSH channel' { Has-Reply $rightTrace $rightWire }

    Send-Output $right (Notice 'i=fast:c=1:w=100' 'Expiring notice')
    $null = Wait-Until 'expiry close on the right SSH channel' { Has-Reply $rightTrace (Notice 'i=fast:p=close' '') }
    Send-Output $left (Notice 'i=left-alive:p=alive' '')
    $null = Wait-Until 'left pane has no retained active ids' { Has-Reply $leftTrace (Notice 'i=left-alive:p=alive' '') }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
