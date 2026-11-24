[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$launch = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru `
    -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'the fixture control pipe' {
        Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json")
    }
    $initial = (Control @('list')).sessions | Where-Object active
    if (!$initial -or !$initial.window_id) { throw 'The initial terminal pane has no window ID' }
    $owner = "$($initial.window_id)"
    $foreground = [FixtureMessages]::GetForegroundWindow()
    $openArgs = @('open-ssh-group','--window',$owner,'--name')
    foreach ($failure in @(@('Missing Ops', '*Unknown saved SSH group*'), @('Broken Ops', '*SSH profile not-in-catalog is unavailable*'))) {
        $rejected = & $Cli --pid $app.Id @openArgs $failure[0] | ConvertFrom-Json
        if ($LASTEXITCODE -eq 0 -or $rejected.ok -or $rejected.error -notlike $failure[1]) {
            throw "A failed group launch was not reported: $($rejected | ConvertTo-Json -Compress)"
        }
    }
    $before = (Control @('list-ssh-groups')).windows | Where-Object window_id -eq $owner
    if ($before.groups.Count -ne 0 -or @((Control @('list')).sessions).Count -ne 1) {
        throw 'A failed launch created a tab or partial sessions'
    }

    $opened = Control ($openArgs + @('Fleet Ops'))
    if ($opened.window_id -ne $owner -or !$opened.tab_id -or $opened.name -cne 'Fleet Ops' -or
        ($opened.members.profile -join ',') -cne 'West,East' -or
        @($opened.members.tab_id | Sort-Object -Unique).Count -ne 2 -or
        @($opened.members | Where-Object { !$_.pane_id -or !$_.tab_id -or $_.tab_id -eq $opened.tab_id }).Count) {
        throw "The CLI lost the group or its ordered member IDs: $($opened | ConvertTo-Json -Depth 6 -Compress)"
    }
    $argsForGroup = @('--window',$owner,'--tab',"$($opened.tab_id)")
    $queued = Wait-Until 'the saved login limit holds one SSH pane in the queue' {
        $roster = (Control (@('get-ssh-group-roster') + $argsForGroup)).roster
        if (($roster.hosts.state | Sort-Object) -join ',' -ceq 'connecting,queued') { $roster }
    }
    if ($queued.scope -cne 'group' -or !$queued.require_all_ready -or $queued.counts.checked -ne 2) {
        throw 'The CLI launch lost the saved roster defaults or its selected members'
    }
    foreach ($name in 'West','East') {
        $gate = [IO.File]::ReadAllText((Join-Path $State "$name-gate"))
        Remove-Item -LiteralPath $gate
    }
    $ready = (Control (@('wait-ssh-group-ready') + $argsForGroup + @('--timeout','15'))).roster
    if ($ready.counts.ready_checked_shown -ne 2 -or !$ready.selection_ready -or
        ($ready.hosts.pane_id -join ',') -cne ($opened.members.pane_id -join ',')) {
        throw 'The returned group ID or pane IDs did not resolve to the verified ready roster'
    }

    $again = Control ($openArgs + @('Fleet Ops'))
    if ($again.tab_id -eq $opened.tab_id -or
        @($again.members | Where-Object { $_.pane_id -in $opened.members.pane_id }).Count) {
        throw 'Opening the same saved group reused an existing group or SSH pane'
    }
    $null = Control @('wait-ssh-group-ready','--window',$owner,'--tab',"$($again.tab_id)",'--timeout','15')
    $sessions = @((Control @('list')).sessions)
    $listed = (Control @('list-ssh-groups')).windows | Where-Object window_id -eq $owner
    if ($sessions.Count -ne 5 -or ($sessions | Where-Object active).id -ne $initial.id -or
        @($sessions | Where-Object window_id -ne $owner).Count -or
        $opened.tab_id -notin $listed.groups.tab_id -or $again.tab_id -notin $listed.groups.tab_id -or
        [FixtureMessages]::GetForegroundWindow() -ne $foreground) {
        throw 'Opening the groups changed the active tab, changed Windows focus, or created panes in another window'
    }
    foreach ($member in @($opened.members) + @($again.members)) {
        $pane = $sessions | Where-Object id -eq $member.pane_id
        if ($pane.tab_id -ne $member.tab_id -or $pane.label -cne $member.profile) {
            throw 'A returned member ID did not match the published terminal tab'
        }
    }
} catch {
    throw "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
