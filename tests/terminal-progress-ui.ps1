[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$dot = [char]0xb7
$tabType = [System.Windows.Automation.ControlType]::TabItem

function Wait-Header([string]$Label) {
    $null = Wait-Until "native tab header '$Label'" { Find-Element $Label $tabType }
}
function Set-SshReport([int]$Pane, [string]$Payload, [string]$Expected) {
    # OSC only. The background terminal has no printable output to trigger a repaint.
    $osc = [Text.Encoding]::ASCII.GetBytes("$([char]27)]$Payload$([char]7)")
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',[Convert]::ToBase64String($osc))
    Wait-Header $Expected
    Wait-Focus $local
}
function Set-SshProgress([int]$Pane, [string]$Value, [string]$Expected) {
    Set-SshReport $Pane "9;4;$Value" $Expected
}
function Type-Line([string]$Command) {
    foreach ($c in $Command.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
}
function Read-ShellPid([string]$Tag) {
    Type-Line ("'$Tag=' + " + '$PID')
    $pattern = "$Tag=([0-9]+)"
    $text = (Control @('wait-for-text','--pane',"$local",'--regex','--timeout','15',$pattern)).text
    return [int]([regex]::Match($text, $pattern).Groups[1].Value)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $manifest = Get-ControlManifestPath $app.Id
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath $manifest }
    $left = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$left",'--timeout','15','ready')
    Run-Palette 'Split right with palette-ssh'
    $right = Wait-Until 'second SSH pane' { ((Control @('list')).sessions | Where-Object { $_.id -ne $left -and $_.label -eq 'palette-ssh' }).id }
    $null = Control @('wait-for-text','--pane',"$right",'--timeout','15','ready')
    $rightBefore = (Control @('events','--pane',"$right",'--since','0')).journal.next
    Run-Palette "New tab with Palette PowerShell $dot Local"
    $local = Wait-Until 'separate PowerShell tab' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $local
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    $shellPid = Read-ShellPid 'before'
    Wait-Header 'palette-ssh'

    Set-SshProgress $left '1;42' "42% $dot palette-ssh"
    Set-SshProgress $right '4;68' "Paused 68% $dot palette-ssh"
    Set-SshProgress $left '2' "Error 42% $dot palette-ssh"
    Set-SshProgress $left '0' "Paused 68% $dot palette-ssh"
    Set-SshProgress $right '3' "In progress $dot palette-ssh"
    Set-SshProgress $right '0' 'palette-ssh'

    Set-SshProgress $right '1;31' "31% $dot palette-ssh"
    $null = Control @('send-text','--pane',"$right",'--no-paste','--base64','BA==')
    $null = Wait-Until 'replacement SSH transport for the same pane' {
        $journal = (Control @('events','--pane',"$right",'--since',"$rightBefore",'--timeout','0')).journal
        $journal.events | Where-Object { $_.kind -eq 'connected' -and $_.data.generation -gt 1 }
    }
    Wait-Header 'palette-ssh'
    Set-SshProgress $right '1;77' "77% $dot palette-ssh"
    Set-SshProgress $right '0' 'palette-ssh'
    Set-SshReport $left '7501;state=working:progress=24' "24% $dot palette-ssh"
    Set-SshReport $right '7501;state=blocked:kind=permission:msg=Q29udGludWU/' "Needs approval $dot palette-ssh"
    Set-SshReport $left '7501;state=done' "Needs approval $dot palette-ssh"
    Set-SshReport $right '133;A' "Done $dot palette-ssh"
    Set-SshProgress $left '1;99' "Done $dot palette-ssh"
    Set-SshReport $left '7501;state=clear' 'palette-ssh'
    Wait-Focus $local
    Wait-Header 'Palette PowerShell'
    if ((Read-ShellPid 'after') -ne $shellPid) { throw 'Background SSH progress interrupted PowerShell' }

    # A local process can report progress through ConPTY too. Its exit clears
    # the header even though the command never sends an OSC reset.
    Type-Line "[Console]::Write([char]27 + ']9;4;1;63' + [char]7)"
    Wait-Header "63% $dot Palette PowerShell"
    Type-Line 'exit 23'
    $null = Wait-Until 'local PowerShell exit status' {
        (Control @('get-text','--pane',"$local")).status -eq 'Local process exited with code 23'
    }
    Wait-Header 'Palette PowerShell'

    Press-Key 0x77 # F8 selects tab 1 and restores its last active split.
    Wait-Focus $right
    foreach ($c in 'ssh-alive'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$right",'--timeout','10','ssh-alive')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
