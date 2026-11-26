[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$restart = 'Restart exited pane'

function Type-Text([string]$Value) {
    foreach ($c in $Value.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
}
function Type-Line([string]$Value) { Type-Text $Value; Press-Key 13 }
function Wait-Text([int]$Pane, [string]$Text) {
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15',$Text)
}
function Choose-Pane([string]$Profile, [int]$Pane) {
    Picker-Query $Profile
    Press-Key 13
    Wait-Focus $Pane
}
function Shell-Pid([string]$Tag) {
    Type-Line ("'$Tag=' + " + '$PID')
    $pattern = "$Tag=([0-9]+)"
    $screen = (Control @('wait-for-text','--pane',"$local",'--regex','--timeout','15',$pattern)).text
    return [int]([regex]::Match($screen, $pattern).Groups[1].Value)
}
function Assert-LiveRestartDisabled([int]$Pane) {
    $field = Open-Palette $restart
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($restart)
    $entry = Wait-Until 'restart command in palette' { Find-Element $restart ([System.Windows.Automation.ControlType]::Button) }
    if ($entry.Current.IsEnabled) { throw "Restart available on live pane $Pane" }
    Press-Key 27
    $null = Wait-Until 'palette close' { !(Find-Element 'Search commands') }
    Wait-Focus $Pane
}
function Wait-NextGeneration($Original) {
    $null = Wait-Until "new connection generation on pane $($Original.id)" {
        $pane = (Control @('list')).sessions | Where-Object id -eq $Original.id
        $pane.connection_generation -gt $Original.connection_generation
    }
}
function Assert-Layout {
    $list = Control @('list')
    if ($list.pid -ne $app.Id -or $list.sessions.Count -ne 2) { throw 'Window process or pane count changed' }
    $app.Refresh()
    if ($app.MainWindowHandle -ne $window) { throw 'Restart replaced the original window' }
    foreach ($original in $baseline) {
        $current = @($list.sessions | Where-Object id -eq $original.id)
        if ($current.Count -ne 1) { throw "Missing original pane $($original.id)" }
        foreach ($name in 'label','window_id','tab_id','tab_title','cols','rows','visible') {
            if ($current[0].$name -ne $original.$name) { throw "Pane $($original.id) changed $name after restart" }
        }
    }
}
function Assert-Transcript([int]$Pane, [string[]]$Expected, [string]$Excluded) {
    $path = Join-Path $State "pane-$Pane.txt"
    $null = Control @('save-text','--pane',"$Pane",$path)
    $saved = [IO.File]::ReadAllText($path, [Text.UTF8Encoding]::new($false,$true))
    foreach ($marker in $Expected) {
        if (!$saved.Contains($marker)) { throw "Missing '$marker' from pane $Pane history" }
    }
    if ($saved.Contains($Excluded)) { throw "Pane $Pane contained neighboring output '$Excluded'" }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $manifest = Get-ControlManifestPath $app.Id
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath $manifest }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    Wait-Text $remote 'ready'
    $oldHandle = (Control @('sftp','--pane',"$remote",'opendir','/')).sftp.handle
    Assert-LiveRestartDisabled $remote
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $local
    Wait-Text $local 'PS '
    Run-Palette ('Rename tab' + [char]0x2026)
    $field = Wait-Until 'tab name field' { Find-Element 'Tab name' }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Operations')
    Invoke-Button (Wait-Until 'Save tab name' { Find-Element 'Save' ([System.Windows.Automation.ControlType]::Button) })
    $null = Wait-Until 'named split' { Find-Element 'Operations' ([System.Windows.Automation.ControlType]::TabItem) }
    $baseline = (Control @('list')).sessions
    $originalSsh = $baseline | Where-Object id -eq $remote
    $originalPs = $baseline | Where-Object id -eq $local
    if ($baseline.Count -ne 2 -or !$originalPs.visible -or !$originalSsh.visible -or
        $originalSsh.tab_id -ne $originalPs.tab_id -or $originalPs.tab_title -ne 'Operations') { throw 'Expected one named mixed split' }
    $oldPid = Shell-Pid 'initial'
    Type-Line '$global:restartValue = 4021; "local-history-" + $restartValue'
    Wait-Text $local 'local-history-4021'
    Type-Line '1..120 | ForEach-Object { "local row $_" }; "local-scrolled"'
    Wait-Text $local 'local row 120'
    if ((Control @('get-text','--pane',"$local")).text.Contains('local-history-4021')) { throw 'Expected retained local history above the viewport' }

    $bytes = [IO.File]::ReadAllBytes((Join-Path $State 'ssh-history.vt'))
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64',[Convert]::ToBase64String($bytes))
    Wait-Text $remote 'before SSH exit'
    if ((Control @('get-text','--pane',"$remote")).text.Contains('archived SSH')) { throw 'Expected retained SSH history above the viewport' }
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64','BA==')
    $sshExit = Control @('wait-pane-exit','--pane',"$remote",'--timeout','15')
    if ($sshExit.pane -ne $remote -or !$sshExit.restart_token -or $sshExit.exit_token -ne $sshExit.restart_token -or
        $sshExit.ssh_connection.phase -cne 'disconnected') { throw 'Incorrect result for the SSH exit' }
    $exitToken = $sshExit.restart_token
    Wait-Focus $local
    Choose-Pane 'palette-ssh' $remote
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    $sub = Wait-Until 'Panes submenu' { Find-Element 'Panes' ([System.Windows.Automation.ControlType]::MenuItem) }
    $sub.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $action = Wait-Until 'enabled restart in Panes' {
        $entry = Find-Element $restart ([System.Windows.Automation.ControlType]::MenuItem)
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    }
    Invoke-Button $action
    Wait-NextGeneration $originalSsh
    Wait-Focus $remote
    $stale = & $Cli --pid $app.Id restart --pane $remote --exit-token $exitToken | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $stale.ok) { throw 'The previous exit could restart a live SSH session twice' }
    $stat = (Control @('sftp','--pane',"$remote",'stat','/sftp-proof.bin')).sftp
    if ($stat.attrs.size -ne 3) { throw 'The restarted SSH pane did not open a fresh SFTP subsystem' }
    $oldDirectory = & $Cli --pid $app.Id sftp --pane $remote readdir --handle $oldHandle | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $oldDirectory.ok -or $oldDirectory.error -notmatch 'handle') {
        throw 'An old SFTP handle remained usable in the replacement login'
    }
    Type-Text 'restarted-ssh'
    Wait-Text $remote 'restarted-ssh'
    Assert-Transcript $remote @(('archived SSH ' + [char]0x96ea),'before SSH exit','restarted-ssh') 'local-history-4021'
    Assert-Layout

    Choose-Pane 'Palette PowerShell' $local
    if ((Shell-Pid 'unchanged') -ne $oldPid) { throw 'SSH restart replaced the local shell' }
    Type-Line '"neighbor-value-" + $restartValue'
    Wait-Text $local 'neighbor-value-4021'
    Assert-LiveRestartDisabled $local
    $waitLog = Join-Path $State 'local-exit-wait.json'
    $waiter = Start-Process -FilePath $Cli -ArgumentList @('--pid',"$($app.Id)",'wait-pane-exit','--pane',"$local",'--timeout','20') -WindowStyle Hidden -PassThru -RedirectStandardOutput $waitLog
    $null = $waiter.Handle
    Type-Line 'exit 37'
    if (!$waiter.WaitForExit(15000)) { throw 'The PowerShell exit wait did not finish' }
    $localExit = Get-Content -LiteralPath $waitLog -Raw | ConvertFrom-Json
    if ($waiter.ExitCode -ne 0 -or !$localExit.ok -or $localExit.result.pane -ne $local -or
        !$localExit.result.restart_token -or $localExit.result.exit_token -ne $localExit.result.restart_token -or
        $localExit.result.ssh_connection -or
        (Control @('get-text','--pane',"$local")).status -cne 'Local process exited with code 37') {
        throw "Incorrect result for the PowerShell exit: $($localExit | ConvertTo-Json -Compress)"
    }
    Run-Palette $restart
    Wait-NextGeneration $originalPs
    Wait-Focus $local
    if ((Shell-Pid 'replacement') -eq $oldPid) { throw 'PowerShell restart did not launch a fresh process' }
    $stalePayload = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes(('Write-Output ("stale-" + "scope")' + "`r")))
    $staleWrite = & $Cli --pid $app.Id send-text --pane $local --no-paste --connection-generation $originalPs.connection_generation --base64 $stalePayload | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $staleWrite.ok -or $staleWrite.error -notmatch 'connection changed') {
        throw 'The previous PowerShell generation could still send input'
    }
    $current = ((Control @('list')).sessions | Where-Object id -eq $local).connection_generation
    $wire = [Text.Encoding]::UTF8.GetBytes(('"new-shell-" + ($null -eq $restartValue)' + "`r"))
    $written = Control @('send-text','--pane',"$local",'--no-paste','--connection-generation',"$current",'--base64',[Convert]::ToBase64String($wire))
    if ($written.pane -ne $local -or $written.connection_generation -ne $current -or $written.bytes_written -ne $wire.Length) {
        throw 'Scoped PowerShell write did not acknowledge the replacement process'
    }
    Wait-Text $local 'new-shell-True'
    if ((Control @('get-text','--pane',"$local")).text.Contains('stale-scope')) {
        throw 'A stale write reached the replacement PowerShell process'
    }
    Assert-Transcript $local @('local-history-4021',"neighbor-value-4021",'new-shell-True') 'restarted-ssh'
    Assert-Layout
    Assert-LiveRestartDisabled $local
    if (((Control @('list')).sessions | Where-Object id -eq $remote).connection_generation -ne
        ($originalSsh.connection_generation + 1)) { throw 'Restarting PowerShell restarted SSH again' }
    Choose-Pane 'palette-ssh' $remote
    Type-Text '-neighbor-alive'
    Wait-Text $remote '-neighbor-alive'
} finally {
    if ($waiter) {
        if (!$waiter.HasExited) { $waiter.Kill(); $null = $waiter.WaitForExit(5000) }
        $waiter.Dispose()
    }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
