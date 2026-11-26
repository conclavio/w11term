[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$peer = [IO.File]::ReadAllText((Join-Path $State 'peer-root'))
$hold = Join-Path $peer 'withhold-shell-reply'
$held = Join-Path $peer 'shell-reply-withheld'
$release = Join-Path $peer 'keep-first-connection'
$stop = 'Stop SSH attempt'

function Get-RemotePane { (Control @('list')).sessions | Where-Object id -eq $remote }
function Wait-Attempt {
    $null = Wait-Until 'loopback peer withheld the shell reply' { Test-Path -LiteralPath $held }
    $pane = Wait-Until 'the pending attempt is published' {
        $p = Get-RemotePane
        if ($p.ssh_attempt_token) { $p }
    }
    $events = Control @('events','--pane',"$remote")
    if ($events.ssh_attempt_token -ne $pane.ssh_attempt_token) {
        throw 'The list and event views disagree about the pending attempt'
    }
    return $pane.ssh_attempt_token
}
function Wait-Stopped([uint64]$LastGeneration = 0) {
    $stopped = Control @('wait-pane-exit','--pane',"$remote",'--timeout','15')
    $pane = Get-RemotePane
    if ($stopped.pane -ne $remote -or !$stopped.restart_token -or
        $stopped.restart_token -ne $pane.restart_token -or $stopped.ssh_connection.phase -cne 'stopped' -or
        $pane.ssh_attempt_token -or $pane.connection_generation -ne $LastGeneration -or $pane.tab_id -ne $originalTab) {
        throw "Stopping an attempt changed the tab or last established generation, or left a pending token: $($pane | ConvertTo-Json -Compress)"
    }
}
function Pick([string]$Query, [int]$Pane) {
    Picker-Query $Query
    Press-Key 13
    Wait-Focus $Pane
}
function Reject-Stop([uint64]$Token) {
    $reply = & $Cli --pid $app.Id stop-ssh-attempt --pane $remote --attempt-token $Token | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $reply.ok -or $reply.error -notmatch 'no longer pending') {
        throw "Stop accepted stale token $Token"
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $session = Wait-Until 'original SSH pane' { (Control @('list')).sessions | Where-Object label -eq 'palette-ssh' }
    $remote = $session.id
    $originalTab = $session.tab_id
    $first = Wait-Attempt
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    Pick 'palette-ssh' $remote
    Invoke-Button (Get-NativeMenuAction 'Tab menu' $stop -Submenu 'Panes')
    Wait-Stopped
    Reject-Stop $first
    $stopped = & $Cli --pid $app.Id wait-ssh-connected --pane $remote --timeout 3 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $stopped.ok -or $stopped.error -notmatch 'connection attempt stopped') {
        throw 'SSH connection wait did not report the stopped login'
    }

    [IO.File]::Delete($held)
    [IO.File]::Delete($hold)
    Run-Palette 'Restart exited pane'
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    $generation = (Get-RemotePane).connection_generation
    $marker = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes('before-retry/'))
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64',$marker)
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','before-retry/')
    [IO.File]::WriteAllText($hold,'')
    [IO.File]::Delete($release)
    $retry = Wait-Attempt
    if ($retry -eq $first -or (Get-RemotePane).ssh_connection.phase -cne 'reconnecting') {
        throw 'An automatic retry should have a fresh pending attempt'
    }
    Run-Palette $stop
    Wait-Stopped $generation
    Reject-Stop $retry

    [IO.File]::Delete($held)
    Run-Palette 'Restart exited pane'
    $third = Wait-Attempt
    Pick 'Palette PowerShell' $local
    Reject-Stop $retry
    if ((Get-RemotePane).ssh_attempt_token -ne $third) { throw 'A stale stop affected the replacement attempt' }
    $ack = Control @('stop-ssh-attempt','--pane',"$remote",'--attempt-token',"$third")
    if (!$ack.accepted -or $ack.attempt_token -ne $third -or $ack.pane -ne $remote) { throw 'Unexpected CLI stop reply' }
    Wait-Stopped $generation
    Wait-Focus $local
    $null = Control @('send-text','--pane',"$local",'--no-paste','Write-Output (''still-'' + ''running'')')
    $null = Control @('send-text','--pane',"$local",'--no-paste','--base64','DQ==')
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','still-running')

    [IO.File]::Delete($hold)
    Pick 'palette-ssh' $remote
    Run-Palette 'Restart exited pane'
    $connected = Control @('wait-ssh-connected','--pane',"$remote",'--after-generation',"$generation",'--timeout','15')
    if ($connected.pane -ne $remote -or $connected.connection_generation -le $generation) {
        throw 'SSH connection wait did not report the fresh login in the original pane'
    }
    Reject-Stop $third
    $output = Join-Path $State 'ssh-history.txt'
    $null = Control @('save-text','--pane',"$remote",$output)
    if (![IO.File]::ReadAllText($output).Contains('before-retry/')) { throw 'Stopping a retry discarded terminal history' }
    Wait-Focus $remote
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    Close-FixtureApp $app
}
