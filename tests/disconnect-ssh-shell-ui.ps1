[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$action = 'Disconnect SSH shell' + [char]0x2026
$buttonType = [System.Windows.Automation.ControlType]::Button

function Get-RemotePane { (Control @('list')).sessions | Where-Object id -eq $remote }
function Pick([string]$Query, [int]$Pane) {
    Picker-Query $Query
    Press-Key 13
    Wait-Focus $Pane
}
function Wait-Ready([uint64]$Previous = 0) {
    $pane = Wait-Until 'a ready SSH shell in the original pane' {
        $p = Get-RemotePane
        if ($p.ssh_connection.phase -ceq 'connected' -and $p.connection_generation -gt $Previous) { $p }
    }
    if ($pane.tab_id -ne $tabId) { throw 'Restart changed the original tab' }
    $pane.connection_generation
}
function Wait-Disconnected([uint64]$Generation) {
    $pane = Wait-Until 'the original SSH pane became restartable' {
        $p = Get-RemotePane
        if ($p.ssh_connection.phase -ceq 'disconnected' -and $p.restart_token) { $p }
    }
    if ($pane.tab_id -ne $tabId -or $pane.connection_generation -ne $Generation -or $pane.ssh_attempt_token) {
        throw 'Disconnect changed the original pane or started another attempt'
    }
    $records = (Control @('events','--pane',"$remote")).journal.events
    $matching = @($records | Where-Object { $_.kind -ceq 'ssh-disconnect-requested' -and $_.data.connection_generation -eq $Generation })
    if ($matching.Count -ne 1) { throw "Expected one accepted disconnect on generation $Generation" }
}
function Review-Button([string]$Name) {
    Wait-Until "enabled '$Name' in the disconnect dialog" {
        $element = Find-Element $Name $buttonType -Visible
        if ($element -and $element.Current.IsEnabled) { $element }
    }
}
function Close-Review([string]$Button) {
    Invoke-Button (Review-Button $Button)
    $null = Wait-Until 'disconnect dialog to close' { -not (Find-Element 'Disconnect shell' $buttonType -Visible) }
    # WinUI hides the buttons before the Closed event returns keyboard routing.
    $null = Wait-Until 'keyboard routing to return to the SSH pane' {
        $label = 'Terminal ' + [char]0xb7 + ' palette-ssh'
        $doc = Find-Element $label ([System.Windows.Automation.ControlType]::Document)
        $doc -and $doc.Current.AutomationId -ceq "w11term-pane-$remote"
    }
}
function Send-Remote([string]$Marker) {
    $null = Control @('send-text','--pane',"$remote",'--no-paste',$Marker)
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15',$Marker)
}
function Request-Disconnect([uint64]$Generation) {
    $reply = Control @('disconnect-ssh-shell','--pane',"$remote",'--connection-generation',"$Generation")
    if (!$reply.accepted -or $reply.pane -ne $remote -or $reply.connection_generation -ne $Generation) {
        throw 'Unexpected disconnect reply'
    }
}
function Reject-Disconnect([int]$Pane, [uint64]$Generation, [string]$ErrorText) {
    $reply = & $Cli --pid $app.Id disconnect-ssh-shell --pane $Pane --connection-generation $Generation | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $reply.ok -or $reply.error -notmatch $ErrorText) {
        throw "Unexpected disconnect result for pane $Pane on generation ${Generation}: $($reply | ConvertTo-Json -Compress)"
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $ssh = Wait-Until 'original SSH pane' { (Control @('list')).sessions | Where-Object label -eq 'palette-ssh' }
    $remote = $ssh.id
    $tabId = $ssh.tab_id
    $original = Wait-Ready
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    Pick 'palette-ssh' $remote
    Send-Remote 'saved-history/'

    Invoke-Button (Get-NativeMenuAction 'Tab menu' $action -Submenu 'Panes')
    $null = Review-Button 'Disconnect shell'
    Close-Review 'Cancel'
    Send-Remote 'after-cancel/'
    if ((Get-RemotePane).connection_generation -ne $original) { throw 'Cancelling the review replaced the shell' }

    Run-Palette $action
    Close-Review 'Disconnect shell'
    Wait-Disconnected $original
    Run-Palette 'Restart exited pane'
    $second = Wait-Ready $original

    Run-Palette $action
    $null = Review-Button 'Disconnect shell'
    Request-Disconnect $second
    Wait-Disconnected $second
    $null = Wait-Until 'the stale review to disable Disconnect' {
        $element = Find-Element 'Disconnect shell' $buttonType -Visible
        $element -and !$element.Current.IsEnabled
    }
    Close-Review 'Cancel'
    Run-Palette 'Restart exited pane'
    $third = Wait-Ready $second

    Pick 'Palette PowerShell' $local
    Reject-Disconnect $remote $original 'changed before disconnect'
    Reject-Disconnect $local 1 'requires a direct SSH shell'
    if ((Get-RemotePane).ssh_connection.phase -cne 'connected') { throw 'A stale request disconnected the replacement shell' }
    Request-Disconnect $third
    Wait-Disconnected $third
    Wait-Focus $local
    $null = Control @('send-text','--pane',"$local",'--no-paste','Write-Output (''local-'' + ''still-live'')')
    $null = Control @('send-text','--pane',"$local",'--no-paste','--base64','DQ==')
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','local-still-live')
    $output = Join-Path $State 'ssh-history.txt'
    $null = Control @('save-text','--pane',"$remote",$output)
    if (![IO.File]::ReadAllText($output).Contains('saved-history/')) { throw 'Disconnecting and restarting discarded SSH history' }
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    Close-FixtureApp $app
}
