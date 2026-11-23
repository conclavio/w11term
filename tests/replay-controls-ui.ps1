[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$capture = Join-Path $State 'review.vt'
$duration = [long]3601200000

function Playback([string]$Op) {
    $argsForCli = @('playback','--pane',"$pane")
    if ($Op) { $argsForCli += $Op }
    return Control $argsForCli
}
function Assert-Position($Status, [string]$ExpectedState, [long]$Position) {
    if ($Status.pane -ne $pane -or $Status.state -cne $ExpectedState -or
        $Status.position_us -ne $Position -or $Status.duration_us -ne $duration) {
        throw "Expected pane $pane $ExpectedState at $Position of $duration us. Got $($Status | ConvertTo-Json -Compress)"
    }
}
function Assert-Screen([string]$Contains, [string]$Excludes) {
    $text = (Control @('get-text','--pane',"$pane")).text
    if (!$text.Contains($Contains) -or $text.Contains($Excludes)) {
        throw "Expected '$Contains' and no '$Excludes'. Screen: $text"
    }
}
function Screenshot([string]$Frame) {
    $null = Control @('screenshot','--pane',"$pane",(Join-Path $State "$Frame.png"))
}
function Local-Pid([string]$Tag) {
    $line = "'$Tag=' + " + '$PID'
    foreach ($ch in $line.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    Press-Key 13
    $pattern = "$Tag=([0-9]+)"
    $frame = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','--regex',$pattern)
    return [int]([regex]::Match($frame.text,$pattern).Groups[1].Value)
}
function Replay-Menu([string]$Label) {
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    $group = Wait-Until 'Recordings submenu' { Find-Element 'Recordings' ([System.Windows.Automation.ControlType]::MenuItem) }
    $group.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    Invoke-Button (Wait-Until "enabled $Label" {
        $item = Find-Element $Label ([System.Windows.Automation.ControlType]::MenuItem)
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--replay-timed',('"' + $capture + '"'),'--paused') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $pane = (Control @('list')).sessions[0].id
    $initial = Wait-Until 'verified capture ready at time zero' {
        $s = Playback
        if ($s.state -ceq 'paused') { $s }
    }
    Assert-Position $initial 'paused' 0
    Assert-Screen '' 'FIRST-RED'
    Run-Palette 'Next recorded update'
    $first = Wait-Until 'palette stepped to the first recorded timestamp' {
        $s = Playback
        if ($s.position_us -gt 0) { $s }
    }
    Assert-Position $first 'paused' 100000
    Assert-Screen 'FIRST-RED' 'SECOND-BLUE'
    Screenshot 'first'

    $localPane = (Control @('open','--profile','Review PowerShell','--activate')).pane
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','20','PS ')
    $pidBefore = Local-Pid 'review-before'
    Assert-Position (Playback) 'paused' 100000
    $foreground = [FixtureMessages]::GetForegroundWindow()
    Assert-Position (Playback '--step') 'paused' 150000
    $modes = Control @('inspect','--pane',"$pane")
    if (!$modes.synchronized_output -or $modes.cols -ne 72 -or $modes.rows -ne 20 -or
        $modes.pixel_width -ne 792 -or $modes.pixel_height -ne 400 -or $modes.dpi -ne 120) {
        throw "Step omitted a tied output or resize: $($modes | ConvertTo-Json -Compress -Depth 3)"
    }
    # Exceed the live synchronized-output watchdog with the replay clock frozen.
    Start-Sleep -Milliseconds 350
    Assert-Screen 'FIRST-RED' 'SECOND-BLUE'
    Assert-Position (Playback) 'paused' 150000
    Screenshot 'held'
    Assert-Position (Playback '--step') 'paused' 200000
    Assert-Screen 'SECOND-BLUE' 'AFTER-LONG-IDLE'
    Screenshot 'committed'
    if ($foreground -ne $window -and [FixtureMessages]::GetForegroundWindow() -eq $window) { throw 'Background replay control activated the fixture window' }
    Wait-Focus $localPane
    if ((Local-Pid 'review-after') -ne $pidBefore) { throw 'Background replay control replaced PowerShell' }

    Press-Key 0x77 # F8 selects the first tab in the fixture keymap.
    Wait-Focus $pane
    Replay-Menu 'Resume replay'
    $null = Wait-Until 'menu resumed playback' { (Playback).state -ceq 'playing' }
    $paused = Playback '--pause'
    if ($paused.state -cne 'paused' -or $paused.position_us -lt 200000 -or $paused.position_us -ge 3600200000) {
        throw "CLI failed to pause within the recorded idle period: $($paused | ConvertTo-Json -Compress)"
    }
    Assert-Position (Playback) 'paused' $paused.position_us
    $resumed = Playback '--resume'
    if ($resumed.state -cne 'playing') { throw "CLI did not resume: $($resumed | ConvertTo-Json -Compress)" }
    Run-Palette 'Pause replay'
    $null = Wait-Until 'palette paused playback' { (Playback).state -ceq 'paused' }
    Assert-Position (Playback '--step') 'paused' 3600200000
    Assert-Screen 'AFTER-LONG-IDLE' 'FINAL-PLAYED'
    $null = Playback '--resume'
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','FINAL-PLAYED')
    $done = Wait-Until 'recorded trailing silence finished' {
        $s = Playback
        if ($s.state -ceq 'complete') { $s }
    }
    Assert-Position $done 'complete' $duration
} catch {
    $failure = "$($_.Exception.Message) at $($_.ScriptStackTrace)"
    if ($pane -and !$app.HasExited) {
        try { $failure += ". Playback: $((Playback) | ConvertTo-Json -Compress)" } catch {}
    }
    throw $failure
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
