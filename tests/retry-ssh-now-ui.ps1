[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$peer = [IO.File]::ReadAllText((Join-Path $State 'peer-root'))
$drop = Join-Path $peer 'drop-shell-before-ready'
$initial = Join-Path $peer 'keep-first-connection'
$retryAction = 'Retry SSH now'

function Get-RemotePane { (Control @('list')).sessions | Where-Object id -eq $remote }
function Wait-Retry([int]$Number) {
    Wait-Until "SSH waits before retry $Number" {
        $pane = Get-RemotePane
        if ($pane.ssh_retry_token -and $pane.ssh_connection.phase -ceq 'reconnecting' -and $pane.ssh_connection.attempt -eq $Number) {
            $pane
        }
    }
}
function Wait-RetryAccepted([uint64]$Token) {
    $null = Wait-Until "retry token $Token is retired by request" {
        $records = (Control @('events','--pane',"$remote")).journal.events
        $records | Where-Object { $_.kind -ceq 'ssh-retry-wait' -and $_.data.retry_token -eq $Token -and $_.data.requested -and !$_.data.pending }
    }
}
function Reject-Retry([uint64]$Token) {
    $reply = & $Cli --pid $app.Id retry-ssh-now --pane $remote --retry-token $Token | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $reply.ok -or $reply.error -notmatch 'no longer waiting') {
        throw "Unexpected acceptance of stale retry token $Token"
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $ssh = Wait-Until 'original SSH pane' { (Control @('list')).sessions | Where-Object label -eq 'palette-ssh' }
    $remote = $ssh.id
    $tabId = $ssh.tab_id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    $generation = (Get-RemotePane).connection_generation
    $null = Control @('send-text','--pane',"$remote",'--no-paste','before-retry/')
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','before-retry/')
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    Wait-Focus $local
    [IO.File]::WriteAllText($drop,'')
    [IO.File]::Delete($initial)

    # Let the short initial waits run normally. The retry controls need time for cross-process UI automation.
    $background = Wait-Retry 3
    $inspection = Control @('inspect','--pane',"$remote")
    if ($inspection.ssh_connection.phase -cne 'reconnecting' -or
        $inspection.ssh_connection.attempt -ne 3 -or $inspection.ssh_progress -or $inspection.ssh) {
        throw 'The inspector retained SSH handshake or route details during the retry wait'
    }
    $events = Control @('events','--pane',"$remote")
    if ($events.ssh_retry_token -ne $background.ssh_retry_token) { throw 'list and events disagree about the retry token' }
    $reply = Control @('retry-ssh-now','--pane',"$remote",'--retry-token',"$($background.ssh_retry_token)")
    if (!$reply.accepted -or $reply.pane -ne $remote -or $reply.retry_token -ne $background.ssh_retry_token) {
        throw 'Unexpected CLI retry reply'
    }
    Wait-Focus $local

    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    $menu = Wait-Retry 4
    if ($menu.ssh_attempt_token -ne $background.ssh_attempt_token -or $menu.ssh_retry_token -eq $background.ssh_retry_token) {
        throw 'The retry wait must have a different token from the preceding wait in the same attempt'
    }
    Reject-Retry $background.ssh_retry_token
    Invoke-Button (Get-NativeMenuAction 'Tab menu' $retryAction -Submenu 'Panes')
    Wait-RetryAccepted $menu.ssh_retry_token

    $palette = Wait-Retry 5
    [IO.File]::Delete($drop)
    Run-Palette $retryAction
    Wait-RetryAccepted $palette.ssh_retry_token
    $ready = Wait-Until 'replacement shell in the original pane' {
        $pane = Get-RemotePane
        if ($pane.ssh_connection.phase -ceq 'connected' -and $pane.connection_generation -gt $generation) { $pane }
    }
    if ($ready.tab_id -ne $tabId -or $ready.ssh_retry_token -or $ready.ssh_attempt_token) { throw 'Unexpected state of the restored pane' }
    Reject-Retry $palette.ssh_retry_token
    Wait-RetryAccepted $background.ssh_retry_token
    $output = Join-Path $State 'ssh-history.txt'
    $null = Control @('save-text','--pane',"$remote",$output)
    if (![IO.File]::ReadAllText($output).Contains('before-retry/')) { throw 'Retrying discarded terminal history' }
    $null = Control @('send-text','--pane',"$local",'--no-paste','Write-Output (''local-'' + ''still-live'')')
    $null = Control @('send-text','--pane',"$local",'--no-paste','--base64','DQ==')
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','local-still-live')
    Wait-Focus $remote
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    Close-FixtureApp $app
}
