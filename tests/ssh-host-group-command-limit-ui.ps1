[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_EXPORT_SMOKE_DIR = $State
$profiles = Join-Path $State 'profiles.json'
$config = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles
$dot = ' ' + [char]0xb7 + ' '
$group = "SSH group${dot}Fleet"
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$item = [System.Windows.Automation.SelectionItemPattern]::Pattern

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $entry = Find-Element $Name $button
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
}
function Read-GroupRecording([string]$Path) {
    try {
        $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    } catch [IO.FileNotFoundException] {
        return
    }
    $reader = [IO.StreamReader]::new($stream)
    try { $reader.ReadToEnd() } finally { $reader.Dispose() }
}
function Set-Command([string]$Command) {
    $field = Wait-Until 'group command' { Find-Element 'Group command' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Command)
}
function Run-Snapshot([uint64]$Run) {
    Control @('get-ssh-group-run','--window',"$groupWindow",'--tab',"$groupTab",'--run',"$Run")
}
function Queue-Action([uint64]$Run, [string]$Action) {
    Control @('ssh-group-queue','--window',"$groupWindow",'--tab',"$groupTab",'--run',"$Run",$Action)
}
function Select-GroupOption([string]$Name, [string]$Label) {
    $combo = [System.Windows.Automation.ControlType]::ComboBox
    $listItem = [System.Windows.Automation.ControlType]::ListItem
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $selection = [System.Windows.Automation.SelectionPattern]::Pattern
    $null = Wait-Until "'$Name' selected '$Label'" {
        try {
            $field = Find-Element $Name $combo -Visible
            $selected = $null
            if (!$field -or !$field.Current.IsEnabled -or
                !$field.TryGetCurrentPattern($selection, [ref]$selected)) { return }
            $current = $selected.Current.GetSelection()
            if ($current.Count -eq 1 -and $current[0].Current.Name -ceq $Label) { return $true }

            # A deferred focus or roster update can dismiss the first popup.
            $popup = $null
            if (!$field.TryGetCurrentPattern($expand, [ref]$popup)) { return }
            if ($popup.Current.ExpandCollapseState -eq 'Collapsed') { $popup.Expand() }
            $option = Find-Element $Label $listItem -Visible
            $choice = $null
            if ($option -and $option.Current.IsEnabled -and
                $option.TryGetCurrentPattern($item, [ref]$choice)) { $choice.Select() }
        } catch {
            if ($_.Exception.GetBaseException() -isnot [System.Windows.Automation.ElementNotAvailableException]) { throw }
        }
    }
    $field = Find-Element $Name $combo -Visible
    $popup = $null
    if ($field -and $field.TryGetCurrentPattern($expand, [ref]$popup) -and
        $popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}

$launch = @('--profiles', ('"' + $profiles + '"'), '--demo', '--cols', '100', '--rows', '36')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
$waitJob = $null
$approvalJob = $null
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    Run-Palette "Open SSH group${dot}Fleet" -Enter
    $listing = Control @('list-ssh-groups')
    $owner = $listing.windows | Where-Object { $_.groups.label -ccontains $group }
    $listedGroup = $owner.groups | Where-Object label -ceq $group
    $groupWindow = $owner.window_id
    $groupTab = $listedGroup.tab_id
    if (!$groupWindow -or !$groupTab -or $listedGroup.runs.Count -ne 0) {
        throw 'The new group was missing from the CLI listing or already had runs'
    }
    $panes = @{}
    $labels = @{}
    foreach ($name in 'West','East','North') {
        $panes[$name] = Wait-Until "$name pane" { ((Control @('list')).sessions | Where-Object label -ceq $name).id }
        $null = Control @('wait-for-text','--pane',"$($panes[$name])",'--timeout','10','fixture$')
        $labels[$name] = "$name${dot}test@127.0.0.1:$($config.$name.port)${dot}Pane $($panes[$name])"
    }
    Select-GroupOption 'Parallel SSH commands' '1 at a time'
    Set-Command 'roll'
    Click 'Preview send order'
    $null = Wait-Until 'tab order starts the preview with West' {
        (Find-Element 'Send to one host at a time.') -and (Find-Element "1. $($labels.West)")
    }
    Select-GroupOption 'Sort SSH roster' 'Host label'
    $null = Wait-Until 'the preview follows the new roster sort' {
        (Find-Element "1. $($labels.East)") -and (Find-Element "2. $($labels.North)") -and
            (Find-Element "3. $($labels.West)") -and !(Find-Element "1. $($labels.West)")
    }
    Click "Move $($labels.East) later"
    $null = Wait-Until 'moving East later puts North first' { Find-Element "1. $($labels.North)" }
    Click 'Use roster order'
    $null = Wait-Until 'discarding the manual order restores the current sort' {
        (Find-Element "1. $($labels.East)") -and !(Find-Element 'Use roster order' $button)
    }

    Click "Move $($labels.West) earlier"
    $null = Wait-Until 'West moves one position earlier' { Find-Element "2. $($labels.West)" }
    Click "Move $($labels.West) earlier"
    $null = Wait-Until 'West becomes the first planned recipient' { Find-Element "1. $($labels.West)" }
    Click "Move $($labels.North) earlier"
    $null = Wait-Until 'the exact manual order is visible before sending' {
        (Find-Element "1. $($labels.West)") -and (Find-Element "2. $($labels.North)") -and (Find-Element "3. $($labels.East)")
    }
    Click 'Send to 3 hosts'
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started roll')
    $null = Wait-Until 'North and East are queued in the results' {
        (Find-Element "$($labels.North)${dot}Queued") -and (Find-Element "$($labels.East)${dot}Queued")
    }
    Select-GroupOption 'Sort SSH roster' 'Tab order'
    $null = Control @('send-text','--pane',"$($panes.North)",'--no-paste','x')
    Click 'Attention inbox, 0 unread'
    $null = Wait-Until 'the group is in the background' { Find-Element 'No bells or notifications' }
    $activeWindow = [FixtureMessages]::GetForegroundWindow()
    $null = Wait-Until 'CLI shows one running host and two queued in the background run' {
        $snapshot = Run-Snapshot 1
        $run = $snapshot.run
        $snapshot.window_id -eq $groupWindow -and $snapshot.tab_id -eq $groupTab -and
            $run.schema_version -eq 1 -and $run.command -ceq 'roll' -and $run.summary.outcomes.running -eq 1 -and
            $run.summary.outcomes.waiting -eq 2 -and $run.dispatch_policy.parallel_limit -eq 1 -and
            $run.recipients[0].pane_id -eq $panes.West -and $run.recipients[0].execution.state -ceq 'running' -and
            $run.recipients[1].pane_id -eq $panes.North -and $run.recipients[1].delivery.state -ceq 'queued'
    }
    $groupRun = (Control @('list-ssh-groups')).windows.groups.runs | Where-Object run_id -eq 1
    if ($groupRun.label -cne 'roll' -or $groupRun.recipient_count -ne 3 -or $groupRun.settled_count -ne 0 -or
        !(Find-Element 'No bells or notifications') -or [FixtureMessages]::GetForegroundWindow() -ne $activeWindow) {
        throw 'CLI discovery changed the visible page or did not summarize the background run'
    }
    $waitArgs = @('wait-ssh-group-run','--window',"$groupWindow",'--tab',"$groupTab",'--run','1')
    $timedOut = & $Cli --pid $app.Id @waitArgs --timeout 1 --require-success | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $timedOut.ok -or $timedOut.error -notlike '*timed out before settlement*') {
        throw 'A strict wait did not distinguish timeout from a settled failure'
    }
    $waitOutput = Join-Path $State 'wait-run-1.json'
    $waitJob = Start-Process -FilePath $Cli -ArgumentList (@('--pid',"$($app.Id)") + $waitArgs + @('--timeout','15')) `
        -NoNewWindow -PassThru -RedirectStandardOutput $waitOutput
    New-Item -ItemType File -Path (Join-Path $State 'release-roll') | Out-Null
    $null = Control @('wait-for-text','--pane',"$($panes.East)",'--timeout','10','East started roll')
    $null = Wait-Until 'the CLI reads the running East host while the wait is pending' {
        $run = (Run-Snapshot 1).run
        $run.recipients[1].delivery.state -ceq 'rejected' -and
            $run.recipients[2].execution.state -ceq 'running'
    }
    if ($waitJob.HasExited) { throw 'The group wait ended before East finished' }
    New-Item -ItemType File -Path (Join-Path $State 'release-east-roll') | Out-Null
    $null = Control @('wait-for-text','--pane',"$($panes.East)",'--timeout','10','East finished roll')
    if (!$waitJob.WaitForExit(10000)) {
        throw 'Wait did not finish after the background run settled'
    }
    $waitReply = [IO.File]::ReadAllText($waitOutput) | ConvertFrom-Json
    if (!$waitReply.ok) { throw "Wait did not return the completed background run: $($waitReply.error)" }
    $settled = $waitReply.result
    $run = $settled.run
    $westResult, $northResult, $eastResult = $run.recipients
    if ($settled.window_id -ne $groupWindow -or $settled.tab_id -ne $groupTab -or
        $run.summary.settled_count -ne 3 -or $run.summary.outcomes.succeeded -ne 2 -or
        $run.summary.outcomes.attention -ne 1 -or $westResult.execution.exit_code -ne 0 -or
        $northResult.delivery.state -cne 'rejected' -or $northResult.delivery.reason -notlike 'The verified prompt changed*' -or
        $eastResult.execution.exit_code -ne 0 -or $eastResult.execution.command_id -le 0 -or
        $null -eq $eastResult.execution.connection_generation -or
        ($eastResult.PSObject.Properties.Name -contains 'output')) {
        throw 'Wait did not return the final recipient outcomes'
    }
    $again = Control ($waitArgs + @('--timeout','1'))
    if ($again.run.summary.settled_count -ne 3 -or !(Find-Element 'No bells or notifications') -or
        [FixtureMessages]::GetForegroundWindow() -ne $activeWindow) {
        throw 'Waiting again did not return the settled run or changed focus'
    }
    $strict = & $Cli --pid $app.Id @waitArgs --require-success | ConvertFrom-Json
    if ($LASTEXITCODE -ne 2 -or !$strict.ok -or $strict.result.run.recipients[1].delivery.state -cne 'rejected') {
        throw 'A settled failure did not preserve the JSON report with exit code 2'
    }
    $tab = Wait-Until 'the group tab' { Find-Element $group ([System.Windows.Automation.ControlType]::TabItem) }
    $tab.GetCurrentPattern($item).Select()
    $null = Wait-Until 'North was tried before East despite the new roster sort' {
        Find-Element "$($labels.North)${dot}Not sent${dot}The verified prompt changed before the command was sent."
    }
    $eastExecution = (Run-Snapshot 1).run.recipients[2].execution
    $output = Control @('get-command','--pane',"$($panes.East)",'--id',"$($eastExecution.command_id)",
        '--connection-generation',"$($eastExecution.connection_generation)")
    if ($output.output_error -or $output.output -notlike '*East finished roll*') {
        throw 'The completed result did not link to the original pane output'
    }
    $missing = & $Cli --pid $app.Id get-ssh-group-run --window $groupWindow --tab $groupTab --run 900 | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $missing.ok -or $missing.error -notlike '*not retained*') {
        throw 'An unknown run did not return an explicit lookup error'
    }
    Click 'Preview send order'
    $null = Wait-Until 'the next draft goes back to the roster order' {
        (Find-Element "1. $($labels.West)") -and (Find-Element "2. $($labels.East)") -and
            !(Find-Element 'Use roster order' $button)
    }
    Click 'Back to roster and results'

    (Wait-CheckBox $labels.North On).Toggle()
    $null = Wait-CheckBox $labels.North Off
    Set-Command 'keep-first'
    Click ('Send and record' + [char]0x2026)
    $dialog = Wait-Until 'output recording dialog' { Find-OwnedDialog 'Record SSH group output' }
    $null = Wait-Until 'start the bounded recording' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started keep-first')
    $stopped = (Queue-Action 2 'stop').run
    $kept = $stopped.recipients | Where-Object pane_id -eq $panes.West
    $withheld = $stopped.recipients | Where-Object pane_id -eq $panes.East
    if ($stopped.queue.state -cne 'drained' -or $stopped.queue.unsent -ne 0 -or
        $kept.execution.state -cne 'running' -or $withheld.delivery.state -cne 'rejected' -or
        $withheld.delivery.reason -cne 'Cancelled before sending.') {
        throw 'Stopping the CLI queue did not withhold the unsent host and leave the running host alone'
    }
    $null = Wait-Until 'East was canceled before sending' {
        Find-Element "$($labels.East)${dot}Not sent${dot}Cancelled before sending."
    }
    New-Item -ItemType File -Path (Join-Path $State 'release-keep-first') | Out-Null
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West finished keep-first')
    $log = Join-Path $State 'ssh-group-output-2.txt'
    $null = Wait-Until 'the recording includes the cancelled delivery and finite limit' {
        $recorded = Read-GroupRecording $log
        $recorded -and $recorded.Contains('Cancelled before sending.') -and
            $recorded.Contains("Dispatch${dot}parallel limit 1${dot}no delay") -and
            $recorded.Contains('[Queue activity]') -and $recorded.Contains('Cancelled queued commands')
    }
    (Wait-CheckBox 'Stop queue on failure' Off).Toggle()
    $null = Wait-CheckBox 'Stop queue on failure' On
    Set-Command 'halt-on-failure'
    Click 'Send to 2 hosts'
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started halt-on-failure')
    $null = Wait-Until 'East is queued until West settles' {
        Find-Element "$($labels.East)${dot}Queued"
    }
    Click 'Pause queue'
    $null = Wait-Until 'queue is paused before West fails' { Find-Element 'Command queue paused' }
    New-Item -ItemType File -Path (Join-Path $State 'release-halt-on-failure') | Out-Null
    $null = Wait-Until 'the failure withholds the remaining command' {
        (Find-Element "$($labels.West)${dot}Exit 17") -and
            (Find-Element "$($labels.East)${dot}Not sent${dot}Queue stopped after an unsuccessful host.") -and
            !(Find-Element 'Resume queue' $button)
    }

    Click 'Show queue activity'
    $null = Wait-Until 'the paused run records why it stopped' {
        (Find-Element 'Paused queue') -and (Find-Element 'Stopped after an unsuccessful host')
    }
    Click 'Hide queue activity'

    Set-Command 'inspect'
    Click 'Send to 2 hosts'
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started inspect')
    Click 'Attention inbox, 0 unread'
    $null = Wait-Until 'the queue is controlled from the background' { Find-Element 'No bells or notifications' }
    $queueFocus = [FixtureMessages]::GetForegroundWindow()
    $paused = (Queue-Action 4 'pause').run
    $pausedAgain = (Queue-Action 4 'pause').run
    if ($paused.queue.state -cne 'paused' -or $paused.queue.unsent -ne 1 -or
        $pausedAgain.queue_activity.events.Count -ne $paused.queue_activity.events.Count) {
        throw 'The background queue did not pause or logged a duplicate pause'
    }
    New-Item -ItemType File -Path (Join-Path $State 'release-inspect') | Out-Null
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West finished inspect')
    $null = Wait-Until 'the queue remains paused with a free slot' {
        $run = (Run-Snapshot 4).run
        $first = $run.recipients | Where-Object pane_id -eq $panes.West
        $next = $run.recipients | Where-Object pane_id -eq $panes.East
        $run.queue.state -ceq 'paused' -and $first.execution.exit_code -eq 0 -and $next.delivery.state -ceq 'queued'
    }
    New-Item -ItemType File -Path (Join-Path $State 'resume-inspect') | Out-Null
    $resumed = (Queue-Action 4 'resume').run
    if ($resumed.queue.state -ceq 'paused' -or
        [FixtureMessages]::GetForegroundWindow() -ne $queueFocus -or !(Find-Element 'No bells or notifications')) {
        throw 'Resuming the queue changed focus or left it paused'
    }
    $null = Control @('wait-for-text','--pane',"$($panes.East)",'--timeout','10','East finished inspect')
    $tab = Wait-Until 'the group tab after resuming' { Find-Element $group ([System.Windows.Automation.ControlType]::TabItem) }
    $tab.GetCurrentPattern($item).Select()
    $null = Wait-Until 'East was sent the resumed command' {
        Find-Element "$($labels.East)${dot}Sent to SSH channel"
    }
    Click 'Show queue activity'
    $null = Wait-Until 'the completed run retains pause and resume events' {
        (Find-Element 'Paused queue') -and (Find-Element 'Resumed queue')
    }
    Click 'Hide queue activity'

    Select-GroupOption 'Parallel SSH commands' 'Unlimited'
    Select-GroupOption 'Canary host' $labels.East
    Set-Command 'canary-ok'
    (Wait-CheckBox $labels.East On).Toggle()
    $null = Wait-Until 'removing the canary blocks submission' {
        $send = Find-Element 'Run canary first' $button
        ($send -and !$send.Current.IsEnabled) -and
            (Find-Element 'The canary must be checked, visible, and at a fresh shell marker.')
    }
    (Wait-CheckBox $labels.East Off).Toggle()
    (Wait-CheckBox 'Notify when canary finishes' Off).Toggle()
    $null = Wait-CheckBox 'Notify when canary finishes' On
    Click 'Preview send order'
    $null = Wait-Until 'the preview puts the chosen canary ahead of the other ready host' {
        (Find-Element "Canary first${dot}$($labels.East)") -and (Find-Element "Ready${dot}$($labels.West)") -and
            (Find-Element 'After approval, send to all remaining ready hosts.')
    }
    Click ('Run canary and record' + [char]0x2026)
    $dialog = Wait-Until 'canary recording dialog' { Find-OwnedDialog 'Record SSH group output' }
    $null = Wait-Until 'start the canary recording' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Control @('wait-for-text','--pane',"$($panes.East)",'--timeout','10','East started canary-ok')
    $blocked = & $Cli --pid $app.Id ssh-group-queue --window $groupWindow --tab $groupTab --run 5 resume | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $blocked.ok -or $blocked.error -notlike '*while the canary is pending*') {
        throw 'Resuming a command queue bypassed its canary gate'
    }
    $running = (Run-Snapshot 5).run.queue
    $early = & $Cli --pid $app.Id ssh-group-queue --window $groupWindow --tab $groupTab --run 5 approve-canary --pane $panes.East | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $early.ok -or $early.error -notlike '*successful exit yet*' -or
        $running.state -cne 'canary_running' -or $running.canary_pane_id -ne $panes.East) {
        throw 'The CLI could not identify the running canary or accepted approval before its exit'
    }
    $cannotSkip = & $Cli --pid $app.Id ssh-group-queue --window $groupWindow --tab $groupTab --run 5 skip --pane $panes.East | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $cannotSkip.ok -or $cannotSkip.error -notlike '*active canary cannot be skipped*') {
        throw 'The CLI let the running canary be skipped'
    }
    $null = Wait-Until 'only East runs before a canary result' {
        (Find-Element "$($labels.West)${dot}Queued") -and
            (Find-Element "$($labels.East)${dot}Waiting for the canary result") -and
            !(Find-Element 'Approve remaining hosts' $button)
    }
    Click 'Notify when run finishes'
    $null = Wait-Until 'both notifications are armed' {
        (Find-Element 'Cancel run notification' $button) -and (Find-Element 'Cancel canary notification' $button)
    }
    Click 'Preview send order'
    $null = Wait-Until 'the preview is open before leaving the group' { Find-Element 'Back to roster and results' $button }
    Click 'Attention inbox, 0 unread'
    $null = Wait-Until 'the canary group is in the background' { Find-Element 'No bells or notifications' }
    $approvalArgs = @('wait-ssh-group-approval','--window',"$groupWindow",'--tab',"$groupTab",'--run')
    $approvalTimeout = & $Cli --pid $app.Id @approvalArgs 5 --timeout 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $approvalTimeout.ok -or $approvalTimeout.error -notlike '*timed out before the canary was ready*') {
        throw 'Waiting for approval did not time out while East was held'
    }
    $approvalOutput = Join-Path $State 'wait-canary-approval.json'
    $approvalJob = Start-Process -FilePath $Cli -ArgumentList (@('--pid',"$($app.Id)") + $approvalArgs + @('5','--timeout','15')) `
        -NoNewWindow -PassThru -RedirectStandardOutput $approvalOutput
    $waitFocus = [FixtureMessages]::GetForegroundWindow()
    New-Item -ItemType File -Path (Join-Path $State 'release-east-canary-ok') | Out-Null
    if (!$approvalJob.WaitForExit(10000)) {
        throw 'The background wait did not return when the canary became ready'
    }
    $approval = [IO.File]::ReadAllText($approvalOutput) | ConvertFrom-Json
    if (!$approval.ok -or $approval.result.window_id -ne $groupWindow -or $approval.result.tab_id -ne $groupTab -or
        $approval.result.run.queue.state -cne 'canary_approval' -or $approval.result.run.queue.canary_pane_id -ne $panes.East -or
        $approval.result.run.queue.unsent -ne 1 -or [FixtureMessages]::GetForegroundWindow() -ne $waitFocus) {
        throw 'The approval wait lost the pending gate, targeted the wrong host, or changed focus'
    }
    $canaryNotice = "$group${dot}Run 5${dot}Canary ready for approval${dot}$($labels.East)"
    Click "Unread${dot}$canaryNotice"
    $null = Wait-Until 'the canary notice opens the run requiring approval' {
        (Find-Element "$($labels.East)${dot}Exit 0") -and
            (Find-Element "$($labels.West)${dot}Queued") -and
            (Find-Element "$($labels.East)${dot}Canary succeeded. Review its result before continuing.") -and
            (Find-Element 'Approve remaining hosts' $button)
    }
    Click 'Attention inbox, 0 unread'
    $null = Wait-Until 'the approved run will stay in the background' {
        Find-Element "Read${dot}$canaryNotice" $button
    }
    $approvalFocus = [FixtureMessages]::GetForegroundWindow()
    $wrong = & $Cli --pid $app.Id ssh-group-queue --window $groupWindow --tab $groupTab --run 5 approve-canary --pane $panes.West | ConvertFrom-Json
    $waiting = (Run-Snapshot 5).run.queue
    if ($wrong.ok -or $wrong.error -notlike '*not the canary for this run*' -or
        $waiting.state -cne 'canary_approval' -or $waiting.canary_pane_id -ne $panes.East -or $waiting.unsent -ne 1) {
        throw 'Approving a different pane changed the waiting canary gate'
    }
    New-Item -ItemType File -Path (Join-Path $State 'release-canary-ok') | Out-Null
    New-Item -ItemType File -Path (Join-Path $State 'approve-canary-ok') | Out-Null
    $approved = (Control @('ssh-group-queue','--window',"$groupWindow",'--tab',"$groupTab",'--run','5',
            'approve-canary','--pane',"$($panes.East)")).run.queue
    if ($approved.state -notin @('dispatching','drained') -or $null -ne $approved.canary_pane_id -or
        [FixtureMessages]::GetForegroundWindow() -ne $approvalFocus -or
        !(Find-Element "Read${dot}$canaryNotice" $button)) {
        throw 'Approving the correct canary left the gate in place or changed focus'
    }
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West finished canary-ok')
    $completedGate = & $Cli --pid $app.Id @approvalArgs 5 --timeout 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $completedGate.ok -or $completedGate.error -notlike '*no canary awaiting approval*') {
        throw 'A second approval wait accepted a gate that was already approved'
    }
    $null = Wait-Until 'the canary notice stays read while the run completion is unread' {
        Find-Element "Read${dot}$canaryNotice" $button
    }
    Click "Unread${dot}$group${dot}Run 5${dot}Group run finished${dot}2 succeeded${dot}0 need attention"
    $canaryPolicy = "Dispatch${dot}unlimited parallel${dot}no delay${dot}canary $($labels.East)"
    $null = Wait-Until 'the completion alert returns to the canary run' {
        (Find-Element "Submitted${dot}canary-ok") -and (Find-Element $canaryPolicy)
    }
    Select-GroupOption 'Time between sends' '1 second'
    $null = Wait-Until 'changing the draft interval leaves the completed canary policy intact' {
        Find-Element $canaryPolicy
    }
    Select-GroupOption 'Time between sends' 'No delay'
    $log = Join-Path $State 'ssh-group-output-5.txt'
    $null = Wait-Until 'the recording names the original canary' {
        $recorded = Read-GroupRecording $log
        if (!$recorded -or !$recorded.Contains($canaryPolicy) -or !$recorded.Contains('[Group run settled]')) { return }
        $readyIndex = $recorded.IndexOf('Canary ready for approval')
        $approveIndex = $recorded.IndexOf('Approved remaining hosts')
        $readyIndex -ge 0 -and $approveIndex -gt $readyIndex
    }

    Set-Command 'canary-fails'
    Click 'Run canary first'
    Click 'Attention inbox, 1 unread'
    Click "Unread${dot}$group${dot}Run 6${dot}Canary did not report a successful exit${dot}$($labels.East)"
    $failedGate = & $Cli --pid $app.Id @approvalArgs 6 --timeout 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 1 -or $failedGate.ok -or $failedGate.error -notlike '*canary gate closed before approval*') {
        throw 'A failed canary did not resolve the approval wait as an error'
    }
    $null = Wait-Until 'a failed canary notice opens the exact run that withheld West' {
        (Find-Element "$($labels.East)${dot}Exit 17") -and
            (Find-Element "$($labels.West)${dot}Not sent${dot}Canary did not report a successful exit.") -and
            !(Find-Element 'Approve remaining hosts' $button)
    }

    Set-Command 'canary-unknown'
    Click 'Run canary first'
    $null = Control @('wait-for-text','--pane',"$($panes.East)",'--timeout','10','East started canary-unknown')
    Click 'Cancel canary notification'
    $null = Wait-Until 'canary notification is disarmed' { !(Find-Element 'Cancel canary notification' $button) }
    New-Item -ItemType File -Path (Join-Path $State 'release-east-canary-unknown') | Out-Null
    $null = Wait-Until 'a canary without an exit status withholds West without an alert' {
        (Find-Element "$($labels.East)${dot}Completed${dot}exit status unavailable") -and
            (Find-Element "$($labels.West)${dot}Not sent${dot}Canary did not report a successful exit.") -and
            !(Find-Element 'Approve remaining hosts' $button) -and
            (Find-Element 'Attention inbox, 0 unread' $button)
    }

    (Wait-CheckBox 'Notify when canary finishes' On).Toggle()
    $null = Wait-CheckBox 'Notify when canary finishes' Off
    Set-Command 'canary-stale'
    Click 'Run canary first'
    $null = Wait-Until 'the canary succeeds before West changes its prompt' {
        (Find-Element "$($labels.East)${dot}Exit 0") -and
            (Find-Element "$($labels.West)${dot}Queued") -and (Find-Element 'Approve remaining hosts' $button)
    }
    $null = Control @('send-text','--pane',"$($panes.West)",'--no-paste','x')
    Click 'Approve remaining hosts'
    $null = Wait-Until 'approval still rechecks the queued prompt' {
        Find-Element "$($labels.West)${dot}Not sent${dot}The verified prompt changed before the command was sent."
    }

    $null = Control @('send-text','--pane',"$($panes.West)",'--no-paste','restore-prompt')
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West restored prompt')
    Select-GroupOption 'Canary host' 'No canary'
    Select-GroupOption 'Time between sends' '5 seconds'
    Set-Command 'pace'
    Click 'Preview send order'
    $null = Wait-Until 'the unlimited queue shows its dispatch interval and order' {
        (Find-Element "Dispatch interval${dot}5s") -and
            (Find-Element "1. $($labels.West)") -and (Find-Element "2. $($labels.East)")
    }
    Click 'Send to 2 hosts'
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West finished pace')
    $null = Wait-Until 'East still waits after the first command finishes' {
        (Find-Element 'Waiting for the next send interval') -and (Find-Element "$($labels.East)${dot}Queued") -and
            (Find-Element "Dispatch${dot}unlimited parallel${dot}5s between sends${dot}stop on failure")
    }
    Click 'Attention inbox, 0 unread'
    $null = Wait-Until 'the group is in the background during the interval' { Find-Element "Read${dot}$canaryNotice" $button }
    $null = Control @('wait-for-text','--pane',"$($panes.East)",'--timeout','15','East finished pace')

    $tab = Wait-Until 'the group tab after the paced run' { Find-Element $group ([System.Windows.Automation.ControlType]::TabItem) }
    $tab.GetCurrentPattern($item).Select()
    $null = Control @('send-text','--pane',"$($panes.North)",'--no-paste','restore-prompt')
    $null = Control @('wait-for-text','--pane',"$($panes.North)",'--timeout','10','North restored prompt')
    (Wait-CheckBox $labels.North Off).Toggle()
    $null = Wait-CheckBox $labels.North On
    Select-GroupOption 'Time between sends' 'No delay'
    Select-GroupOption 'Parallel SSH commands' '1 at a time'
    $null = Wait-CheckBox 'Stop queue on failure' On
    Set-Command 'skip-middle'
    Click ('Send and record' + [char]0x2026)
    $dialog = Wait-Until 'skip run recording dialog' { Find-OwnedDialog 'Record SSH group output' }
    $null = Wait-Until 'start the skip run' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started skip-middle')
    $null = Wait-Until 'only queued hosts can be skipped' {
        (Find-Element "Skip queued pane $($panes.East)" $button) -and
            (Find-Element "Skip queued pane $($panes.North)" $button) -and
            !(Find-Element "Skip queued pane $($panes.West)" $button)
    }
    Click 'Attention inbox, 0 unread'
    $null = Wait-Until 'the skip is issued with the group in the background' { Find-Element "Read${dot}$canaryNotice" $button }
    $skipFocus = [FixtureMessages]::GetForegroundWindow()
    $runningSkip = & $Cli --pid $app.Id ssh-group-queue --window $groupWindow --tab $groupTab --run 10 skip --pane $panes.West | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $runningSkip.ok -or $runningSkip.error -notlike '*no queued command to skip*') {
        throw 'The CLI let an already running host be skipped'
    }
    $skipped = (Control @('ssh-group-queue','--window',"$groupWindow",'--tab',"$groupTab",'--run','10',
            'skip','--pane',"$($panes.East)")).run
    $east = $skipped.recipients | Where-Object pane_id -eq $panes.East
    $north = $skipped.recipients | Where-Object pane_id -eq $panes.North
    $west = $skipped.recipients | Where-Object pane_id -eq $panes.West
    if ($skipped.queue.unsent -ne 1 -or $east.delivery.state -cne 'skipped' -or
        $north.delivery.state -cne 'queued' -or $west.execution.state -cne 'running' -or
        [FixtureMessages]::GetForegroundWindow() -ne $skipFocus -or !(Find-Element "Read${dot}$canaryNotice" $button)) {
        throw 'The CLI skip changed focus or modified a host other than East'
    }
    $tab = Wait-Until 'the group tab after skipping' { Find-Element $group ([System.Windows.Automation.ControlType]::TabItem) }
    $tab.GetCurrentPattern($item).Select()
    $null = Wait-Until 'East is skipped while North remains queued' {
        (Find-Element "$($labels.East)${dot}Skipped before sending") -and
            (Find-Element "$($labels.North)${dot}Queued") -and
            !(Find-Element "Skip queued pane $($panes.East)" $button)
    }
    Click 'Notify when run finishes'
    $null = Wait-Until 'the skip completion notice is armed' { Find-Element 'Cancel run notification' $button }
    New-Item -ItemType File -Path (Join-Path $State 'release-skip-middle') | Out-Null
    $null = Control @('wait-for-text','--pane',"$($panes.North)",'--timeout','10','North finished skip-middle')

    Click 'Attention inbox, 1 unread'
    Click "Unread${dot}$group${dot}Run 10${dot}Group run finished${dot}2 succeeded${dot}0 need attention${dot}1 skipped"
    $null = Wait-Until 'the skip appears as a settled outcome without attention' {
        Find-Element "3 of 3 settled${dot}0 waiting${dot}0 running${dot}2 succeeded${dot}0 need attention${dot}1 skipped"
    }
    Select-GroupOption 'Filter submission results' 'Skipped'
    $null = Wait-Until 'the skipped result can be isolated' {
        (Find-Element "Focus result pane $($panes.East)" $button) -and
            !(Find-Element "Focus result pane $($panes.West)" $button)
    }
    $log = Join-Path $State 'ssh-group-output-10.txt'
    $null = Wait-Until 'the saved recording identifies the skipped host' {
        $recorded = Read-GroupRecording $log
        $recorded -and $recorded.Contains("Skipped queued host${dot}Pane $($panes.East)") -and
            $recorded.Contains("$($labels.East)${dot}Skipped before sending") -and $recorded.Contains('[Group run settled]')
    }

    Select-GroupOption 'Filter submission results' 'All results'
    Select-GroupOption 'Expire unsent commands' '5 seconds'
    Set-Command 'deadline'
    Click 'Preview send order'
    $null = Wait-Until 'the queue deadline is visible in preview' { Find-Element 'Unsent commands expire after 5s' }
    Click ('Send and record' + [char]0x2026)
    $dialog = Wait-Until 'deadline recording dialog' { Find-OwnedDialog 'Record SSH group output' }
    $null = Wait-Until 'start the deadline run' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started deadline')
    $elapsed = [Diagnostics.Stopwatch]::StartNew()
    Click 'Pause queue'
    Click 'Notify when run finishes'
    $null = Wait-Until 'the queue is paused and waiting for the run notice' {
        (Find-Element 'Command queue paused') -and (Find-Element 'Cancel run notification' $button)
    }
    Click 'Attention inbox, 0 unread'
    $null = Wait-Until 'the deadline passes with the paused group in the background' { $elapsed.ElapsedMilliseconds -ge 5200 }
    New-Item -ItemType File -Path (Join-Path $State 'release-deadline') | Out-Null
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West finished deadline')
    Click "Unread${dot}$group${dot}Run 11${dot}Group run finished${dot}1 succeeded${dot}2 need attention"
    $null = Wait-Until 'background expiry withheld the queued hosts but kept the active command' {
        (Find-Element "$($labels.East)${dot}Not sent${dot}Queue expired before sending.") -and
            (Find-Element "$($labels.North)${dot}Not sent${dot}Queue expired before sending.") -and
            (Find-Element "$($labels.West)${dot}Exit 0") -and !(Find-Element 'Resume queue' $button)
    }
    $log = Join-Path $State 'ssh-group-output-11.txt'
    $null = Wait-Until 'the recording keeps the deadline, expiry, and final outcomes' {
        $recorded = Read-GroupRecording $log
        $recorded -and $recorded.Contains('queue expires after 5s') -and $recorded.Contains('Expired unsent commands') -and
            $recorded.Contains('Queue expired before sending.') -and $recorded.Contains('[Group run settled]')
    }

    Select-GroupOption 'Canary host' $labels.East
    Set-Command 'deadline-approval'
    Click 'Run canary first'
    $null = Wait-Until 'a successful canary waits for approval before the deadline' {
        (Find-Element "$($labels.East)${dot}Exit 0") -and (Find-Element 'Approve remaining hosts' $button)
    }
    $null = Wait-Until 'expiry closes the canary approval gate and withholds the remaining hosts' {
        (Find-Element "$($labels.West)${dot}Not sent${dot}Queue expired before sending.") -and
            (Find-Element "$($labels.North)${dot}Not sent${dot}Queue expired before sending.") -and
            !(Find-Element 'Approve remaining hosts' $button)
    }
    Select-GroupOption 'Expire unsent commands' 'Never'
    $null = Control @('wait-ssh-group-ready','--window',"$groupWindow",'--tab',"$groupTab",
        '--pane',"$($panes.East)",'--pane',"$($panes.West)",'--timeout','10')
    $overrideLog = Join-Path $State 'cli-canary-override.txt'
    $submissionArgs = @('submit-ssh-group-command','--window',"$groupWindow",'--tab',"$groupTab",
        '--pane',"$($panes.East)",'--pane',"$($panes.West)",'--canary',"$($panes.West)",
        '--record-output',$overrideLog,'--expect-output','finished verify-override')
    $beforePreview = (Control @('list-ssh-groups')).windows.groups.runs.run_id -join ','
    $preview = Control ($submissionArgs + @('--dry-run','--','verify-override'))
    $afterPreview = (Control @('list-ssh-groups')).windows.groups.runs.run_id -join ','
    if ($preview.window_id -ne $groupWindow -or $preview.tab_id -ne $groupTab -or $null -ne $preview.run_id -or
        $preview.plan.command -cne 'verify-override' -or $preview.plan.dispatch_policy.canary.pane_id -ne $panes.West -or
        ($preview.plan.recipients.pane_id -join ',') -cne "$($panes.West),$($panes.East)" -or
        $beforePreview -cne $afterPreview -or (Test-Path -LiteralPath $overrideLog)) {
        throw 'The dry run planned the wrong recipients or allocated a run or recording'
    }
    $submitted = Control ($submissionArgs + @('--','verify-override'))
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started verify-override')
    $override = (Run-Snapshot $submitted.run_id).run
    if (($override.dispatch_policy | ConvertTo-Json -Compress) -cne
        ($preview.plan.dispatch_policy | ConvertTo-Json -Compress) -or
        $override.queue.state -cne 'canary_running' -or $override.queue.canary_pane_id -ne $panes.West -or
        $override.queue.unsent -ne 1) {
        throw 'The recorded CLI submission did not run its requested canary first'
    }
    New-Item -ItemType File -Path (Join-Path $State 'release-verify-override') | Out-Null
    $null = Wait-Until 'the selected West canary finished and needs approval' {
        (Run-Snapshot $submitted.run_id).run.queue.state -ceq 'canary_approval'
    }
    New-Item -ItemType File -Path (Join-Path $State 'approve-verify-override') | Out-Null
    $null = Control @('ssh-group-queue','--window',"$groupWindow",'--tab',"$groupTab",'--run',"$($submitted.run_id)",
        'approve-canary','--pane',"$($panes.West)")
    $null = Control @('wait-for-text','--pane',"$($panes.East)",'--timeout','10','East finished verify-override')
    $strictSuccess = Control @('wait-ssh-group-run','--window',"$groupWindow",'--tab',"$groupTab",
        '--run',"$($submitted.run_id)",'--require-success','--timeout','10')
    if ($strictSuccess.run.summary.outcomes.succeeded -ne 2 -or
        ($strictSuccess.run.recipients.output_expectation -join ',') -cne 'matched,matched') {
        throw 'Strict wait did not return a successful checked run'
    }
    $null = Wait-Until 'the recording contains the override and both verified outcomes' {
        $recorded = Read-GroupRecording $overrideLog
        $recorded -and $recorded.Contains("canary $($labels.West)") -and
            $recorded.Contains('West finished verify-override') -and
            $recorded.Contains('East finished verify-override') -and
            $recorded.Contains('[Group run settled]')
    }
    Set-Command 'existing draft'
    Click 'Preview send order'
    $null = Wait-Until 'the draft still uses the configured East canary' {
        Find-Element "Canary first${dot}$($labels.East)"
    }
    Click 'Back to roster and results'
    Select-GroupOption 'Canary host' 'No canary'
    Select-GroupOption 'Parallel SSH commands' 'Unlimited'
    $null = Control @('wait-ssh-group-ready','--window',"$groupWindow",'--tab',"$groupTab",
        '--pane',"$($panes.West)",'--pane',"$($panes.East)",'--timeout','10')
    $parallelLog = Join-Path $State 'cli-serial-output.txt'
    $serialArgs = @('submit-ssh-group-command','--window',"$groupWindow",'--tab',"$groupTab",
        '--pane',"$($panes.West)",'--pane',"$($panes.East)")
    $serial = Control ($serialArgs + @('--parallel','1','--record-output',$parallelLog,'--','verify-parallel'))
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started verify-parallel')
    $queued = (Run-Snapshot $serial.run_id).run
    if ($queued.dispatch_policy.parallel_limit -ne 1 -or $queued.queue.unsent -ne 1 -or
        $queued.recipients[1].delivery.state -cne 'queued') {
        throw 'The recorded CLI command did not honor its serial dispatch limit'
    }
    New-Item -ItemType File -Path (Join-Path $State 'release-verify-parallel') | Out-Null
    $settledSerial = Control @('wait-ssh-group-run','--window',"$groupWindow",'--tab',"$groupTab",
        '--run',"$($serial.run_id)",'--require-success','--timeout','10')
    if ($settledSerial.run.recording.state -cne 'saved' -or
        !(Read-GroupRecording $parallelLog).Contains('parallel limit 1')) {
        throw 'The serial run did not preserve its policy in the saved recording'
    }
    $null = Control @('wait-ssh-group-ready','--window',"$groupWindow",'--tab',"$groupTab",
        '--pane',"$($panes.West)",'--pane',"$($panes.East)",'--timeout','10')
    $default = Control ($serialArgs + @('--dry-run','--','verify-parallel'))
    if ($null -ne $default.plan.dispatch_policy.parallel_limit) {
        throw 'The per-run override changed the configured parallel limit'
    }
    Select-GroupOption 'Time between sends' '5 seconds'
    $pacingLog = Join-Path $State 'cli-paced-output.txt'
    $paced = Control ($serialArgs + @('--interval-ms','1200','--record-output',$pacingLog,'--','verify-pacing'))
    $pacedResult = Control @('wait-ssh-group-run','--window',"$groupWindow",'--tab',"$groupTab",
        '--run',"$($paced.run_id)",'--require-success','--timeout','15')
    if ($pacedResult.run.dispatch_policy.interval_ms -ne 1200 -or
        $pacedResult.run.recording.state -cne 'saved' -or !(Read-GroupRecording $pacingLog).Contains('1.2s between sends')) {
        throw 'The recorded run lost its custom pacing interval'
    }
    $null = Control @('wait-ssh-group-ready','--window',"$groupWindow",'--tab',"$groupTab",
        '--pane',"$($panes.West)",'--pane',"$($panes.East)",'--timeout','10')
    $default = Control ($serialArgs + @('--dry-run','--','verify-pacing'))
    $noDelay = Control ($serialArgs + @('--dry-run','--interval-ms','0','--','verify-pacing'))
    if ($default.plan.dispatch_policy.interval_ms -ne 5000 -or $noDelay.plan.dispatch_policy.interval_ms -ne 0) {
        throw 'The pacing override changed the configured interval or failed to represent no delay'
    }
    Select-GroupOption 'Expire unsent commands' '5 seconds'
    $expiryLog = Join-Path $State 'cli-expiry-output.txt'
    $expiring = Control ($serialArgs + @('--parallel','1','--interval-ms','0',
        '--queue-expiry-ms','1800','--record-output',$expiryLog,'--','verify-expiry'))
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started verify-expiry')
    $null = Wait-Until 'expiry withholds East while West stays running' {
        $run = (Run-Snapshot $expiring.run_id).run
        $run.dispatch_policy.queue_expiry_ms -eq 1800 -and $run.queue.state -ceq 'drained' -and
            $run.recipients[0].execution.state -ceq 'running' -and
            $run.recipients[1].delivery.reason -ceq 'Queue expired before sending.'
    }
    New-Item -ItemType File -Path (Join-Path $State 'release-verify-expiry') | Out-Null
    $expired = Control @('wait-ssh-group-run','--window',"$groupWindow",'--tab',"$groupTab",
        '--run',"$($expiring.run_id)",'--timeout','10')
    if ($expired.run.recipients[0].execution.exit_code -ne 0 -or $expired.run.recording.state -cne 'saved' -or
        !(Read-GroupRecording $expiryLog).Contains('queue expires after 1.8s')) {
        throw 'The expiring run interrupted West or lost its policy in the recording'
    }
    $null = Control @('wait-ssh-group-ready','--window',"$groupWindow",'--tab',"$groupTab",
        '--pane',"$($panes.West)",'--pane',"$($panes.East)",'--timeout','10')
    $default = Control ($serialArgs + @('--dry-run','--','verify-expiry'))
    $noExpiry = Control ($serialArgs + @('--dry-run','--queue-expiry-ms','0','--','verify-expiry'))
    if ($default.plan.dispatch_policy.queue_expiry_ms -ne 5000 -or $null -ne $noExpiry.plan.dispatch_policy.queue_expiry_ms) {
        throw 'The expiry override changed the configured deadline or failed to disable expiry'
    }
    Select-GroupOption 'Expire unsent commands' 'Never'
    Select-GroupOption 'Time between sends' 'No delay'
    $groupWait = @('wait-ssh-group-run','--window',"$groupWindow",'--tab',"$groupTab",'--timeout','10','--run')
    $stop = Control ($serialArgs + @('--on-failure','stop','--','policy-stop'))
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started policy-stop')
    New-Item -ItemType File -Path (Join-Path $State 'release-policy-stop') | Out-Null
    $stopped = (Control ($groupWait + @("$($stop.run_id)"))).run
    if (!$stopped.dispatch_policy.stop_on_failure -or $stopped.dispatch_policy.parallel_limit -ne 1 -or
        $stopped.recipients[0].execution.exit_code -ne 17 -or
        $stopped.recipients[1].delivery.reason -cne 'Queue stopped after an unsuccessful host.') {
        throw 'Stop-on-failure did not keep East unsent after West failed'
    }
    $readyForPolicy = @('wait-ssh-group-ready','--window',"$groupWindow",'--tab',"$groupTab",
        '--pane',"$($panes.West)",'--pane',"$($panes.East)",'--timeout','10')
    $null = Control $readyForPolicy
    $keepGoing = Control ($serialArgs + @('--parallel','1','--on-failure','continue','--','policy-continue'))
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started policy-continue')
    New-Item -ItemType File -Path (Join-Path $State 'release-policy-continue') | Out-Null
    $continued = (Control ($groupWait + @("$($keepGoing.run_id)"))).run
    if ($continued.dispatch_policy.stop_on_failure -or $continued.recipients[0].execution.exit_code -ne 17 -or
        $continued.recipients[1].execution.exit_code -ne 0) {
        throw 'Continue-on-failure did not send East after West failed'
    }
    $null = Control $readyForPolicy
    $default = Control ($serialArgs + @('--parallel','1','--dry-run','--','policy-continue'))
    if (!$default.plan.dispatch_policy.stop_on_failure) {
        throw 'The per-run failure policy changed the group default'
    }
    $failedCanary = Control ($serialArgs + @('--canary',"$($panes.West)",'--on-failure','continue','--','policy-canary'))
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started policy-canary')
    New-Item -ItemType File -Path (Join-Path $State 'release-policy-canary') | Out-Null
    $canaryResult = (Control ($groupWait + @("$($failedCanary.run_id)"))).run
    if ($canaryResult.recipients[0].execution.exit_code -ne 17 -or
        $canaryResult.recipients[1].delivery.reason -cne 'Canary did not report a successful exit.') {
        throw 'Continue-on-failure bypassed the failed canary'
    }
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    if ($approvalJob) {
        if (!$approvalJob.HasExited) { $approvalJob.Kill(); $approvalJob.WaitForExit() }
        $approvalJob.Dispose()
    }
    if ($waitJob) {
        if (!$waitJob.HasExited) { $waitJob.Kill(); $waitJob.WaitForExit() }
        $waitJob.Dispose()
    }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
