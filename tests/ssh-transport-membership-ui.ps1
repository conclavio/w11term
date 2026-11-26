[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","split_right":"F6"}}')
$profiles = Join-Path $State 'profiles.json'

function Get-Pane([uint64]$Pane) { (Control @('list')).sessions | Where-Object id -eq $Pane }
function Inspect-Transport([uint64]$Pane) { (Control @('inspect','--pane',"$Pane")).ssh_transport }
function Wait-Transport([uint64]$Pane, [string]$Members) {
    Wait-Until "pane $Pane to report transport members $Members" {
        $transport = Inspect-Transport $Pane
        if ($transport.id -gt 0 -and ($transport.pane_ids -join ',') -ceq $Members -and
            (Get-Pane $Pane).ssh_transport_id -eq $transport.id) { $transport }
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $original = Wait-Until 'original SSH pane' { ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id }
    $first = Wait-Transport $original "$original"
    Invoke-NativeMenuAction 'Tab menu' 'Terminal inspector: Off'
    $null = Wait-Until 'the inspector shows the original transport and its sole member' {
        (Find-Element "SSH transport   #$($first.id)") -and (Find-Element 'Other panes   None')
    }

    Press-Key 0x75 # F6 splits the active pane with its current SSH connection.
    $child = Wait-Until 'new SSH split' {
        $entries = @((Control @('list')).sessions)
        if ($entries.Count -eq 2) { ($entries | Where-Object id -ne $original).id }
    }
    Wait-Focus $child
    $expected = (@($original, $child) | Sort-Object) -join ','
    $shared = Wait-Transport $child $expected
    if ($shared.id -ne $first.id -or (Wait-Transport $original $expected).id -ne $first.id) {
        throw 'Split current shell changed the transport instead of sharing it'
    }
    $null = Wait-Until 'the inspector follows the split and identifies its sibling pane' {
        (Find-Element "Terminal inspector $([char]0xb7) Pane $child") -and
            (Find-Element "SSH transport   #$($shared.id)") -and (Find-Element "Other panes   $original")
    }

    $other = (Control @('open','--profile','palette-ssh')).pane
    $independent = Wait-Transport $other "$other"
    if ($independent.id -eq $shared.id) { throw 'Opening the same profile should use its own transport' }
    Wait-Focus $child
    $generation = (Get-Pane $original).connection_generation
    $null = Control @('disconnect-ssh-shell','--pane',"$original",'--connection-generation',"$generation")
    $exit = Wait-Until 'the original pane disconnected and lost its transport' {
        $pane = Get-Pane $original
        if ($pane.restart_token -and !$pane.ssh_transport_id -and !(Inspect-Transport $original)) { $pane.restart_token }
    }
    if ((Wait-Transport $child "$child").id -ne $shared.id) { throw 'Disconnect changed the child transport' }
    $null = Wait-Until 'the still-focused inspector drops the retired member' {
        (Find-Element "SSH transport   #$($shared.id)") -and (Find-Element 'Other panes   None')
    }

    $null = Control @('restart','--pane',"$original",'--exit-token',"$exit")
    $replacement = Wait-Transport $original "$original"
    if ($replacement.id -in @($shared.id,$independent.id)) { throw 'A manual restart should allocate a fresh transport' }
    if ((Wait-Transport $child "$child").id -ne $shared.id) { throw 'Restart changed the child transport' }
    Wait-Focus $child
    $null = Control @('send-text','--pane',"$child",'--no-paste','child-still-live/')
    $null = Control @('wait-for-text','--pane',"$child",'--timeout','15','child-still-live/')
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    Close-FixtureApp $app
}
