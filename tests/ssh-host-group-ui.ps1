[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
Add-Type -AssemblyName System.Windows.Forms
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_TEST_COPY_FILE = Join-Path $State 'group-result-clipboard.txt'
$env:W11TERM_TEST_EXPLORER_TRACE = Join-Path $State 'recording-explorer.json'
$env:W11TERM_EXPORT_SMOKE_DIR = Join-Path $State 'exports'
$null = [IO.Directory]::CreateDirectory($env:W11TERM_EXPORT_SMOKE_DIR)
$null = [IO.Directory]::CreateDirectory($env:W11TERM_DATA_DIR)
$library = Join-Path $env:W11TERM_DATA_DIR 'saved-commands.json'
$profiles = Join-Path $State 'profiles.json'
$profileData = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

function Save-FixtureCommand([string]$Text) {
    $entries = @(@{ name = 'Fleet health'; text = $Text }, @{ name = 'Fleet uptime'; text = 'uptime' })
    [IO.File]::WriteAllText($library, (ConvertTo-Json -InputObject $entries))
}
Save-FixtureCommand 'show old health'

function Pane-Status([uint64]$Pane, [string]$Status) {
    $null = Wait-Until "pane $Pane status '$Status'" { Find-Element "Pane $Pane$dot$Status" }
}
function Target-Name([string]$Name, [uint64]$Pane) {
    $port = $profileData.$Name.port
    "$Name${dot}test@127.0.0.1:${port}${dot}Pane $Pane"
}
function Send-Raw([uint64]$Pane, [string]$Text) {
    $null = Control @('send-text','--pane',"$Pane",'--no-paste',$Text)
}
function Set-HostFilter([string]$Query) {
    $field = Wait-Until 'host filter editor' {
        $element = Find-Element 'Host filter' $edit
        $provider = $null
        if ($element -and $element.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $field.SetValue($Query)
}
function Command-Editor([string]$Name = 'Group command') {
    Wait-Until "command editor '$Name'" {
        $element = Find-Element $Name $edit
        $provider = $null
        if ($element -and $element.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider)) { $provider }
    }
}
function Wait-CommandDraft([string]$Text) {
    $null = Wait-Until "focused command draft '$Text'" {
        $field = Find-Element 'Group command' $edit
        $provider = $null
        $field -and $field.Current.HasKeyboardFocus -and $field.TryGetCurrentPattern(
            [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider
        ) -and $provider.Current.Value -ceq $Text
    }
}
function Submit([string]$Text, [string]$ButtonName) {
    (Command-Editor).SetValue($Text)
    Invoke-Button (Wait-Until "enabled $ButtonName" {
        $entry = Find-Element $ButtonName $button
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
    $null = Wait-Until "submission for '$Text'" { Find-Element "Submitted$dot$Text" }
}
function Outcome([string]$Target, [string]$Status) {
    $null = Wait-Until "$Target $Status" { Find-Element "$Target$dot$Status" }
}
function Result-Timing([uint64]$Pane) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $byId = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, "ssh-group-result-time-$Pane")
    $entry = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $byId)
    if ($entry) { $entry.Current.Name }
}
function Combo-Popup([string]$ControlName, [string]$Expected) {
    Wait-Until "combo '$ControlName' with selection '$Expected'" {
        $entry = Find-Element $ControlName ([System.Windows.Automation.ControlType]::ComboBox)
        $provider = $null
        $selection = $null
        if ($entry -and $entry.TryGetCurrentPattern(
                [System.Windows.Automation.SelectionPattern]::Pattern, [ref]$selection
            ) -and $entry.TryGetCurrentPattern(
                [System.Windows.Automation.ExpandCollapsePattern]::Pattern, [ref]$provider
            )) {
            $items = $selection.Current.GetSelection()
            if (!$Expected -or ($items.Count -eq 1 -and $items[0].Current.Name -ceq $Expected)) { $provider }
        }
    }
}
function Select-ComboValue([string]$ControlName, [string]$Name) {
    (Combo-Popup $ControlName '').Expand()
    $choice = Wait-Until "combo item '$Name'" {
        $entry = Find-Element $Name ([System.Windows.Automation.ControlType]::ListItem)
        $provider = $null
        if ($entry -and $entry.Current.IsEnabled -and !$entry.Current.IsOffscreen -and
            $entry.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$provider)) {
            $provider
        }
    }
    $choice.Select()
    $popup = Combo-Popup $ControlName $Name
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}
function Set-ResultFilter([string]$Name) {
    Select-ComboValue 'Filter submission results' $Name
}
function Roster-Snapshot {
    (Control @('get-ssh-group-roster','--window',"$groupWindow",'--tab',"$groupTab")).roster
}
function Set-RecipientSearch([string]$Query) {
    $field = Wait-Until 'result recipient search' { Find-Element 'Search result recipients' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Query)
}
function Expect-PaneButtonOrder([string]$Prefix, [uint64[]]$Panes) {
    $expected = ($Panes | ForEach-Object { "$Prefix$_" }) -join '|'
    $null = Wait-Until "pane order '$expected'" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $byButton = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $button)
        $actual = foreach ($entry in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $byButton)) {
            $name = $entry.Current.Name
            if ($name.StartsWith($Prefix, [StringComparison]::Ordinal)) { $name }
        }
        ($actual -join '|') -ceq $expected
    }
}
function Expect-ResultOrder([uint64[]]$Panes) {
    Expect-PaneButtonOrder 'Focus result pane ' $Panes
}
function Result-Progress([string]$Caption) {
    $null = Wait-Until 'submission progress counts' { Find-Element $Caption }
}
function Click([string]$Name) {
    $invoke = Wait-Until "enabled '$Name'" {
        $entry = Find-Element $Name $button
        $provider = $null
        if ($entry -and $entry.Current.IsEnabled -and $entry.TryGetCurrentPattern(
                [System.Windows.Automation.InvokePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $invoke.Invoke()
}

function Wait-ArmedOutputAlert([string]$Summary, [string]$Query) {
    $null = Wait-Until "armed output alert for '$Query'" {
        (Find-Element $Summary) -and (Find-Element 'Cancel output alert' $button) -and
            (Find-Element "Armed output search${dot}$Query")
    }
}

function Wait-RecordingStatus([string]$Prefix, [string]$Path, [switch]$PassThru) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $text = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    $last = @{ Status = @() }
    try {
        $reported = Wait-Until "$Prefix$Path" {
            $last.Status = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text) |
                ForEach-Object { $_.Current.Name } |
                Where-Object { $_ -match 'recording|record live output|live output to|output file' })
            foreach ($name in $last.Status) {
                if ($name.StartsWith($Prefix, [StringComparison]::Ordinal) -and
                    [FixtureDialog]::SameFile($Path, $name.Substring($Prefix.Length))) {
                    return $name.Substring($Prefix.Length)
                }
            }
        }
        if ($PassThru) { return $reported }
    } catch {
        throw "$($_.Exception.Message). Recording statuses: $($last.Status -join ' | '). Expected file exists: $([IO.File]::Exists($Path))."
    }
}

$launchArgs = @('--profiles',('"' + $profiles + '"'),'--profile','East',
    '--tab','West','--tab','"Local witness"','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $launchArgs -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
$readyJob = $null
$explicitReadyJob = $null
$outputJob = $null
$followJob = $null
$followReader = $null
$logReader = $null
$explorer = $null
$priorExplorerWindows = @()
try {
    $window = Wait-InputWindow
    $sessions = (Control @('list')).sessions
    $east = ($sessions | Where-Object label -eq East).id
    $west = ($sessions | Where-Object label -eq West).id
    $local = ($sessions | Where-Object label -eq 'Local witness').id
    if (!$east -or !$west -or !$local -or ($sessions.tab_id | Sort-Object -Unique).Count -ne 3) {
        throw 'Expected two SSH panes and PowerShell in their own tabs'
    }
    $null = Control @('wait-for-text','--pane',"$east",'--timeout','15','east$')
    $westShellGate = [IO.File]::ReadAllText((Join-Path $State 'west-shell-gate-path'))
    Press-Key 0x77 # F8 selects the East tab in the fixture keymap.
    Wait-Focus $east
    Press-Key 0x10 # An unencoded Shift must not consume the verified prompt.
    Run-Palette 'SSH host group'
    Pane-Status $east "Connected${dot}Ready"
    Pane-Status $west 'Connecting'
    $owner = (Control @('list-ssh-groups')).windows | Where-Object { $_.groups.label -ccontains 'SSH host group' }
    $groupWindow = $owner.window_id
    $groupTab = ($owner.groups | Where-Object label -ceq 'SSH host group').tab_id
    if (!$groupWindow -or !$groupTab) { throw 'The CLI did not list the active SSH group' }
    $readyArgs = @('wait-ssh-group-ready','--window',"$groupWindow",'--tab',"$groupTab")
    $hostStateFilter = 'Filter host state'
    Select-ComboValue $hostStateFilter 'Connecting'
    $null = Wait-Until 'the connecting filter isolates West during its shell request' {
        (Find-Element "Focus pane $west" $button) -and !(Find-Element "Focus pane $east" $button) -and
            !(Find-Element "Reconnect SSH pane $west" $button)
    }
    $roster = Roster-Snapshot
    $eastRoster = $roster.hosts | Where-Object pane_id -eq $east
    $westRoster = $roster.hosts | Where-Object pane_id -eq $west
    if ($roster.scope -cne 'window' -or $roster.filter.state -cne 'connecting' -or $roster.counts.hosts -ne 2 -or
        $roster.counts.shown -ne 1 -or $eastRoster.shown -or $eastRoster.prompt_verification -cne 'shell_markers' -or
        !$westRoster.shown -or $westRoster.connected -or $westRoster.state -cne 'connecting' -or
        $null -ne ($roster.hosts | Where-Object pane_id -eq $local)) {
        throw 'The CLI roster did not preserve the live filter and SSH pane states'
    }
    Remove-Item -LiteralPath $westShellGate
    Select-ComboValue $hostStateFilter 'All host states'
    Pane-Status $west "Connected${dot}Ready"
    if (Find-Element "Focus pane $local" $button) { throw 'The local shell appeared in the SSH roster' }
    $eastName = Target-Name East $east
    $westName = Target-Name West $west
    Click 'Select shown'
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On
    Set-HostFilter 'no-such-host'
    $null = Wait-Until 'no matching SSH panes' { Find-Element 'No SSH panes match this filter.' }
    $null = Wait-Until 'an empty filter result cannot send' {
        $entry = Find-Element 'Send to 0 hosts' $button
        $entry -and !$entry.Current.IsEnabled
    }
    $emptyWait = & $Cli --pid $app.Id @readyArgs --timeout 1 | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $emptyWait.ok -or $emptyWait.error -notlike 'No checked, shown*') {
        throw 'The readiness wait accepted an empty host scope'
    }
    $eastAddress = "EaST 127.0.0.1:$($profileData.East.port)"
    Set-HostFilter $eastAddress
    $null = Wait-Until 'only East is shown' {
        (Find-Element "Focus pane $east" $button) -and !(Find-Element "Focus pane $west" $button)
    }
    $null = Wait-Until 'the hidden selection is disclosed' {
        Find-Element '1 selected host is hidden by the filter.'
    }
    (Wait-CheckBox 'Use wildcards' Off).Toggle()
    Set-HostFilter 'Eas['
    $null = Wait-Until 'an incomplete wildcard disables sending and bulk selection' {
        $send = Find-Element 'Send to 0 hosts' $button
        $bulk = Find-Element 'Select shown' $button
        (Find-Element 'Close the wildcard character set with ].') -and
            $send -and !$send.Current.IsEnabled -and $bulk -and !$bulk.Current.IsEnabled
    }
    Set-HostFilter "Eas?, West, !test@127.0.0.1:$($profileData.West.port)"
    $null = Wait-Until 'the wildcard exclusion removes West from the send scope' {
        (Find-Element "Focus pane $east" $button) -and !(Find-Element "Focus pane $west" $button)
    }
    (Command-Editor).SetValue('one')
    $alertedSend = 'Send with alert' + [char]0x2026
    $alertSearch = 'Search for the output alert'
    Click $alertedSend
    $null = Wait-Until 'an empty alert rule cannot send the command' {
        $confirm = Find-Element 'Send with alert' $button
        $confirm -and !$confirm.Current.IsEnabled
    }
    (Wait-CheckBox 'Regular expression' Off).Toggle()
    $null = Wait-CheckBox 'Regular expression' On
    (Command-Editor $alertSearch).SetValue('(')
    $null = Wait-Until 'an invalid pattern cannot send the command' {
        $confirm = Find-Element 'Send with alert' $button
        $confirm -and !$confirm.Current.IsEnabled -and (Find-Element 'unclosed group')
    }
    Click 'Cancel'
    $null = Wait-Until 'cancelling the alert review preserves the unsent draft' {
        (Find-Element 'Send to 1 host' $button) -and !(Find-Element "Submitted${dot}one")
    }
    Click $alertedSend
    $savedAlertRules = 'Saved alert rules' + [char]0x2026
    $ruleName = 'Fast success'
    $ruleQuery = '^east-accepted one$'
    (Wait-CheckBox 'Regular expression' Off).Toggle()
    $null = Wait-CheckBox 'Regular expression' On
    (Wait-CheckBox 'Match case' Off).Toggle()
    $null = Wait-CheckBox 'Match case' On
    (Command-Editor $alertSearch).SetValue($ruleQuery)
    Click $savedAlertRules
    (Command-Editor 'Output alert rule name').SetValue($ruleName)
    Click 'Save current rule'
    $null = Wait-Until 'the named rule appears after saving' { Find-Element "Use alert rule $ruleName" $button }
    $storedRule = Get-Content -LiteralPath (Join-Path $env:W11TERM_DATA_DIR 'output-alert-rules.json') -Raw | ConvertFrom-Json
    if ($storedRule.name -cne $ruleName -or $storedRule.query -cne $ruleQuery -or !$storedRule.regex -or !$storedRule.match_case) {
        throw 'The named rule lost its query or matching options'
    }
    Click 'Close saved rules'
    $chooseRecording = 'Choose recording file' + [char]0x2026
    (Wait-CheckBox 'Record full output' Off).Toggle()
    Click $chooseRecording
    $dialog = Wait-Until 'the combined send opens a recording destination' { Find-OwnedDialog 'Record SSH group output' }
    if (Find-Element "Submitted${dot}one") { throw 'The command was sent before the file was chosen' }
    $null = Wait-Until 'cancel the recorded alert' { [FixtureDialog]::ClickButton($dialog, 2) }
    $null = Wait-Until 'cancelling the file picker keeps the command unsent' {
        !(Find-OwnedDialog 'Record SSH group output') -and
            (Find-Element $alertedSend $button) -and !(Find-Element "Submitted${dot}one")
    }
    Click $alertedSend
    Click $savedAlertRules
    Click "Use alert rule $ruleName"
    $null = Wait-Until 'loading the saved rule restores the exact review query' {
        (Command-Editor $alertSearch).Current.Value -ceq $ruleQuery
    }
    $null = Wait-CheckBox 'Regular expression' On
    $null = Wait-CheckBox 'Match case' On
    (Wait-CheckBox 'Record full output' Off).Toggle()
    Click $chooseRecording
    $dialog = Wait-Until 'reopen the recording destination for the alert' { Find-OwnedDialog 'Record SSH group output' }
    $firstAlertRecording = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'first-alert-run.txt'
    $null = Wait-Until 'set the destination for the recorded alert' {
        [FixtureDialog]::SetFilename($dialog, $firstAlertRecording, 'ssh-group-output-1.txt')
    }
    $null = Wait-Until 'approve the recording destination and send' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the first output of the quick run triggers its pre-armed alert' {
        (Find-Element "Submitted${dot}one") -and (Find-Element 'Attention inbox, 1 unread' $button)
    }
    Click 'Attention inbox, 1 unread'
    Click "Unread${dot}SSH host group${dot}Run 1${dot}Matched output${dot}$eastName${dot}east-accepted one"
    $null = Wait-Until 'the pre-armed alert opens its original run' {
        (Find-Element "Submitted${dot}one") -and (Find-Element 'Attention inbox, 0 unread' $button)
    }
    Wait-RecordingStatus 'Saved live output to ' $firstAlertRecording
    $firstRecording = [IO.File]::ReadAllLines($firstAlertRecording)
    if ($firstRecording[0] -cne "Submitted${dot}one" -or
        "$eastName${dot}east-accepted one" -cnotin $firstRecording -or
        "$eastName${dot}Sent to SSH channel${dot}Exit 0" -cnotin $firstRecording -or
        $firstRecording[-1] -cne '[Group run settled]') {
        throw 'The recorded alert omitted the matching output or final result'
    }
    Outcome $eastName 'Sent to SSH channel'
    Pane-Status $east "Connected${dot}Ready"
    $showLastOutput = 'Show last command output'
    $openEastOutput = "Open last command for pane $east"
    $openWestOutput = "Open last command for pane $west"
    if (Find-Element $openEastOutput $button) { throw 'The roster output preview was on by default' }
    (Wait-CheckBox $showLastOutput Off).Toggle()
    $null = Wait-Until 'the checked preview shows the last completed output on East' {
        Find-Element "Pane $east${dot}Last command${dot}Exit 0${dot}east-accepted one"
    }
    Click $openEastOutput
    $null = Wait-Until 'the roster preview opens that exact command history entry' {
        Find-Element "Selected, one${dot}exit 0" $button
    }
    Run-Palette 'SSH host group'
    (Wait-CheckBox $showLastOutput On).Toggle()
    $null = Wait-Until 'turning off the preview hides the history shortcut' {
        !(Find-Element $openEastOutput $button)
    }
    Click 'Deselect shown'
    $null = Wait-CheckBox $eastName Off
    Set-HostFilter ''
    (Wait-CheckBox 'Use wildcards' On).Toggle()
    $null = Wait-CheckBox $westName On
    $null = Wait-CheckBox $eastName Off
    Click 'Select shown'
    $null = Wait-CheckBox $eastName On
    Submit 'both' 'Send to 2 hosts'
    Outcome $eastName 'Sent to SSH channel'
    Outcome $westName 'Sent to SSH channel'
    Outcome $eastName 'Exit 0'
    Outcome $westName 'Running'
    Click 'Notify when run finishes'
    Click 'Cancel run notification'
    Click 'Notify when run finishes'
    $null = Wait-Until 'the current run is armed for completion' {
        Find-Element 'Cancel run notification' $button
    }
    $runningPrefix = [regex]::Escape("$westName${dot}Running for ")
    $null = Wait-Until 'the held West result reports live elapsed time' {
        (Result-Timing $west) -match ($runningPrefix + '[1-9][0-9]* s$')
    }
    Expect-ResultOrder @($east, $west)
    Select-ComboValue 'Sort submission results' 'Longest shell time'
    Expect-ResultOrder @($west, $east)
    Select-ComboValue 'Sort submission results' 'Shortest shell time'
    Expect-ResultOrder @($east, $west)
    Select-ComboValue 'Sort submission results' 'Longest shell time'
    Expect-ResultOrder @($west, $east)
    $null = Wait-Until 'East output in the group' { Find-Element 'east-accepted both' }
    Set-RecipientSearch "WEST TEST@127.0.0.1:$($profileData.West.port)"
    Expect-ResultOrder @($west)
    Result-Progress 'Showing 1 of 2 recipients'
    Result-Progress "1 of 2 settled${dot}0 waiting${dot}1 running${dot}1 succeeded${dot}0 need attention"
    Set-ResultFilter 'Succeeded'
    Expect-ResultOrder @()
    Result-Progress 'Showing 0 of 2 recipients'
    Click 'Clear recipient search'
    Expect-ResultOrder @($east)
    Set-ResultFilter 'All results'
    Expect-ResultOrder @($west, $east)
    $liveButton = 'Live output' + [char]0x2026
    Click $liveButton
    $null = Wait-Until 'completed East output in the live feed' {
        (Find-Element 'SSH group live output') -and
            (Find-Element 'east-accepted both') -and (Find-Element "Focus output pane $east" $button)
    }
    $recordOutput = 'Record output' + [char]0x2026
    $logFile = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'live-both.txt'
    Click $recordOutput
    $dialog = Wait-Until 'owned live output save dialog' { Find-OwnedDialog 'Record SSH group output' }
    $null = Wait-Until 'cancel the output recording' { [FixtureDialog]::ClickButton($dialog, 2) }
    $null = Wait-Until 'recording dialog closes after cancel' { !(Find-OwnedDialog 'Record SSH group output') }
    if ([IO.File]::Exists($logFile)) { throw 'Cancelling created an output recording' }
    Click $recordOutput
    $dialog = Wait-Until 'owned output recording dialog reopens' { Find-OwnedDialog 'Record SSH group output' }
    $null = Wait-Until 'enter the exact output recording destination' {
        [FixtureDialog]::SetFilename($dialog, $logFile, 'ssh-group-output-2.txt')
    }
    $null = Wait-Until 'accept the output recording file' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'output recording picker is dismissed after Save' { !(Find-OwnedDialog 'Record SSH group output') }
    Wait-RecordingStatus 'Recording live output to ' $logFile
    $logReader = Open-PeerTraceReader $logFile
    [IO.File]::WriteAllText((Join-Path $State 'release-west-preview'), '')
    $null = Wait-Until 'running West output appears above the earlier East output' {
        $westLine = Find-Element 'west live first'
        $eastLine = Find-Element 'east-accepted both'
        $westLine -and $eastLine -and $westLine.Current.BoundingRectangle.Top -lt $eastLine.Current.BoundingRectangle.Top
    }
    $outputSearch = 'Find in live output'
    $outputHosts = 'Filter live output by host'
    (Command-Editor $outputSearch).SetValue('EAST-ACCEPTED')
    $null = Wait-Until 'literal live output search finds East without regard to case' {
        (Find-Element "Newest first${dot}1 of 2 retained lines") -and
            (Find-Element 'east-accepted both') -and !(Find-Element 'west live first')
    }
    Select-ComboValue $outputHosts $westName
    $null = Wait-Until 'the text search also respects the selected output host' {
        $copy = Find-Element 'Copy shown output' $button
        $copy -and !$copy.Current.IsEnabled -and
            (Find-Element "Newest first${dot}0 of 1 retained lines") -and !(Find-Element 'east-accepted both')
    }
    $outputRegex = 'Regular expression in live output'
    $outputCase = 'Match case in live output'
    (Wait-CheckBox $outputRegex Off).Toggle()
    $null = Wait-CheckBox $outputRegex On
    (Command-Editor $outputSearch).SetValue('(')
    $null = Wait-Until 'an invalid pattern shows a diagnostic and disables both exports' {
        $copy = Find-Element 'Copy shown output' $button
        $save = Find-Element ('Save shown output' + [char]0x2026) $button
        $copy -and !$copy.Current.IsEnabled -and $save -and !$save.Current.IsEnabled -and
            (Find-Element "Live output search${dot}unclosed group")
    }
    Click 'Pause output'
    $null = Wait-Until 'output is paused' { Find-Element 'Resume output' $button }
    [IO.File]::WriteAllText((Join-Path $State 'release-west-paused'), '')
    $null = Control @('wait-for-text','--pane',"$west",'--timeout','15','west while paused')
    $null = Wait-Until 'the recorder keeps writing while the display is paused' {
        [Text.Encoding]::UTF8.GetString((Read-PeerTrace $logReader)).Contains("$westName$($dot)west while paused")
    }
    (Command-Editor $outputSearch).SetValue('^WEST WHILE (PAUSED|AWAY)$')
    $until = [DateTime]::UtcNow.AddMilliseconds(650)
    do {
        if ((Find-Element 'west while paused') -or
            !(Find-Element "Paused${dot}newest first${dot}0 of 1 retained lines")) {
            throw 'Filtered output changed while paused'
        }
        Start-Sleep -Milliseconds 120
    } while ([DateTime]::UtcNow -lt $until)
    Click 'Resume output'
    $null = Wait-Until 'matching output received while paused is available on resume' {
        (Find-Element "Newest first${dot}1 of 2 retained lines") -and
            (Find-Element 'west while paused') -and !(Find-Element 'west live first')
    }
    (Wait-CheckBox $outputCase Off).Toggle()
    $null = Wait-Until 'matching case excludes the lowercase live output' {
        (Find-Element "Newest first${dot}0 of 2 retained lines") -and !(Find-Element 'west while paused')
    }
    (Wait-CheckBox $outputCase On).Toggle()
    $null = Wait-Until 'disabling match case restores the live output match' {
        (Find-Element "Newest first${dot}1 of 2 retained lines") -and (Find-Element 'west while paused')
    }
    Click "Focus output pane $west"
    Wait-Focus $west
    $outputWait = @('wait-ssh-group-output','--window',"$groupWindow",'--tab',"$groupTab",'--run','2','--pane',"$west")
    $timedOut = & $Cli --pid $app.Id @outputWait --timeout 1 'east-accepted both' | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $timedOut.ok -or $timedOut.error -notlike '*timed out before a match*') {
        throw 'The output wait did not respect its pane scope or timeout'
    }
    $earlierMatch = Control @('wait-ssh-group-output','--window',"$groupWindow",'--tab',"$groupTab",'--run','2','EAST-ACCEPTED')
    if ($earlierMatch.line.pane_id -ne $east -or $earlierMatch.line.text -cne 'east-accepted both') {
        throw 'The output wait missed an existing match in retained output'
    }
    $westPrior = Control ($outputWait + @('--regex','^west while (paused|away)$'))
    if ($westPrior.line.text -cne 'west while paused') { throw 'The first West output marker was missing' }
    $westReading = Control @('get-ssh-group-output','--window',"$groupWindow",'--tab',"$groupTab",
        '--run','2','--pane',"$west",'--after-line',"$($westPrior.line.line_id)")
    if ($westReading.settled -or $westReading.gap -or $westReading.has_more -or
        $westReading.next_after_line -lt $westPrior.line.line_id) {
        throw 'Reading the live feed did not provide a cursor for the next line'
    }
    $quietFollow = & $Cli --pid $app.Id follow-ssh-group-output --window $groupWindow --tab $groupTab --run 2 --pane $west --after-line $westReading.next_after_line --timeout 1 | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $quietFollow.ok -or $quietFollow.error -notlike '*timed out before settlement*') {
        throw 'A quiet output stream did not return a terminal timeout record'
    }
    $future = & $Cli --pid $app.Id @outputWait --after-line 999999 'west while' | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $future.ok -or $future.error -notlike '*ahead of the latest line*') {
        throw 'An output cursor ahead of the run did not return an explicit error'
    }
    $settledMiss = & $Cli --pid $app.Id wait-ssh-group-output --window $groupWindow --tab $groupTab --run 1 unreachable-match | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $settledMiss.ok -or $settledMiss.error -notlike '*settled without a matching output line*') {
        throw 'A settled run with no match did not return a distinct error'
    }
    $foregroundBeforeOutputWait = [FixtureMessages]::GetForegroundWindow()
    $waitPath = Join-Path $State 'wait-group-output.json'
    $outputJob = Start-Process -FilePath $Cli -ArgumentList (@('--pid',"$($app.Id)") + $outputWait +
        @('--after-line',"$($westReading.next_after_line)",'--regex','--match-case','--timeout','15','^west\x20while\x20(?:paused|away)$')) -NoNewWindow -PassThru -RedirectStandardOutput $waitPath
    if ($outputJob.WaitForExit(250)) { throw 'The output wait ended before West produced its matching line' }
    [IO.File]::WriteAllText((Join-Path $State 'release-west-away'), '')
    if (!$outputJob.WaitForExit(10000)) { throw 'The output wait did not receive the new line' }
    $outputReply = [IO.File]::ReadAllText($waitPath) | ConvertFrom-Json
    $match = $outputReply.result.line
    if (!$outputReply.ok -or $outputReply.result.window_id -ne $groupWindow -or
        $outputReply.result.tab_id -ne $groupTab -or $outputReply.result.run_id -ne 2 -or
        $match.pane_id -ne $west -or $match.label -cne 'West' -or
        $match.address -cne "test@127.0.0.1:$($profileData.West.port)" -or
        $match.text -cne 'west while away' -or $match.shortened -ne $false -or
        $match.line_id -le $westReading.next_after_line -or $match.elapsed_ms -lt $westPrior.line.elapsed_ms -or
        [FixtureMessages]::GetForegroundWindow() -ne $foregroundBeforeOutputWait) {
        throw "The output wait returned the wrong match or changed focus: $($outputReply.error)"
    }
    Wait-Focus $west
    $null = Wait-Until 'the recorder keeps writing while the terminal tab is focused' {
        [Text.Encoding]::UTF8.GetString((Read-PeerTrace $logReader)).Contains("$westName$($dot)west while away")
    }
    Run-Palette 'SSH host group'
    $null = Wait-Until 'the same output view returns after focusing its source pane' {
        (Find-Element 'SSH group live output') -and (Find-Element 'west while paused') -and
            (Find-Element 'west while away') -and !(Find-Element 'east-accepted both')
    }
    Click 'Copy shown output'
    $excerpt = "Submitted${dot}both`r`n$westName${dot}west while away`r`n$westName${dot}west while paused`r`n"
    $null = Wait-Until 'clipboard contains just the two matching lines in newest-first order' {
        (Read-ClipboardFixture) -ceq $excerpt
    }
    $null = Wait-Until 'confirm the excerpt was copied' { Find-Element 'Copied shown output' }
    $excerptPath = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'ssh-group-output-excerpt-2.txt'
    Click ('Save shown output' + [char]0x2026)
    $dialog = Wait-Until 'owned shown output save dialog' { Find-OwnedDialog 'Save shown SSH group output' }
    $null = Wait-Until 'save the shown output' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the saved excerpt matches the clipboard' {
        [IO.File]::Exists($excerptPath) -and
            [IO.File]::ReadAllText($excerptPath, [Text.UTF8Encoding]::new($false, $true)) -ceq $excerpt
    }
    $outputOldest = 'Oldest first in live output'
    $outputElapsed = 'Show elapsed time in live output'
    (Wait-CheckBox $outputOldest Off).Toggle()
    (Wait-CheckBox $outputElapsed Off).Toggle()
    $null = Wait-Until 'oldest-first view places the earlier row above the later row' {
        $earlier = Find-Element 'west while paused'
        $later = Find-Element 'west while away'
        $earlier -and $later -and
            $earlier.Current.BoundingRectangle.Top -lt $later.Current.BoundingRectangle.Top
    }
    $null = Wait-CheckBox $outputElapsed On
    Click 'Copy shown output'
    $stamp = '\+\d+\.\d{3} s'
    $timedPattern = '^' + [regex]::Escape("Submitted${dot}both`r`n") +
        $stamp + [regex]::Escape("${dot}$westName${dot}west while paused`r`n") +
        $stamp + [regex]::Escape("${dot}$westName${dot}west while away`r`n") + '$'
    $timedExcerpt = Wait-Until 'copy includes the elapsed times in oldest-first order' {
        $text = Read-ClipboardFixture
        if ($text -cmatch $timedPattern) { $text }
    }
    $timelinePath = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'timeline.txt'
    Click ('Save shown output' + [char]0x2026)
    $dialog = Wait-Until 'owned timeline save dialog' { Find-OwnedDialog 'Save shown SSH group output' }
    $null = Wait-Until 'enter the timeline file name' {
        [FixtureDialog]::SetFilename($dialog, $timelinePath, 'ssh-group-output-excerpt-2.txt')
    }
    $null = Wait-Until 'save the timeline' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the saved timeline matches the clipboard' {
        [IO.File]::Exists($timelinePath) -and [IO.File]::ReadAllText($timelinePath) -ceq $timedExcerpt
    }
    $outputContext = 'Show surrounding lines'
    Select-ComboValue $outputHosts 'All output hosts'
    (Wait-CheckBox $outputContext Off).Toggle()
    $null = Wait-Until 'context includes West neighbors while the combined feed excludes unrelated East lines' {
        (Find-Element 'west live first') -and (Find-Element 'Context') -and
            (Find-Element 'west while paused') -and !(Find-Element 'east-accepted both')
    }
    Click 'Copy shown output'
    $null = Wait-Until 'copy includes the surrounding West lines' {
        $text = Read-ClipboardFixture
        $text -and $text.Contains("$westName${dot}west live first") -and
            $text.Contains("$westName${dot}west while away") -and !$text.Contains('east-accepted both')
    }
    Select-ComboValue $outputHosts $westName
    $outputRun = 'Live output run'
    $outputPattern = '^WEST WHILE (PAUSED|AWAY)$'
    Click 'Pause output'
    $null = Wait-Until 'the current run is paused before switching' { Find-Element 'Resume output' $button }
    Select-ComboValue $outputRun "Run 1${dot}one"
    $null = Wait-Until 'the earlier run keeps the absent host scoped and starts live' {
        $copy = Find-Element 'Copy shown output' $button
        $copy -and !$copy.Current.IsEnabled -and (Find-Element 'Pause output' $button) -and
            (Find-Element 'The selected host did not receive this run.') -and
            (Command-Editor $outputSearch).Current.Value -ceq $outputPattern -and
            !(Find-Element 'west while paused')
    }
    Select-ComboValue $outputRun "Run 2${dot}both"
    $null = Wait-CheckBox $outputRegex On
    $null = Wait-CheckBox $outputContext On
    $null = Wait-CheckBox $outputOldest On
    $null = Wait-CheckBox $outputElapsed On
    $null = Wait-Until 'returning restores the same scoped lines without the prior pause' {
        (Find-Element 'Pause output' $button) -and (Find-Element 'west while paused') -and
            (Find-Element 'west while away') -and (Find-Element 'west live first') -and
            (Command-Editor $outputSearch).Current.Value -ceq $outputPattern
    }
    Wait-RecordingStatus 'Recording live output to ' $logFile
    Click 'Clear output filters'
    $null = Wait-Until 'clearing the filters restores all retained output' {
        (Find-Element 'east-accepted both') -and (Find-Element 'west live first') -and
            (Find-Element 'Oldest first')
    }
    $null = Wait-CheckBox $outputRegex Off
    $null = Wait-CheckBox $outputCase Off
    $null = Wait-CheckBox $outputContext Off
    (Wait-CheckBox $outputOldest On).Toggle()
    (Wait-CheckBox $outputElapsed On).Toggle()
    $null = Wait-CheckBox $outputOldest Off
    $null = Wait-CheckBox $outputElapsed Off
    Click 'Stop recording'
    $firstRecordedPath = Wait-RecordingStatus 'Saved live output to ' $logFile -PassThru
    $logReader.Dispose()
    $logReader = $null
    $recorded = [IO.File]::ReadAllText($logFile)
    $expectedLines = @(
        "Submitted$($dot)both", "Dispatch$($dot)unlimited parallel$($dot)no delay",
        "$eastName$($dot)east-accepted both",
        "$westName$($dot)west live first", "$westName$($dot)west while paused",
        "$westName$($dot)west while away", '[Host outcomes]',
        "$eastName$($dot)Sent to SSH channel$($dot)Exit 0",
        "$westName$($dot)Sent to SSH channel$($dot)Running", '[Recording stopped]'
    )
    # Refocusing a terminal can reflow it, which adds a host-specific gap notice.
    $gap = 'A portion of this host' + [char]0x2019 + 's output was skipped.'
    $withNoGaps = $recorded.Replace("$eastName$($dot)$gap`r`n", '').Replace("$westName$($dot)$gap`r`n", '')
    if ($withNoGaps -cne (($expectedLines -join "`r`n") + "`r`n")) {
        throw "The output log lost, repeated, or reordered a line: $recorded"
    }
    Select-ComboValue $outputRun "Run 1${dot}one"
    Click 'Back to results'
    $null = Wait-Until 'Back opens the selected run results' {
        (Find-Element "Submitted${dot}one") -and !(Find-Element 'SSH group live output') -and
            !(Find-Element "Focus result pane $west" $button)
    }
    Click 'Next submission'
    Outcome $westName 'Running'
    Set-ResultFilter 'Running'
    $null = Wait-Until 'show the running result only' {
        (Find-Element "Focus result pane $west" $button) -and !(Find-Element "Focus result pane $east" $button)
    }
    Result-Progress "1 of 2 settled${dot}0 waiting${dot}1 running${dot}1 succeeded${dot}0 need attention"
    $readyButton = 'Select only ready shown'
    $null = Wait-Until 'only one selected host is ready' { Find-Element '1 of 2 selected hosts ready' }
    Click $readyButton
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName Off
    $null = Wait-Until 'the ready selection matches the send scope' {
        (Find-Element '1 of 1 selected host ready') -and (Find-Element 'Send to 1 host' $button)
    }
    Outcome $westName 'Running'
    Click 'Select shown'
    Set-HostFilter 'west'
    $null = Wait-Until 'selected East stays hidden' {
        (Find-Element '1 selected host is hidden by the filter.') -and
            (Find-Element '0 of 1 selected host ready')
    }
    Click $readyButton
    $null = Wait-CheckBox $westName Off
    Set-HostFilter ''
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName Off
    Click 'Select shown'
    Submit 'while-slow' 'Send to 2 hosts'
    Outcome $eastName 'Exit 0'
    Outcome $westName "Not sent${dot}Waiting for a fresh prompt"
    $null = Combo-Popup 'Sort submission results' 'Longest shell time'
    Expect-ResultOrder @($east, $west)
    Result-Progress "2 of 2 settled${dot}0 waiting${dot}0 running${dot}1 succeeded${dot}1 needs attention"
    Set-HostFilter 'east'
    $null = Wait-Until 'West has left the roster' { !(Find-Element "Focus pane $west" $button) }
    $hostList = "test@127.0.0.1:$($profileData.East.port)`n"
    $hostListDestination = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'ssh-hosts.txt'
    $saveHostList = 'Save host list' + [char]0x2026
    Click ('Export host list' + [char]0x2026)
    $null = Wait-Until 'the checked host list preview' { Find-Element 'SSH host list preview' }
    Click 'Copy host list'
    $null = Wait-Until 'only the shown checked East address is copied' {
        (Read-ClipboardFixture) -ceq $hostList
    }
    $null = Wait-Until 'the address count is confirmed' { Find-Element 'Copied 1 SSH address' }
    Click $saveHostList
    $dialog = Wait-Until 'owned host list save dialog' { Find-OwnedDialog 'Save SSH host list' }
    $null = Wait-Until 'cancel the host list export' { [FixtureDialog]::ClickButton($dialog, 2) }
    $null = Wait-Until 'host list dialog closes after cancel' { !(Find-OwnedDialog 'Save SSH host list') }
    if ([IO.File]::Exists($hostListDestination)) { throw 'Cancelling published a host list' }
    Click $saveHostList
    $dialog = Wait-Until 'host list save dialog reopens' { Find-OwnedDialog 'Save SSH host list' }
    $null = Wait-Until 'save the portable host list' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the saved list contains the same portable address' {
        [IO.File]::Exists($hostListDestination) -and
            [IO.File]::ReadAllText($hostListDestination, [Text.UTF8Encoding]::new($false, $true)) -ceq $hostList
    }
    Click 'Back to SSH group'
    $null = Wait-CheckBox $eastName On
    $null = Wait-Until 'West remains hidden after host list export' { !(Find-Element "Focus pane $west" $button) }
    $null = Wait-Until 'the full East output is retained' { Find-Element "View output for pane $east" $button }
    Set-RecipientSearch 'east'
    Expect-ResultOrder @($east)
    Click 'Copy results'
    $null = Wait-Until 'the current report contains full output and both host outcomes' {
        $report = Read-ClipboardFixture
        $report -and $report.StartsWith("Submitted${dot}while-slow") -and
            $report.Contains("Dispatch${dot}unlimited parallel${dot}no delay") -and
            $report.Contains("$eastName${dot}Sent to SSH channel") -and
            $report.Contains("  Exit 0") -and $report.Contains("east-report-tail") -and
            $report.Contains("$westName${dot}Not sent${dot}Waiting for a fresh prompt")
    }
    $null = Wait-Until 'copy confirmation' { Find-Element 'Copied results' }
    Click 'Clear recipient search'
    $saveResults = 'Save results' + [char]0x2026
    $destination = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'ssh-group-submission-3.txt'
    Click $saveResults
    $dialog = Wait-Until 'owned group results save dialog' { Find-OwnedDialog 'Save SSH group results as text' }
    $null = Wait-Until 'cancel the group results export' { [FixtureDialog]::ClickButton($dialog, 2) }
    $null = Wait-Until 'group results dialog closes after cancel' { !(Find-OwnedDialog 'Save SSH group results as text') }
    if ([IO.File]::Exists($destination)) { throw 'Cancelling the save published a report' }
    Click $saveResults
    $dialog = Wait-Until 'owned group results save dialog reopens' { Find-OwnedDialog 'Save SSH group results as text' }
    $null = Wait-Until 'save the group results report' { [FixtureDialog]::ClickButton($dialog, 1) }
    $savedStatus = Wait-Until 'report was saved with its submission name' {
        if ([IO.File]::Exists($destination)) {
            $prefix = 'Saved SSH group results to '
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            $buttons = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $button)
            foreach ($element in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $buttons)) {
                $name = $element.Current.Name
                if ($name.StartsWith($prefix, [StringComparison]::Ordinal) -and
                    [FixtureDialog]::SameFile($destination, $name.Substring($prefix.Length))) {
                    return $name
                }
            }
        }
    }
    $exported = [IO.File]::ReadAllText($destination, [Text.UTF8Encoding]::new($false, $true))
    if ($exported -cne (Read-ClipboardFixture)) { throw 'The saved report omitted retained output or host status' }
    Click $savedStatus
    Click "Compare earlier run for pane $east"
    $null = Wait-Until 'the cross-run comparison opens for East' { Find-Element 'SSH output across runs' }
    Select-ComboValue 'Earlier group run' "Run 2${dot}both"
    Click 'Compare output'
    $null = Wait-Until 'the diff compares complete captures from the same pane' {
        (Find-Element "Run 2${dot}both${dot}Exit 0") -and
            (Find-Element "Run 3${dot}while-slow${dot}Exit 0") -and
            (Find-Element "Removed line 1${dot}east-accepted both") -and
            (Find-Element "Added line 1${dot}east-accepted while-slow")
    }
    Click 'Back to results'
    Click ('Compare runs' + [char]0x2026)
    $null = Wait-Until 'group run comparison opens' { Find-Element 'SSH group run comparison' }
    Select-ComboValue 'Earlier group run' "Run 1${dot}one"
    Click 'Compare runs'
    $null = Wait-Until 'the comparison separates hosts only in the current run' {
        (Find-Element "1 changed${dot}0 unchanged${dot}0 earlier only${dot}1 current only${dot}0 unavailable") -and
            (Find-Element "$westName${dot}Current run only")
    }
    Select-ComboValue 'Earlier group run' "Run 2${dot}both"
    Click 'Compare runs'
    $overviewSummary = "1 changed${dot}0 unchanged${dot}0 earlier only${dot}0 current only${dot}1 unavailable"
    $null = Wait-Until 'the comparison separates an unavailable shared host from changed output' {
        (Find-Element $overviewSummary) -and (Find-Element "$eastName${dot}Output changed") -and
            (Find-Element "$westName${dot}Comparison unavailable") -and
            !(Find-Element "View run differences for pane $west" $button)
    }
    $comparisonField = 'Find compared hosts'
    $valuePattern = [System.Windows.Automation.ValuePattern]::Pattern
    $westSearch = "WEST $($profileData.West.port)"
    (Find-Element $comparisonField $edit).GetCurrentPattern($valuePattern).SetValue($westSearch)
    $null = Wait-Until 'the comparison search matches the host label and endpoint' {
        (Find-Element 'Showing 1 of 2 hosts') -and (Find-Element $overviewSummary) -and
            (Find-Element "$westName${dot}Comparison unavailable") -and
            !(Find-Element "$eastName${dot}Output changed")
    }
    Click 'Copy comparison'
    $null = Wait-Until 'the clipboard includes both run labels and all host classifications' {
        $report = Read-ClipboardFixture
        (Find-Element 'Copied comparison') -and $report -and
            $report.Contains("Earlier${dot}Run 2${dot}both") -and
            $report.Contains("Current${dot}Run 3${dot}while-slow") -and
            $report.Contains($overviewSummary) -and
            $report.Contains("$eastName${dot}Output changed") -and
            $report.Contains("$westName${dot}Comparison unavailable") -and
            !$report.Contains('east-report-tail')
    }
    Click ('Save comparison as JSON' + [char]0x2026)
    $dialog = Wait-Until 'owned group run comparison save dialog' { Find-OwnedDialog 'Save SSH group run comparison as JSON' }
    $null = Wait-Until 'save the group run comparison JSON' { [FixtureDialog]::ClickButton($dialog, 1) }
    $comparisonPath = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'ssh-group-run-comparison-2-to-3.json'
    $null = Wait-Until 'the group run comparison JSON is published' { [IO.File]::Exists($comparisonPath) }
    $comparisonText = [IO.File]::ReadAllText($comparisonPath, [Text.UTF8Encoding]::new($false, $true))
    $comparison = $comparisonText | ConvertFrom-Json
    $eastComparison = $comparison.hosts | Where-Object pane_id -EQ $east
    $westComparison = $comparison.hosts | Where-Object pane_id -EQ $west
    if ($comparison.schema_version -ne 1 -or $comparison.earlier.run_id -ne 2 -or
        $comparison.earlier.label -cne 'both' -or $comparison.current.run_id -ne 3 -or
        $comparison.current.label -cne 'while-slow' -or $comparison.hosts.Count -ne 2 -or
        $comparison.counts.changed -ne 1 -or $comparison.counts.unavailable -ne 1 -or
        $eastComparison.name -cne 'East' -or $eastComparison.address -cne "test@127.0.0.1:$($profileData.East.port)" -or
        $eastComparison.change -cne 'output_changed' -or
        $eastComparison.earlier_exit_code -ne 0 -or $eastComparison.current_exit_code -ne 0 -or
        $westComparison.name -cne 'West' -or $westComparison.address -cne "test@127.0.0.1:$($profileData.West.port)" -or
        $westComparison.change -cne 'unavailable' -or $westComparison.detail -notlike '*Run 2*' -or
        $null -ne $westComparison.earlier_exit_code -or $null -ne $westComparison.current_exit_code -or
        $comparisonText.Contains('east-report-tail')) {
        throw 'The JSON comparison did not preserve the run and host classifications'
    }
    Select-ComboValue 'Filter compared hosts' 'Output changed'
    $null = Wait-Until 'classification and text filters are combined' {
        (Find-Element 'Showing 0 of 2 hosts') -and (Find-Element 'No hosts match these filters')
    }
    (Find-Element $comparisonField $edit).GetCurrentPattern($valuePattern).SetValue('EAST')
    $null = Wait-Until 'the filtered comparison offers only the matching host diff' {
        (Find-Element "View run differences for pane $east" $button) -and (Find-Element 'Showing 1 of 2 hosts') -and
            !(Find-Element "$westName${dot}Comparison unavailable")
    }
    Click "View run differences for pane $east"
    $null = Wait-Until 'the group overview opens the same pane diff' {
        (Find-Element 'SSH output across runs') -and (Find-Element 'Back to run overview' $button) -and
            (Find-Element "Added line 1${dot}east-accepted while-slow")
    }
    Click ('Save full run differences' + [char]0x2026)
    $dialog = Wait-Until 'owned run differences save dialog' { Find-OwnedDialog 'Save SSH group differences' }
    $null = Wait-Until 'save the cross-run differences' { [FixtureDialog]::ClickButton($dialog, 1) }
    $runDiffPath = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR "ssh-group-run-diff-2-to-3-pane-$east.txt"
    $null = Wait-Until 'the export includes the entire later output and both run headers' {
        if (![IO.File]::Exists($runDiffPath)) { return }
        $diff = [IO.File]::ReadAllText($runDiffPath)
        $diff.Contains("Run 2${dot}both${dot}Exit 0") -and $diff.Contains("Run 3${dot}while-slow${dot}Exit 0") -and
            $diff.Contains('east-report-tail') -and ([regex]::Matches($diff, 'east-report-line')).Count -eq 35
    }
    Click 'Back to run overview'
    $null = Wait-Until 'the overview preserves its baseline and host filters' {
        $search = Find-Element $comparisonField $edit
        $search -and $search.GetCurrentPattern($valuePattern).Current.Value -ceq 'EAST' -and
            (Find-Element 'SSH group run comparison') -and (Find-Element $overviewSummary) -and
            (Find-Element 'Showing 1 of 2 hosts') -and !(Find-Element "$westName${dot}Comparison unavailable")
    }
    Click 'Back to results'
    $null = Wait-Until 'the cross-run comparison returns to the current submission' {
        (Find-Element "Submitted${dot}while-slow") -and !(Find-Element 'SSH output across runs')
    }
    Set-ResultFilter 'Succeeded'
    $null = Wait-Until 'the rejected result is hidden in the filtered view' {
        (Find-Element "Focus result pane $east" $button) -and !(Find-Element "Focus result pane $west" $button)
    }
    $jsonPath = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'group results.json'
    Click ('Save results as JSON' + [char]0x2026)
    $dialog = Wait-Until 'owned group JSON save dialog' { Find-OwnedDialog 'Save SSH group results as JSON' }
    $null = Wait-Until 'enter the JSON destination' {
        [FixtureDialog]::SetFilename($dialog, $jsonPath, 'ssh-group-submission-3.json')
    }
    $null = Wait-Until 'save the group JSON report' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the JSON report is published' { [IO.File]::Exists($jsonPath) }
    $json = [IO.File]::ReadAllText($jsonPath, [Text.UTF8Encoding]::new($false, $true)) | ConvertFrom-Json
    if ($json.schema_version -ne 1 -or $json.submission_id -ne 3 -or $json.kind -cne 'command' -or
        $json.command -cne 'while-slow' -or $json.recipients.Count -ne 2) {
        throw 'The JSON report did not identify the entire viewed submission'
    }
    $policy = $json.dispatch_policy
    if (!$policy -or $null -ne $policy.parallel_limit -or $policy.interval_ms -ne 0 -or
        $false -ne $policy.stop_on_failure -or $null -ne $policy.canary) {
        throw 'The JSON report did not preserve the effective dispatch policy'
    }
    $eastJson, $westJson = $json.recipients
    if ($eastJson.pane_id -ne $east -or $eastJson.profile -cne 'East' -or
        $eastJson.address -cne "test@127.0.0.1:$($profileData.East.port)" -or
        $eastJson.delivery.state -cne 'sent' -or $eastJson.execution.state -cne 'completed' -or
        $eastJson.execution.exit_code -ne 0 -or $null -eq $eastJson.execution.duration_ms -or
        $eastJson.execution.duration_ms -lt 0 -or $eastJson.output.state -cne 'complete' -or
        !$eastJson.output.text.Contains('east-report-tail') -or
        ([regex]::Matches($eastJson.output.text, 'east-report-line')).Count -ne 35) {
        throw 'The JSON report lost the completed host identity, exit status, or full retained output'
    }
    if ($westJson.pane_id -ne $west -or $westJson.delivery.state -cne 'rejected' -or
        $westJson.delivery.reason -cne 'Waiting for a fresh prompt' -or
        $null -ne $westJson.execution -or $null -ne $westJson.output) {
        throw 'The JSON report omitted or misclassified the hidden rejected host'
    }
    if ($json.PSObject.Properties.Name -contains 'total_recipient_count') {
        throw 'The full report was marked as a subset'
    }
    Set-RecipientSearch 'west'
    $null = Wait-Until 'an empty result filter has no shown export' {
        (Find-Element 'Showing 0 of 2 recipients') -and !(Find-Element 'Copy shown results' $button)
    }
    Click 'Clear recipient search'
    Click 'Copy shown results'
    $shownText = Wait-Until 'clipboard holds only the successful result' {
        $report = Read-ClipboardFixture
        if ($report -and $report.Contains("Shown results${dot}1 of 2 recipients") -and
            $report.Contains("$eastName${dot}Sent to SSH channel") -and
            $report.Contains('east-report-tail') -and !$report.Contains($westName)) { $report }
    }
    Click ('Save shown results' + [char]0x2026)
    $dialog = Wait-Until 'shown group text Save dialog' { Find-OwnedDialog 'Save SSH group results as text' }
    $null = Wait-Until 'save the shown group text report' { [FixtureDialog]::ClickButton($dialog, 1) }
    $shownTextPath = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'ssh-group-submission-3-shown.txt'
    $null = Wait-Until 'the saved text matches the shown clipboard report' {
        [IO.File]::Exists($shownTextPath) -and
            [IO.File]::ReadAllText($shownTextPath, [Text.UTF8Encoding]::new($false, $true)) -ceq $shownText
    }
    Click ('Save shown results as JSON' + [char]0x2026)
    $dialog = Wait-Until 'shown group JSON Save dialog' { Find-OwnedDialog 'Save SSH group results as JSON' }
    $null = Wait-Until 'save the shown group JSON report' { [FixtureDialog]::ClickButton($dialog, 1) }
    $shownJsonPath = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'ssh-group-submission-3-shown.json'
    $null = Wait-Until 'the shown JSON report is published' { [IO.File]::Exists($shownJsonPath) }
    $shownJson = [IO.File]::ReadAllText($shownJsonPath, [Text.UTF8Encoding]::new($false, $true)) | ConvertFrom-Json
    if ($shownJson.total_recipient_count -ne 2 -or $shownJson.recipients.Count -ne 1 -or
        $shownJson.recipients[0].pane_id -ne $east -or
        $shownJson.recipients[0].output.text -cne $eastJson.output.text) {
        throw 'The filtered JSON report lost the original recipient count or the shown result'
    }
    Set-ResultFilter 'All results'
    $null = Wait-Until 'the hidden West result is outside the retry scope' {
        $entry = Find-Element 'Prepare retry' $button
        $entry -and !$entry.Current.IsEnabled
    }
    Set-HostFilter ''
    (Command-Editor).SetValue('draft before withheld retry')
    Click 'Prepare retry'
    Wait-CommandDraft 'while-slow'
    $null = Wait-CheckBox $eastName Off
    $null = Wait-CheckBox $westName On
    $null = Wait-Until 'the withheld host still needs a fresh prompt before sending' {
        $entry = Find-Element 'Send to 1 host' $button
        $entry -and !$entry.Current.IsEnabled
    }
    Click 'Restore draft'
    Wait-CommandDraft 'draft before withheld retry'
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On
    $findRun = 'Find run' + [char]0x2026
    $runSearch = 'Search group runs'
    Click $findRun
    (Wait-PickerEditor $runSearch 'the run picker button').GetCurrentPattern(
        [System.Windows.Automation.ValuePattern]::Pattern).SetValue('not-a-submission')
    $null = Wait-Until 'run search reports an empty result' { Find-Element 'No matching group runs' }
    Press-Key 27
    $null = Wait-Until 'Escape returns to the same submission' {
        !(Find-Element $runSearch $edit) -and (Find-Element "Submitted${dot}while-slow")
    }
    if ((Command-Editor).Current.Value -cne 'draft before withheld retry') { throw 'Closing the run picker changed the draft' }
    Click $findRun
    (Wait-PickerEditor $runSearch 'the reopened run picker').GetCurrentPattern(
        [System.Windows.Automation.ValuePattern]::Pattern).SetValue('BOTH')
    $null = Wait-Until 'only the matching run is offered' {
        (Find-Element "Run 2${dot}both" $button) -and !(Find-Element "Run 3${dot}while-slow" $button)
    }
    Press-Key 13
    $null = Wait-Until 'run search jumps to the earlier submission' { Find-Element "Submitted${dot}both" }
    if ((Command-Editor).Current.Value -cne 'draft before withheld retry') { throw 'Choosing a run changed the draft' }
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On
    Outcome $westName 'Running'
    $compareButton = 'Compare results' + [char]0x2026
    Click $compareButton
    $null = Wait-Until 'the comparison separates the still-running host' {
        (Find-Element 'SSH group result comparison') -and
        (Find-Element "1 distinct result${dot}1 host compared${dot}1 pending or unavailable") -and
        (Find-Element "$westName${dot}Running")
    }
    Click "Focus compared pane $east"
    Wait-Focus $east
    [IO.File]::WriteAllText((Join-Path $State 'release-west-result'), '')
    $null = Control @('wait-for-text','--pane',"$west",'--timeout','15','west-accepted both')
    $null = Wait-Until 'an inactive group posts one completion notice' {
        Find-Element 'Attention inbox, 1 unread' $button
    }
    Wait-Focus $east
    Run-Palette 'SSH host group'
    if ([IO.File]::ReadAllText($logFile) -cne $recorded) { throw 'Output was added after recording stopped' }
    if (!(Find-Element "$westName${dot}Running")) { throw 'The comparison changed before refresh' }
    Click 'Refresh comparison'
    $null = Wait-Until 'refresh exposes the two distinct finished results' {
        (Find-Element "2 distinct results${dot}2 hosts compared${dot}0 pending or unavailable") -and
        (Find-Element "First different line from largest group${dot}1") -and
        (Find-Element "View comparison output for pane $west" $button)
    }
    Click "View differences for pane $west"
    $null = Wait-Until 'the difference shows changes with the original host labels and exit codes' {
        (Find-Element 'SSH group output differences') -and
            (Find-Element "Reference${dot}$eastName${dot}Exit 0") -and
            (Find-Element "Selected${dot}$westName${dot}Exit 17") -and
            (Find-Element "Removed line 1${dot}east-accepted both") -and
            (Find-Element "Added line 1${dot}west live first")
    }
    Click 'Back to comparison'
    Click "Use pane $west as comparison reference"
    $null = Wait-Until 'the manual reference reverses the comparison' {
        (Find-Element 'Selected reference group') -and
            (Find-Element "First different line from selected reference${dot}1") -and
            (Find-Element "View differences for pane $east" $button) -and
            !(Find-Element "View differences for pane $west" $button)
    }
    Click 'Refresh comparison'
    $null = Wait-Until 'refresh keeps the chosen reference' {
        (Find-Element 'Selected reference group') -and (Find-Element "View differences for pane $east" $button)
    }
    Click "View differences for pane $east"
    $null = Wait-Until 'the diff uses West as the new baseline' {
        (Find-Element "Reference${dot}$westName${dot}Exit 17") -and
            (Find-Element "Selected${dot}$eastName${dot}Exit 0") -and
            (Find-Element "Removed line 1${dot}west live first") -and
            (Find-Element "Added line 1${dot}east-accepted both")
    }
    Click ('Save full differences' + [char]0x2026)
    $dialog = Wait-Until 'owned output difference save dialog' { Find-OwnedDialog 'Save SSH group differences' }
    $null = Wait-Until 'save the full output difference' { [FixtureDialog]::ClickButton($dialog, 1) }
    $diffPath = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR "ssh-group-diff-2-$west-to-$east.txt"
    $null = Wait-Until 'the saved difference preserves the manual reference and changed lines' {
        if ([IO.File]::Exists($diffPath)) {
            $diff = [IO.File]::ReadAllText($diffPath, [Text.UTF8Encoding]::new($false, $true))
            $diff.StartsWith("Submitted${dot}both`r`nReference${dot}$westName${dot}Exit 17`r`nSelected${dot}$eastName${dot}Exit 0`r`n") -and
                $diff.Contains("Removed line 1${dot}west live first`r`n") -and
                $diff.Contains("Removed line 4${dot}west-accepted both`r`n") -and
                $diff.Contains("Added line 1${dot}east-accepted both`r`n")
        }
    }
    Click 'Back to comparison'
    Click 'Use largest group'
    $null = Wait-Until 'automatic selection restores the largest reference group' {
        (Find-Element 'Largest result group') -and (Find-Element "View differences for pane $west" $button)
    }
    Click "View comparison output for pane $west"
    $null = Wait-Until 'comparison links to the exact West command history entry' {
        Find-Element "Selected, both${dot}exit 17" $button
    }
    Run-Palette 'SSH host group'
    $null = Wait-Until 'returning to the group restores the comparison view' { Find-Element 'SSH group result comparison' }
    Click "Prepare follow-up for result group starting at pane $west${dot}Prepare follow-up (1 host)"
    Wait-CommandDraft 'both'
    $null = Wait-CheckBox $eastName Off
    $null = Wait-CheckBox $westName On
    $null = Wait-Until 'preparing a follow-up did not create a submission' { Find-Element "Submitted${dot}both" }
    Click 'Restore draft'
    Wait-CommandDraft 'draft before withheld retry'
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On
    Outcome $westName 'Exit 17'
    $finishedPrefix = [regex]::Escape("$westName${dot}Shell time$dot")
    $null = Wait-Until 'the West result keeps its measured completion time' {
        (Result-Timing $west) -match ($finishedPrefix + '[1-9][0-9]*\.[0-9]{2} s$')
    }
    Expect-ResultOrder @($west, $east)
    Click $liveButton
    $null = Wait-Until 'completed West output in the earlier live feed' {
        (Find-Element 'west-accepted both') -and (Find-Element 'west while paused')
    }
    Click $recordOutput
    $dialog = Wait-Until 'picker for a second recording of the same submission' { Find-OwnedDialog 'Record SSH group output' }
    $null = Wait-Until 'cancel the second recording' { [FixtureDialog]::ClickButton($dialog, 2) }
    $null = Wait-Until 'dismiss the cancelled picker' { !(Find-OwnedDialog 'Record SSH group output') }
    Wait-RecordingStatus 'Saved live output to ' $logFile
    Click $recordOutput
    $dialog = Wait-Until 'picker for the second saved recording' { Find-OwnedDialog 'Record SSH group output' }
    $secondRecording = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'live both second.txt'
    $null = Wait-Until 'enter the distinct recording path' {
        [FixtureDialog]::SetFilename($dialog, $secondRecording, 'ssh-group-output-2.txt')
    }
    $null = Wait-Until 'save the second recording' { [FixtureDialog]::ClickButton($dialog, 1) }
    Wait-RecordingStatus 'Recording live output to ' $secondRecording
    Click 'Stop recording'
    Wait-RecordingStatus 'Saved live output to ' $secondRecording
    Select-ComboValue 'Saved recordings' $firstRecordedPath
    Wait-RecordingStatus 'Saved live output to ' $logFile
    Click 'Copy recording path'
    $null = Wait-Until 'copy the chosen older recording path' {
        $path = Read-ClipboardFixture
        $path -and [FixtureDialog]::SameFile($logFile, $path)
    }
    Click 'Back to results'
    Wait-RecordingStatus 'Saved live output to ' $logFile
    $moved = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'moved-recording.txt'
    [IO.File]::Move($logFile, $moved)
    try {
        Click 'Show in File Explorer'
        $null = Wait-Until 'missing recording status in the result view' {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
                    [System.Windows.Automation.Condition]::TrueCondition)) {
                if ($item.Current.Name.StartsWith('Could not show the recording in File Explorer')) { return $true }
            }
        }
        if ([IO.File]::Exists($env:W11TERM_TEST_EXPLORER_TRACE)) {
            throw 'The missing recording reached the Windows Shell selector'
        }
    } finally {
        [IO.File]::Move($moved, $logFile)
    }
    Result-Progress "2 of 2 settled${dot}0 waiting${dot}0 running${dot}1 succeeded${dot}1 needs attention"
    Set-ResultFilter 'Needs attention'
    $null = Wait-Until 'show only the failed result' {
        (Find-Element "Focus result pane $west" $button) -and !(Find-Element "Focus result pane $east" $button)
    }
    Click 'Copy results'
    $null = Wait-Until 'the earlier report contains its own outputs and exit statuses' {
        $report = Read-ClipboardFixture
        $report -and $report.StartsWith("Submitted${dot}both") -and
            $report.IndexOf("$eastName${dot}Sent to SSH channel") -lt
                $report.IndexOf("$westName${dot}Sent to SSH channel") -and
            $report.Contains("  Exit 0") -and $report.Contains('east-accepted both') -and
            $report.Contains("  Exit 17") -and $report.Contains('west-accepted both') -and
            $report.Contains('west while paused') -and
            !$report.Contains('east-report-tail')
    }
    Select-ComboValue 'Sort submission results' 'Submission order'
    Click "View output for pane $west"
    $null = Wait-Until 'full West output opens the matching command history entry' {
        Find-Element "Selected, both${dot}exit 17" $button
    }
    Run-Palette 'SSH host group'
    Outcome $westName 'Exit 17'
    Run-Palette ('Find group run' + [char]0x2026)
    (Wait-PickerEditor $runSearch 'Find group run from the palette').GetCurrentPattern(
        [System.Windows.Automation.ValuePattern]::Pattern).SetValue('#3')
    $null = Wait-Until 'the run number selects the exact submission' {
        (Find-Element "Run 3${dot}while-slow" $button) -and !(Find-Element "Run 2${dot}both" $button)
    }
    Press-Key 13
    $null = Wait-Until 'later submission restored by run number' { Find-Element "Submitted${dot}while-slow" }
    Click 'Attention inbox, 1 unread'
    $runNotice = "Unread${dot}SSH host group${dot}Run 2${dot}Group run finished${dot}1 succeeded${dot}1 needs attention"
    $null = Wait-Until 'the attention view is active before reading the background roster' {
        Find-Element $runNotice $button
    }
    $beforeRosterFocus = [FixtureMessages]::GetForegroundWindow()
    $backgroundRoster = Roster-Snapshot
    if ($backgroundRoster.counts.hosts -ne 2 -or [FixtureMessages]::GetForegroundWindow() -ne $beforeRosterFocus -or
        !(Find-Element $runNotice $button)) {
        throw 'Reading the background group roster changed focus or lost its hosts'
    }
    Click $runNotice
    $null = Wait-Until 'the alert opens its original run and marks the inbox read' {
        (Find-Element "Submitted${dot}both") -and (Find-Element 'Attention inbox, 0 unread' $button)
    }
    Outcome $westName 'Exit 17'
    Click 'Next submission'
    $null = Wait-Until 'return to the later submission after visiting the alert' {
        Find-Element "Submitted${dot}while-slow"
    }
    Send-Raw $west 'part'
    Pane-Status $west "Connected${dot}Waiting for a fresh prompt"
    Submit 'while-edit' 'Send to 2 hosts'
    Outcome $eastName 'Sent to SSH channel'
    Outcome $westName "Not sent${dot}Waiting for a fresh prompt"
    Send-Raw $west 'alt'
    Pane-Status $west "Connected${dot}Alternate screen"
    $rosterSort = 'Sort SSH roster'
    $null = Combo-Popup $rosterSort 'Tab order'
    Expect-PaneButtonOrder 'Focus pane ' @($east, $west)
    Select-ComboValue $rosterSort 'Needs attention first'
    Expect-PaneButtonOrder 'Focus pane ' @($west, $east)
    Submit 'while-alt' 'Send to 2 hosts'
    Outcome $eastName 'Sent to SSH channel'
    Outcome $westName "Not sent${dot}Alternate screen"
    Expect-ResultOrder @($east, $west)
    (Wait-CheckBox $showLastOutput Off).Toggle()
    $null = Wait-Until 'the alternate-screen host has no roster preview' {
        (Find-Element $openEastOutput $button) -and !(Find-Element $openWestOutput $button)
    }
    $copyRoster = 'Copy shown roster'
    Pane-Status $east "Connected${dot}Ready"
    (Wait-CheckBox $westName On).Toggle()
    $null = Wait-CheckBox $westName Off
    $null = Wait-Until 'the roster summarizes both ready and alternate-screen hosts' {
        Find-Element "2 matching hosts${dot}Ready (1)${dot}Alternate screen (1)"
    }
    Click $copyRoster
    $null = Wait-Until 'the copied roster includes both the ready and unchecked alternate-screen host' {
        $report = Read-ClipboardFixture
        (Find-Element 'Copied roster with 2 hosts') -and $report -and
            $report.Contains("2 shown${dot}1 checked") -and
            $report.Contains("Checked${dot}$eastName${dot}Connected${dot}Ready") -and
            $report.Contains("Unchecked${dot}$westName${dot}Connected${dot}Alternate screen") -and
            $report.IndexOf($westName) -lt $report.IndexOf($eastName) -and
            !$report.Contains('east-accepted while-alt')
    }
    Select-ComboValue $rosterSort 'Checked first'
    Expect-PaneButtonOrder 'Focus pane ' @($east, $west)
    (Wait-CheckBox $westName Off).Toggle()
    $null = Wait-CheckBox $westName On
    Select-ComboValue $rosterSort 'Needs attention first'
    Select-ComboValue $hostStateFilter 'Ready'
    $null = Wait-Until 'the ready filter scopes the roster and send count to East' {
        $send = Find-Element 'Send to 1 host' $button
        $send -and $send.Current.IsEnabled -and (Find-Element "Focus pane $east" $button) -and
            !(Find-Element "Focus pane $west" $button) -and
            (Find-Element '1 selected host is hidden by the filter.') -and
            (Find-Element "Showing 1 of 2 matching hosts${dot}Ready (1)${dot}Alternate screen (1)")
    }
    $roster = Roster-Snapshot
    if ($roster.schema_version -ne 1 -or $roster.label -cne 'SSH host group' -or
        $roster.filter.state -cne 'ready' -or $roster.sort -cne 'attention-first' -or $roster.counts.checked -ne 2 -or
        $roster.counts.checked_shown -ne 1 -or $roster.counts.ready_checked_shown -ne 1 -or !$roster.selection_ready -or
        $roster.hosts[0].pane_id -ne $west -or $roster.hosts[0].shown -or !$roster.hosts[0].checked -or
        $roster.hosts[0].state -cne 'alternate' -or $roster.hosts[1].pane_id -ne $east -or !$roster.hosts[1].shown -or
        $roster.hosts[1].profile -cne 'East' -or $roster.hosts[1].address -cne "test@127.0.0.1:$($profileData.East.port)") {
        throw 'The CLI roster lost the checked hidden host, display order, or readiness'
    }
    Select-ComboValue $hostStateFilter 'Alternate screen'
    Run-Palette 'Copy shown SSH roster'
    $null = Wait-Until 'only the host shown by the state filter is copied' {
        $report = Read-ClipboardFixture
        (Find-Element 'Copied roster with 1 host') -and $report -and
            $report.Contains("1 shown${dot}1 checked") -and
            $report.Contains("Checked${dot}$westName${dot}Connected${dot}Alternate screen") -and
            !$report.Contains($eastName)
    }
    Set-HostFilter 'east'
    $null = Wait-Until 'the text filter still applies inside the selected host state' {
        $copy = Find-Element $copyRoster $button
        (Find-Element 'No SSH panes match this filter.') -and $copy -and !$copy.Current.IsEnabled -and
            (Find-Element "Showing 0 of 1 matching host${dot}Ready (1)")
    }
    Set-HostFilter ''
    $null = Wait-CheckBox $westName On
    $null = Wait-Until 'West is shown with its original check but cannot receive a command' {
        $send = Find-Element 'Send to 1 host' $button
        $send -and !$send.Current.IsEnabled -and !(Find-Element "Focus pane $east" $button)
    }
    Select-ComboValue $hostStateFilter 'All host states'
    $roster = Roster-Snapshot
    if ($roster.require_all_ready -or !$roster.selection_ready -or $roster.counts.ready_checked_shown -ne 1 -or
        $roster.counts.checked_shown -ne 2) {
        throw 'Expected the normal send policy to allow East while West was still unavailable'
    }
    $readyTimeout = & $Cli --pid $app.Id @readyArgs --timeout 1 | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $readyTimeout.ok -or $readyTimeout.error -notlike '*timed out before all*') {
        throw 'The readiness wait did not require both shown checked hosts'
    }
    $readyOutput = Join-Path $State 'wait-group-ready.json'
    $readyJob = Start-Process -FilePath $Cli -ArgumentList (@('--pid',"$($app.Id)") + $readyArgs + @('--timeout','15')) `
        -NoNewWindow -PassThru -RedirectStandardOutput $readyOutput
    Click 'Attention inbox, 0 unread'
    $null = Wait-Until 'Attention view before releasing the West prompt' {
        Find-Element 'Attention inbox' ([System.Windows.Automation.ControlType]::Text)
    }
    $readyFocus = [FixtureMessages]::GetForegroundWindow()
    if ($readyJob.HasExited) { throw 'The readiness wait ended while West was in the alternate screen' }
    Send-Raw $west 'prompt'
    if (!$readyJob.WaitForExit(10000)) { throw 'The readiness wait did not finish while the group was in the background' }
    $readyReply = [IO.File]::ReadAllText($readyOutput) | ConvertFrom-Json
    $readyRoster = $readyReply.result.roster
    if (!$readyReply.ok -or $readyRoster.counts.checked_shown -ne 2 -or
        $readyRoster.counts.ready_checked_shown -ne 2 -or
        [FixtureMessages]::GetForegroundWindow() -ne $readyFocus -or
        !(Find-Element 'Attention inbox' ([System.Windows.Automation.ControlType]::Text))) {
        throw "The readiness wait lost its original host scope or changed focus: $($readyReply.error)"
    }
    Run-Palette 'SSH host group'
    Select-ComboValue $hostStateFilter 'Ready'
    Pane-Status $west "Connected${dot}Ready"
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On
    Select-ComboValue $hostStateFilter 'All host states'
    Expect-PaneButtonOrder 'Focus pane ' @($east, $west)
    Select-ComboValue $rosterSort 'Tab order'
    $null = Wait-Until 'the retained preview returns when West leaves the alternate screen' {
        Find-Element $openWestOutput $button
    }
    $previous = (Control @('inspect','--pane',"$west")).connection_generation
    [IO.File]::WriteAllText($westShellGate, '')
    Send-Raw $west 'drop'
    Pane-Status $west "Reconnecting${dot}attempt 1"
    Select-ComboValue $hostStateFilter 'Reconnecting'
    $null = Wait-Until 'the retry filter isolates West and shows its attempt' {
        (Find-Element "Pane $west${dot}Reconnecting${dot}attempt 1") -and
            !(Find-Element "Focus pane $east" $button) -and !(Find-Element "Reconnect SSH pane $west" $button)
    }
    Click $copyRoster
    $null = Wait-Until 'the roster report carries the automatic retry' {
        $report = Read-ClipboardFixture
        $report -and $report.Contains("$westName${dot}Reconnecting${dot}attempt 1") -and !$report.Contains($eastName)
    }
    Remove-Item -LiteralPath $westShellGate
    Select-ComboValue $hostStateFilter 'All host states'
    $null = Wait-Until 'replacement SSH transport' {
        (Control @('inspect','--pane',"$west")).connection_generation -gt $previous
    }
    Pane-Status $west "Connected${dot}Prompt unverified"
    if (Find-Element $openWestOutput $button) { throw 'The roster preview carried across an SSH reconnect' }
    Submit 'while-reconnecting' 'Send to 2 hosts'
    Outcome $eastName 'Sent to SSH channel'
    Outcome $westName "Not sent${dot}Prompt unverified"
    $requireReady = 'Require all recipients ready'
    $sendRecordedOption = 'Send and record' + [char]0x2026
    (Wait-CheckBox $requireReady Off).Toggle()
    (Command-Editor).SetValue('after-reconnect')
    $null = Wait-Until 'both send actions wait for the unverified checked host' {
        $send = Find-Element 'Send to 2 hosts' $button
        $record = Find-Element $sendRecordedOption $button
        $send -and !$send.Current.IsEnabled -and $record -and !$record.Current.IsEnabled -and
            (Find-Element "1 of 2 selected hosts ready${dot}waiting for all")
    }
    Set-HostFilter 'east'
    $null = Wait-Until 'strict readiness respects the visible recipient scope' {
        $send = Find-Element 'Send to 1 host' $button
        $send -and $send.Current.IsEnabled
    }
    Set-HostFilter ''
    $null = Wait-Until 'returning West to the scope blocks sending again' {
        $send = Find-Element 'Send to 2 hosts' $button
        $send -and !$send.Current.IsEnabled
    }
    Send-Raw $west 'prompt'
    Pane-Status $west "Connected${dot}Ready"
    $null = Wait-Until 'both send actions become available when both prompts are fresh' {
        $send = Find-Element 'Send to 2 hosts' $button
        $record = Find-Element $sendRecordedOption $button
        $send -and $send.Current.IsEnabled -and $record -and $record.Current.IsEnabled
    }
    if (Find-Element "Submitted${dot}after-reconnect") { throw 'The ready check sent the draft automatically' }
    Submit 'after-reconnect' 'Send to 2 hosts'
    Outcome $eastName 'Exit 0'
    Outcome $westName 'Exit 17'
    $null = Wait-Until 'the roster preview uses the replacement SSH connection output' {
        Find-Element "Pane $west${dot}Last command${dot}Exit 17${dot}west-accepted after-reconnect"
    }
    (Wait-CheckBox $showLastOutput On).Toggle()
    (Wait-CheckBox $requireReady On).Toggle()
    $null = Wait-Until 'finished results cannot be interrupted' {
        $entry = Find-Element 'Interrupt selected' $button
        $entry -and !$entry.Current.IsEnabled
    }
    Submit 'interrupt-pair' 'Send to 2 hosts'
    Outcome $eastName 'Running'
    Outcome $westName 'Running'
    Set-ResultFilter 'Succeeded'
    $null = Wait-Until 'the empty result filter disables interrupt' {
        $interrupt = Find-Element 'Interrupt selected' $button
        (Find-Element 'No recipients match this result filter.') -and $interrupt -and !$interrupt.Current.IsEnabled
    }
    Result-Progress "0 of 2 settled${dot}0 waiting${dot}2 running${dot}0 succeeded${dot}0 need attention"
    Set-ResultFilter 'All results'
    Set-HostFilter "test@127.0.0.1:$($profileData.West.port)"
    $null = Wait-Until 'East is selected and hidden' {
        Find-Element '1 selected host is hidden by the filter.'
    }
    Set-RecipientSearch 'east'
    Expect-ResultOrder @($east)
    $null = Wait-Until 'disjoint roster and result filters disable interrupt' {
        $entry = Find-Element 'Interrupt selected' $button
        $entry -and !$entry.Current.IsEnabled
    }
    Set-RecipientSearch 'west'
    Expect-ResultOrder @($west)
    Click 'Interrupt selected'
    Outcome $westName 'Ctrl+C sent'
    Outcome $westName 'Exit 130'
    Click 'Clear recipient search'
    Outcome $eastName 'Running'
    $null = Wait-Until 'only hidden East remains running' {
        $entry = Find-Element 'Interrupt selected' $button
        $entry -and !$entry.Current.IsEnabled
    }
    Set-HostFilter ''
    (Wait-CheckBox $eastName On).Toggle()
    $null = Wait-CheckBox $eastName Off
    $null = Wait-Until 'unselected East cannot be interrupted' {
        $entry = Find-Element 'Interrupt selected' $button
        $entry -and !$entry.Current.IsEnabled
    }
    $interruptRun = ((Control @('list-ssh-groups')).windows.groups.runs | Where-Object label -ceq 'interrupt-pair').run_id
    $interruptArgs = @('interrupt-ssh-group-command','--window',"$groupWindow",'--tab',"$groupTab",'--run',"$interruptRun")
    $outside = & $Cli --pid $app.Id @interruptArgs --pane $local | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $outside.ok -or $outside.error -notlike '*not a recipient in this run*') {
        throw 'An SSH group interrupt accepted a pane that was outside the run'
    }
    $interrupted = Control ($interruptArgs + @('--pane',"$east"))
    $eastReceipt = $interrupted.run.recipients | Where-Object pane_id -eq $east
    if ($interrupted.window_id -ne $groupWindow -or $interrupted.tab_id -ne $groupTab -or
        $eastReceipt.interrupt.state -notin @('pending','sent')) {
        throw 'The scoped interrupt did not report delivery state for East'
    }
    (Wait-CheckBox $eastName Off).Toggle()
    $null = Wait-CheckBox $eastName On
    Outcome $eastName 'Ctrl+C sent'
    Outcome $eastName 'Exit 130'
    Submit 'interrupt-both' 'Send to 2 hosts'
    Outcome $eastName 'Running'
    Outcome $westName 'Running'
    $stale = & $Cli --pid $app.Id @interruptArgs --pane $east | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $stale.ok -or $stale.error -notlike '*no verified running command*') {
        throw 'An interrupt for the previous run reached a newer command on the same pane'
    }
    Click 'Interrupt selected'
    Outcome $eastName 'Exit 130'
    Outcome $westName 'Exit 130'
    Set-RecipientSearch 'west'
    Expect-ResultOrder @($west)
    Click 'Previous submission'
    $null = Wait-Until 'show the previous interrupt' { Find-Element "Submitted${dot}interrupt-pair" }
    Expect-ResultOrder @($west)
    Click 'Previous submission'
    $null = Wait-Until 'show the command to load' { Find-Element "Submitted${dot}after-reconnect" }
    Expect-ResultOrder @($west)
    Click 'Clear recipient search'
    Set-HostFilter 'west'
    $null = Wait-Until 'East is selected but hidden during reuse' {
        Find-Element '1 selected host is hidden by the filter.'
    }
    (Command-Editor).SetValue('unsent notes')
    Click 'Load command'
    Wait-CommandDraft 'after-reconnect'
    Click 'Restore draft'
    Wait-CommandDraft 'unsent notes'
    Click 'Load command'
    Wait-CommandDraft 'after-reconnect'
    Submit 'after-reconnect-review' 'Send to 1 host'
    Outcome $westName 'Exit 17'
    $null = Wait-Until 'submission clears the restore action' { !(Find-Element 'Restore draft' $button) }
    (Command-Editor).SetValue('draft before library')
    $libraryButton = 'Saved commands' + [char]0x2026
    $saved = "Load group command${dot}Fleet health"
    Click $libraryButton
    $librarySearch = Wait-Until 'the group library opens with its saved command results' {
        $entry = Find-Element 'Search saved commands' $edit
        $provider = $null
        if ($entry -and $entry.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider
            ) -and $provider.Current.Value -ceq '' -and
            (Find-Element "Load group command${dot}Fleet uptime" $button) -and
            !(Find-Element 'SSH host group' $button)) { $provider }
    }
    $librarySearch.SetValue('FLEET HEALTH')
    $null = Wait-Until 'the library shows only the matching saved command' {
        (Find-Element $saved $button) -and !(Find-Element "Load group command${dot}Fleet uptime" $button)
    }
    Save-FixtureCommand 'show  fleet-health'
    Click $saved
    Click ("This saved command changed. Open the palette again.  " + [char]0xb7 + '  Dismiss')
    if ((Command-Editor).Current.Value -cne 'draft before library') { throw 'A stale saved command replaced the draft' }
    Click $libraryButton
    Click $saved
    Wait-CommandDraft 'show  fleet-health'
    Click 'Restore draft'
    Wait-CommandDraft 'draft before library'
    Run-Palette $saved
    Wait-CommandDraft 'show  fleet-health'
    Click 'Send to 1 host'
    $null = Wait-Until 'the saved command was explicitly submitted' { Find-Element "Submitted${dot}show  fleet-health" }
    Outcome $westName 'Exit 17'
    $saveButton = 'Save as command' + [char]0x2026
    (Command-Editor).SetValue('snapshot  status')
    Click $saveButton
    (Command-Editor 'Saved command name').SetValue('Discarded name')
    Click 'Cancel saving command'
    $null = Wait-Until 'canceled naming form is gone' { !(Find-Element 'Saved command name' $edit) }
    $entries = Get-Content -LiteralPath $library -Raw | ConvertFrom-Json
    if ($entries.Count -ne 2 -or
        (Command-Editor).Current.Value -cne 'snapshot  status') {
        throw 'Canceling save changed the draft or command library'
    }
    Click $saveButton
    $null = Wait-Until 'save form shows the exact captured draft' { Find-Element 'snapshot  status' }
    (Command-Editor).SetValue('new unsent notes')
    (Command-Editor 'Saved command name').SetValue('Fleet health')
    Click 'Save to commands'
    $null = Wait-Until 'duplicate command name is reported' { Find-Element 'A command named Fleet health already exists.' }
    (Command-Editor 'Saved command name').SetValue('Fleet snapshot')
    Click 'Save to commands'
    $null = Wait-Until 'group command was saved' { Find-Element "Saved command${dot}Fleet snapshot" }
    $entries = Get-Content -LiteralPath $library -Raw | ConvertFrom-Json
    if ($entries.Count -ne 3 -or
        ($entries | Where-Object name -ceq 'Fleet health').text -cne 'show  fleet-health' -or
        ($entries | Where-Object name -ceq 'Fleet snapshot').text -cne 'snapshot  status' -or
        (Command-Editor).Current.Value -cne 'new unsent notes') {
        throw 'Saving did not preserve the captured command, existing entry, and current draft'
    }
    Click $libraryButton
    Click "Load group command${dot}Fleet snapshot"
    Wait-CommandDraft 'snapshot  status'
    Click 'Send to 1 host'
    $null = Wait-Until 'the new library entry was explicitly submitted' { Find-Element "Submitted${dot}snapshot  status" }
    Outcome $westName 'Exit 17'
    Set-HostFilter ''
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On
    Click "Focus result pane $west"
    Wait-Focus $west

    Run-Palette 'SSH host group'
    (Command-Editor).SetValue('draft before reconnect')
    $beforeReconnect = @((Control @('list')).sessions)
    $exitTokens = @{}
    foreach ($pane in $east,$west) {
        $null = Control @('send-text','--pane',"$pane",'--no-paste','--base64','BA==')
        $exitTokens[$pane] = Wait-Until "SSH pane $pane exits" { (Control @('events','--pane',"$pane")).restart_token }
        Pane-Status $pane "Disconnected${dot}reconnect available"
    }
    Set-HostFilter 'east'
    $null = Wait-Until 'the ended West pane is hidden' { !(Find-Element "Reconnect SSH pane $west" $button) }
    Click 'Reconnect selected'
    $null = Wait-Until 'one visible reconnect requested' { Find-Element 'Reconnect requested for 1 host.' }
    Pane-Status $east "Connected${dot}Ready"
    if ((Control @('events','--pane',"$west")).restart_token -ne $exitTokens[$west]) {
        throw 'Reconnect selected claimed a hidden pane'
    }
    Set-HostFilter ''
    (Wait-CheckBox $westName On).Toggle()
    $null = Wait-CheckBox $westName Off
    $null = Wait-Until 'the bulk action excludes the unchecked host and the live host' {
        $bulk = Find-Element 'Reconnect selected' $button
        $bulk -and !$bulk.Current.IsEnabled -and !(Find-Element "Reconnect SSH pane $east" $button)
    }
    $rejectWest = Join-Path (Split-Path -LiteralPath $westShellGate) 'reject-shell-request'
    [IO.File]::WriteAllText($rejectWest, '')
    [IO.File]::WriteAllText($westShellGate, '')
    Click "Reconnect SSH pane $west"
    Pane-Status $west 'Reconnecting'
    Remove-Item -LiteralPath $westShellGate
    Pane-Status $west "Connection failed${dot}reconnect available"
    Select-ComboValue $hostStateFilter 'Connection failed'
    $failureReason = "Pane $west${dot}remote server rejected a PTY or shell request (1 outstanding)"
    $null = Wait-Until 'the failure filter shows only West and its SSH diagnostic' {
        (Find-Element $failureReason) -and (Find-Element "Reconnect SSH pane $west" $button) -and
            !(Find-Element "Focus pane $east" $button)
    }
    Click $copyRoster
    $null = Wait-Until 'the copied roster includes the failed connection state' {
        $report = Read-ClipboardFixture
        $report -and $report.Contains("$westName${dot}Connection failed${dot}reconnect available")
    }
    Remove-Item -LiteralPath $rejectWest
    Select-ComboValue $hostStateFilter 'All host states'
    Click "Reconnect SSH pane $west"
    Pane-Status $west "Connected${dot}Ready"
    if (Find-Element $failureReason) { throw 'The previous SSH error remained after reconnect' }
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName Off
    if ((Command-Editor).Current.Value -cne 'draft before reconnect') {
        throw 'Reconnect changed the working command draft'
    }
    Outcome $westName 'Exit 17'
    $afterReconnect = @((Control @('list')).sessions)
    if ($afterReconnect.Count -ne $beforeReconnect.Count) { throw 'Reconnect opened or removed a pane' }
    foreach ($previous in $beforeReconnect) {
        $current = $afterReconnect | Where-Object id -eq $previous.id
        if (!$current -or $current.tab_id -ne $previous.tab_id) { throw 'Reconnect replaced a pane or tab' }
        if ($previous.id -in @($east,$west)) {
            if ($current.connection_generation -le $previous.connection_generation) {
                throw 'A requested SSH pane did not start a fresh connection'
            }
        } elseif ($current.connection_generation -ne $previous.connection_generation) {
            throw 'Reconnect changed the local PowerShell session'
        }
    }
    foreach ($pair in @(@($east,'east-accepted both'),@($west,'west-accepted snapshot  status'))) {
        $path = Join-Path $State "reconnect-pane-$($pair[0]).txt"
        $null = Control @('save-text','--pane',"$($pair[0])",$path)
        if (![IO.File]::ReadAllText($path).Contains($pair[1])) { throw 'Reconnect discarded previous SSH output' }
    }
    Click 'Prepare retry'
    Wait-CommandDraft 'snapshot  status'
    $null = Wait-CheckBox $eastName Off
    $null = Wait-CheckBox $westName On
    Click 'Send to 1 host'
    $null = Control @('wait-for-text','--pane',"$west",'--timeout','15','west-accepted snapshot  status')
    Outcome $westName 'Exit 17'
    Pane-Status $west "Connected${dot}Ready"
    Click 'Select shown'
    Submit 'shared-status' 'Send to 2 hosts'
    Outcome $eastName 'Exit 0'
    Outcome $westName 'Exit 0'
    Click $compareButton
    $null = Wait-Until 'identical complete results from both hosts share one group' {
        (Find-Element "1 distinct result${dot}2 hosts compared${dot}0 pending or unavailable") -and
        (Find-Element 'fleet-status-ok') -and (Find-Element "Focus compared pane $east" $button) -and
        (Find-Element "Focus compared pane $west" $button)
    }
    Click 'Back to results'
    Set-HostFilter 'west'
    Click $compareButton
    $firstComparedPane = [Math]::Min($east, $west)
    Click "Prepare follow-up for result group starting at pane $firstComparedPane${dot}Prepare follow-up (1 of 2 hosts in roster)"
    Wait-CommandDraft 'shared-status'
    $null = Wait-CheckBox $westName On
    Set-HostFilter ''
    $null = Wait-CheckBox $eastName Off
    Click 'Restore draft'
    Set-HostFilter ''
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On
    Pane-Status $east "Connected$($dot)Ready"
    Pane-Status $west "Connected$($dot)Ready"
    $recent = 'Search recent group commands'
    (Command-Editor).SetValue('unsent recent witness')
    Set-HostFilter 'west'
    Click ('Recent commands' + [char]0x2026)
    (Command-Editor $recent).SetValue('never-submitted-witness')
    $null = Wait-Until 'recent search reports no matches' { Find-Element 'No matching recent commands' }
    Press-Key 27
    $null = Wait-Until 'Escape closes recent search' { !(Find-Element $recent $edit) }
    if ((Command-Editor).Current.Value -cne 'unsent recent witness') { throw 'Escape changed the draft' }
    Run-Palette ('Recent group commands' + [char]0x2026)
    $null = Wait-PickerEditor $recent 'Recent group commands from the palette'
    Press-Key 27
    $null = Wait-Until 'the palette search closes' { !(Find-Element $recent $edit) }
    $entry = Wait-Until 'the SSH group command editor' { Find-Element 'Group command' $edit }
    $entry.SetFocus()
    $null = Wait-Until 'fixture owns input in its command editor' {
        $entry.Current.HasKeyboardFocus -and [FixtureMessages]::GetForegroundWindow() -eq $window
    }
    if ([FixtureMessages]::GetForegroundWindow() -ne $window) { throw 'Fixture lost keyboard input' }
    [Windows.Forms.SendKeys]::SendWait('^r')
    $null = Wait-PickerEditor $recent 'Ctrl+R'
    (Command-Editor $recent).SetValue('WHILE-SLO')
    $null = Wait-Until 'recent search finds an older full command without regard to case' {
        (Find-Element 'while-slow' $button) -and !(Find-Element 'shared-status' $button)
    }
    Press-Key 13
    Wait-CommandDraft 'while-slow'
    $null = Wait-Until 'recall leaves the current submission and hidden East selection in place' {
        (Find-Element "Submitted${dot}shared-status") -and
        (Find-Element '1 selected host is hidden by the filter.')
    }
    $null = Wait-CheckBox $westName On
    Click 'Restore draft'
    Wait-CommandDraft 'unsent recent witness'
    Set-HostFilter ''
    $sendRecorded = 'Send and record' + [char]0x2026
    (Command-Editor).SetValue('record-fast')
    Click $sendRecorded
    $dialog = Wait-Until 'record-before-send file picker' { Find-OwnedDialog 'Record SSH group output' }
    $null = Wait-Until 'cancel recording before dispatch' { [FixtureDialog]::ClickButton($dialog, 2) }
    $null = Wait-Until 'file picker closes without a recorded submission' { !(Find-OwnedDialog 'Record SSH group output') }
    if (Find-Element "Submitted$($dot)record-fast") { throw 'Cancelling the picker sent the command' }
    $previousLogs = @(Get-ChildItem -LiteralPath $env:W11TERM_EXPORT_SMOKE_DIR -Filter 'ssh-group-output-*.txt')
    Click $sendRecorded
    $dialog = Wait-Until 'second record-before-send file picker' { Find-OwnedDialog 'Record SSH group output' }
    $null = Wait-Until 'accept recording before dispatch' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'submission is attached to its recording' { Find-Element "Submitted$($dot)record-fast" }
    Outcome $eastName 'Exit 0'
    Outcome $westName 'Sent to SSH channel'
    $newLogs = @(Get-ChildItem -LiteralPath $env:W11TERM_EXPORT_SMOKE_DIR -Filter 'ssh-group-output-*.txt' |
        Where-Object Name -NotIn $previousLogs.Name)
    if ($newLogs.Count -ne 1 -or $newLogs[0].Name -notmatch '^ssh-group-output-(\d+)\.txt$') {
        throw 'Expected one new file for the recorded run'
    }
    $burstRunId = [int]$Matches[1]
    $nextName = "ssh-group-output-$($burstRunId + 1).txt"
    $burstPath = $newLogs[0].FullName
    $logReader = Open-PeerTraceReader $burstPath
    $null = Wait-Until 'the recorder is still open with West awaiting its result' {
        $captured = [Text.Encoding]::UTF8.GetString((Read-PeerTrace $logReader))
        $captured.Contains("$eastName$($dot)east record row 79") -and !$captured.Contains('[Group run settled]')
    }
    Click $liveButton
    $null = Wait-Until 'retained burst output is shown before arming an alert' {
        Find-Element 'east record row 79'
    }
    Click $savedAlertRules
    Click "Use alert rule $ruleName"
    $null = Wait-Until 'the live output view can load the same named rule' {
        (Command-Editor $outputSearch).Current.Value -ceq $ruleQuery
    }
    $null = Wait-CheckBox $outputRegex On
    $null = Wait-CheckBox $outputCase On
    Click 'Clear output filters'
    $null = Wait-CheckBox $outputRegex Off
    $null = Wait-CheckBox $outputCase Off
    Click 'Pause output'
    (Command-Editor $outputSearch).SetValue('record row')
    $null = Wait-Until 'the paused view has a valid search for a live alert' {
        $armAlert = Find-Element 'Notify on next match' $button
        $armAlert -and $armAlert.Current.IsEnabled
    }
    Click 'Notify on next match'
    $literalRule = "Output alert armed${dot}Literal text${dot}Ignore case${dot}All output hosts"
    Wait-ArmedOutputAlert $literalRule 'record row'
    $null = Wait-Until 'the alert arms without matching the retained East rows' {
        Find-Element 'Attention inbox, 0 unread' $button
    }
    Click 'Clear output filters'
    Wait-ArmedOutputAlert $literalRule 'record row'
    Click 'Back to results'
    $null = Wait-Until 'return to results with the alert still armed' { Find-Element "Submitted${dot}record-fast" }
    Wait-ArmedOutputAlert $literalRule 'record row'
    Click 'Cancel output alert'
    $null = Wait-Until 'cancelling the alert removes its rule from the run results' {
        !(Find-Element $literalRule) -and !(Find-Element 'Cancel output alert' $button)
    }
    Click $liveButton
    (Wait-CheckBox $outputRegex Off).Toggle()
    $null = Wait-CheckBox $outputRegex On
    (Wait-CheckBox $outputCase Off).Toggle()
    $null = Wait-CheckBox $outputCase On
    Select-ComboValue $outputHosts $westName
    (Command-Editor $outputSearch).SetValue('^west record row')
    Click 'Notify on next match'
    $regexRule = "Output alert armed${dot}Regular expression${dot}Match case${dot}$westName"
    Wait-ArmedOutputAlert $regexRule '^west record row'
    Click 'Back to results'
    Wait-ArmedOutputAlert $regexRule '^west record row'
    Click "Focus pane $west"
    Wait-Focus $west
    $followPath = Join-Path $State 'follow-burst.jsonl'
    $followArgs = @('--pid',"$($app.Id)",'follow-ssh-group-output','--window',"$groupWindow",'--tab',"$groupTab",
        '--run',"$burstRunId",'--after-line','0','--timeout','30')
    $followJob = Start-Process -FilePath $Cli -ArgumentList $followArgs -NoNewWindow -PassThru -RedirectStandardOutput $followPath
    $followReader = Open-PeerTraceReader $followPath
    $null = Wait-Until 'the follower flushes retained East lines while West is held' {
        $text = [Text.Encoding]::UTF8.GetString((Read-PeerTrace $followReader))
        $text.Contains('"text":"east record row 79"') -and !$followJob.HasExited
    }
    $followForeground = [FixtureMessages]::GetForegroundWindow()
    [IO.File]::WriteAllText((Join-Path $State 'release-record-fast'), '')
    if (!$followJob.WaitForExit(10000)) { throw 'The output follower did not finish after settlement' }
    $followReader.Dispose()
    $followReader = $null
    $stream = @([IO.File]::ReadAllLines($followPath) | ForEach-Object { $_ | ConvertFrom-Json })
    if ($stream.Count -lt 162 -or $stream[-1].result.kind -cne 'settled' -or
        @($stream | Where-Object { !$_.ok -or $_.result.run_id -ne $burstRunId }).Count -or
        [FixtureMessages]::GetForegroundWindow() -ne $followForeground) {
        throw 'The output follower lost records, changed focus, or missed settlement'
    }
    $streamedLines = @($stream | Where-Object { $_.result.kind -ceq 'line' } | ForEach-Object { $_.result.line })
    $cursor = 0
    foreach ($line in $streamedLines) {
        if ($line.line_id -ne ($cursor + 1)) { throw 'The output follower repeated or skipped a line ID' }
        $cursor = $line.line_id
    }
    if ($stream[-1].result.next_after_line -ne $cursor -or
        $streamedLines[-1].text -cne 'west record tail' -or $streamedLines[-1].pane_id -ne $west) {
        throw 'The output follower omitted the last live line or its cursor'
    }
    Wait-Focus $west
    $null = Wait-Until 'the recording ends automatically with the terminal tab focused' {
        [Text.Encoding]::UTF8.GetString((Read-PeerTrace $logReader)).Contains('[Group run settled]')
    }
    $logReader.Dispose()
    $logReader = $null
    Click 'Attention inbox, 1 unread'
    $outputNotice = "Unread${dot}SSH host group${dot}Run $burstRunId${dot}Matched output${dot}$westName${dot}west record row 00"
    $null = Wait-Until 'the unread alert identifies the matching host and line' { Find-Element $outputNotice $button }
    $matchedLine = $streamedLines | Where-Object text -CEQ 'west record row 00'
    $viewMatch = "View matching output for SSH host group, run $burstRunId, line $($matchedLine.line_id)"
    Click $viewMatch
    $null = Wait-Until 'the alert opens the match and two nearby lines from its source host' {
        $match = Find-Element 'west record row 00'
        $next = Find-Element 'west record row 01'
        $last = Find-Element 'west record row 02'
        (Find-Element 'SSH group output match') -and (Find-Element 'Matched line') -and
            (Find-Element "Run $burstRunId${dot}$westName") -and
            $match -and $next -and $last -and
            $match.Current.BoundingRectangle.Top -lt $next.Current.BoundingRectangle.Top -and
            $next.Current.BoundingRectangle.Top -lt $last.Current.BoundingRectangle.Top -and
            !(Find-Element 'east record row 79') -and !(Find-Element 'west record row 03') -and
            (Find-Element 'Attention inbox, 0 unread' $button)
    }
    Click 'Back to inbox'
    $readOutputNotice = $outputNotice -creplace '^Unread', 'Read'
    $null = Wait-Until 'returning from the excerpt keeps the notice read' { Find-Element $readOutputNotice $button }
    Click $viewMatch
    Click 'Open run results'
    $null = Wait-Until 'the match view opens its original run' {
        (Find-Element "Submitted${dot}record-fast") -and (Find-Element 'Attention inbox, 0 unread' $button) -and
            !(Find-Element $regexRule) -and !(Find-Element 'Cancel output alert' $button)
    }
    Outcome $westName 'Exit 17'
    Wait-RecordingStatus 'Saved live output to ' $burstPath
    $burst = [IO.File]::ReadAllLines($burstPath)
    $expectedTail = @(
        "$westName$($dot)west record tail", '[Host outcomes]',
        "$eastName$($dot)Sent to SSH channel$($dot)Exit 0",
        "$westName$($dot)Sent to SSH channel$($dot)Exit 17", '[Group run settled]'
    )
    if ($burst[0] -cne "Submitted$($dot)record-fast" -or
        ($burst[-5..-1] -join '|') -cne ($expectedTail -join '|')) {
        throw "Unexpected final output and outcomes: $($burst[-5..-1] -join ' | ')"
    }
    foreach ($side in @('east', 'west')) {
        $prefix = if ($side -eq 'east') { $eastName + $dot } else { $westName + $dot }
        $actual = @($burst | Where-Object { $_.StartsWith("$prefix$side record row ") })
        $expected = @(0..79 | ForEach-Object { "$prefix$side record row " + $_.ToString('00') })
        if (($actual -join '|') -cne ($expected -join '|')) {
            throw "Unexpected burst from $side ($($actual.Count) of 80 rows): $($actual -join ', ')"
        }
    }
    $burstWait = @('wait-ssh-group-output','--window',"$groupWindow",'--tab',"$groupTab",'--run',"$burstRunId",'--after-line','0')
    $lost = & $Cli --pid $app.Id @burstWait 'east record row 79' | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $lost.ok -or $lost.error -notlike '*history after cursor 0 is incomplete through line*') {
        throw 'A cursor preceding trimmed combined history did not report a gap'
    }
    $kept = Control ($burstWait + @('--pane',"$east",'east record row 00'))
    if ($kept.line.pane_id -ne $east -or $kept.line.text -cne 'east record row 00') {
        throw 'A gap in the combined feed incorrectly affected the East cursor'
    }
    $burstRead = @('get-ssh-group-output','--window',"$groupWindow",'--tab',"$groupTab",'--run',"$burstRunId")
    $page = Control ($burstRead + @('--after-line','0','--limit','2'))
    if ($page.run_id -ne $burstRunId -or !$page.settled -or !$page.gap -or
        !$page.has_more -or $page.lines.Count -ne 2 -or
        $page.lines[0].line_id -ne ($page.evicted_through + 1) -or
        $page.next_after_line -ne $page.lines[1].line_id -or
        $page.next_after_line -ge $page.latest_line) {
        throw 'Reading the combined output did not report its gap and first page'
    }
    $replay = @(& $Cli --pid $app.Id follow-ssh-group-output --window $groupWindow --tab $groupTab --run $burstRunId |
        ForEach-Object { $_ | ConvertFrom-Json })
    if ($LASTEXITCODE -ne 0 -or $replay[0].result.kind -cne 'gap' -or
        $replay[0].result.evicted_through -ne $page.evicted_through -or
        $replay[1].result.line.line_id -ne $page.lines[0].line_id -or
        $replay[-1].result.kind -cne 'settled' -or $replay[-1].result.next_after_line -ne $page.latest_line) {
        throw 'Following a settled run did not report the history gap and replay the retained lines'
    }
    $strictFollow = & $Cli --pid $app.Id follow-ssh-group-output --window $groupWindow --tab $groupTab --run $burstRunId --after-line 0 | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $strictFollow.ok -or $strictFollow.error -notlike '*incomplete through line*') {
        throw 'Following from a missing cursor did not fail before sending history'
    }
    $nextPage = Control ($burstRead + @('--after-line',"$($page.next_after_line)",'--limit','2'))
    if ($nextPage.gap -or !$nextPage.has_more -or $nextPage.lines.Count -ne 2 -or
        $nextPage.lines[0].line_id -ne ($page.next_after_line + 1)) {
        throw 'The second output page did not resume immediately after the first'
    }
    $eastRead = $burstRead + @('--pane',"$east")
    $eastPage = Control ($eastRead + @('--after-line','0'))
    $firstEast = $eastPage.lines | Where-Object text -CEQ 'east record row 00'
    if ($eastPage.pane_id -ne $east -or $eastPage.gap -or $eastPage.has_more -or
        $eastPage.evicted_through -ne 0 -or $firstEast.line_id -ne $kept.line.line_id -or
        $firstEast.label -cne 'East' -or @($eastPage.lines | Where-Object pane_id -NE $east).Count -or
        $eastPage.next_after_line -ne $eastPage.latest_line -or $eastPage.latest_line -ne $page.latest_line) {
        throw 'Reading East output did not preserve its own history and the run cursor'
    }
    $drained = Control ($eastRead + @('--after-line',"$($eastPage.next_after_line)"))
    if (!$drained.settled -or $drained.lines.Count -ne 0 -or $drained.has_more -or
        $drained.next_after_line -ne $eastPage.next_after_line) {
        throw 'Reading from the final cursor did not return an empty settled page'
    }
    Click $liveButton
    $null = Wait-Until 'open live output for the recorded burst' { Find-Element 'SSH group live output' }
    (Wait-CheckBox $outputRegex Off).Toggle()
    $null = Wait-CheckBox $outputRegex On
    (Command-Editor $outputSearch).SetValue('^east record row (00|79)$')
    $null = Wait-Until 'the shared tail lost the first East line after the West burst' {
        (Find-Element 'east record row 79') -and !(Find-Element 'east record row 00')
    }
    Click 'Pause output'
    $null = Wait-Until 'the combined view is paused' { Find-Element 'Resume output' $button }
    Select-ComboValue $outputHosts $eastName
    $null = Wait-Until 'selecting East recovers the older line from the same frozen output' {
        (Find-Element 'east record row 00') -and (Find-Element 'east record row 79') -and
            (Find-Element 'Resume output' $button)
    }
    Click 'Copy shown output'
    $null = Wait-Until 'the recovered lines are available to clipboard exports' {
        (Read-ClipboardFixture) -ceq "Submitted${dot}record-fast`r`n$eastName${dot}east record row 79`r`n$eastName${dot}east record row 00`r`n"
    }
    Click 'Back to results'
    $null = Wait-Until 'return to the recorded burst results' { Find-Element "Submitted${dot}record-fast" }

    $beforeCollision = [IO.File]::ReadAllText($burstPath)
    (Command-Editor).SetValue('never-send-on-collision')
    Click $sendRecorded
    $dialog = Wait-Until 'recording file picker for collision' { Find-OwnedDialog 'Record SSH group output' }
    $null = Wait-Until 'choose the existing capture' { [FixtureDialog]::SetFilename($dialog, $burstPath, $nextName) }
    $null = Wait-Until 'confirm the existing name in the picker' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'exclusive creation reports a collision' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        foreach ($element in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.Condition]::TrueCondition)) {
            if ($element.Current.Name.StartsWith('Could not prepare the recording')) { return $true }
        }
    }
    if ([IO.File]::ReadAllText($burstPath) -cne $beforeCollision) { throw 'The existing recording was modified' }
    (Wait-CheckBox $requireReady Off).Toggle()
    Select-ComboValue $hostStateFilter 'Ready'
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On
    (Command-Editor).SetValue('never-send-with-stale-prompt')
    Click $sendRecorded
    $dialog = Wait-Until 'file picker before a host changes prompt state' { Find-OwnedDialog 'Record SSH group output' }
    Send-Raw $west 'alt'
    $null = Control @('wait-for-text','--pane',"$west",'--timeout','15','alternate-proof')
    $null = Wait-Until 'approve a path after the target left its prompt' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'changed prompts require another review' {
        Find-Element 'The command, hosts or prompts changed. Review them before sending again.'
    }
    if ([IO.File]::ReadAllText((Join-Path $env:W11TERM_EXPORT_SMOKE_DIR $nextName)).Trim() -cne '[No command was sent]') {
        throw 'A stale review was presented as a recorded submission'
    }
    Select-ComboValue $hostStateFilter 'All host states'
    (Wait-CheckBox $requireReady On).Toggle()
    Pane-Status $west "Connected$($dot)Alternate screen"
    (Command-Editor).SetValue('record-one')
    Click $sendRecorded
    $dialog = Wait-Until 'file picker for a group with an unavailable host' { Find-OwnedDialog 'Record SSH group output' }
    $withheldPath = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'withheld.txt'
    $null = Wait-Until 'enter the filename for the partially delivered submission' {
        [FixtureDialog]::SetFilename($dialog, $withheldPath, $nextName)
    }
    $null = Wait-Until 'approve the new output path' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the final submission is shown' { Find-Element "Submitted$($dot)record-one" }
    Wait-RecordingStatus 'Saved live output to ' $withheldPath
    $outcomes = [IO.File]::ReadAllLines($withheldPath)
    if ("$eastName$($dot)Sent to SSH channel$($dot)Exit 0" -cnotin $outcomes -or
        "$westName$($dot)Not sent$($dot)Alternate screen" -cnotin $outcomes -or
        $outcomes[-1] -cne '[Group run settled]') {
        throw "The recording footer omitted the rejected host: $($outcomes -join ' | ')"
    }
    Set-HostFilter 'east'
    $null = Wait-CheckBox $eastName On
    $null = Wait-Until 'the missing-output check has only East in scope' {
        Find-Element 'Send to 1 host' $button
    }
    (Command-Editor).SetValue('check-expected')
    Click $alertedSend
    (Command-Editor $alertSearch).SetValue('ready marker')
    (Wait-CheckBox 'Alert if expected output is missing' Off).Toggle()
    $null = Wait-CheckBox 'Alert if expected output is missing' On
    Click 'Send with alert'
    $null = Wait-Until 'the verified run reports a missing match only after settling' {
        (Find-Element "Submitted${dot}check-expected") -and
            (Find-Element "$eastName${dot}Exit 0") -and (Find-Element 'Attention inbox, 1 unread' $button)
    }
    Click 'Attention inbox, 1 unread'
    $missingRun = $burstRunId + 2
    Click "Unread${dot}SSH host group${dot}Run $missingRun${dot}Expected output missing${dot}ready marker"
    $null = Wait-Until 'the missing-output notice opens the settled run and marks it read' {
        (Find-Element "Submitted${dot}check-expected") -and (Find-Element 'Attention inbox, 0 unread' $button)
    }
    Send-Raw $west 'prompt'
    Set-HostFilter ''
    Pane-Status $west "Connected${dot}Ready"
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On
    (Command-Editor).SetValue('check-each-host')
    Click $alertedSend
    $expectedLine = 'east-accepted check-each-host'
    (Command-Editor $alertSearch).SetValue($expectedLine)
    (Wait-CheckBox 'Alert if expected output is missing' Off).Toggle()
    $null = Wait-CheckBox 'Alert if expected output is missing' On
    (Wait-CheckBox 'Require a match from every host' Off).Toggle()
    $null = Wait-CheckBox 'Require a match from every host' On
    Click 'Send with alert'
    $everyHostRun = $missingRun + 1
    $runArgs = @('--window',"$groupWindow",'--tab',"$groupTab",'--run',"$everyHostRun")
    $finished = (Control (@('wait-ssh-group-run') + $runArgs + @('--timeout','15'))).run
    $details = (Control (@('get-ssh-group-run') + $runArgs)).run
    foreach ($result in @($finished, $details)) {
        $check = $result.output_expectation
        $matched = $result.recipients | Where-Object pane_id -eq $east
        $missing = $result.recipients | Where-Object pane_id -eq $west
        if ($check.query -cne $expectedLine -or $check.regex -ne $false -or
            $check.match_case -ne $false -or $matched.output_expectation -cne 'matched' -or
            $missing.output_expectation -cne 'missing') {
            throw "The run reply lost the resolved check: $($result | ConvertTo-Json -Depth 8 -Compress)"
        }
    }
    $null = Wait-Until 'a match on East leaves West under its own check' {
        (Find-Element "Submitted${dot}check-each-host") -and (Find-Element 'Attention inbox, 1 unread' $button)
    }
    Click 'Attention inbox, 1 unread'
    Click "Unread${dot}SSH host group${dot}Run $everyHostRun${dot}Expected output missing on 1 host${dot}$expectedLine"
    $null = Wait-Until 'run results identify West as the host missing expected output' {
        (Find-Element "Submitted${dot}check-each-host") -and
            (Find-Element "Expected output missing${dot}$westName") -and
            !(Find-Element "Expected output missing${dot}$eastName") -and
            (Find-Element 'Attention inbox, 0 unread' $button)
    }
    Click 'Copy results'
    $null = Wait-Until 'the text report includes the expected output check and both outcomes' {
        $text = Read-ClipboardFixture
        $text -and $text.Contains("Expected output${dot}$expectedLine") -and
            $text.Contains("  Expected output matched`r`n") -and $text.Contains("  Expected output missing`r`n")
    }
    $expectedJson = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'output check.json'
    Click ('Save results as JSON' + [char]0x2026)
    $dialog = Wait-Until 'owned expected output JSON save dialog' { Find-OwnedDialog 'Save SSH group results as JSON' }
    $null = Wait-Until 'enter the expected output JSON destination' {
        [FixtureDialog]::SetFilename($dialog, $expectedJson, "ssh-group-submission-$everyHostRun.json")
    }
    $null = Wait-Until 'save the expected output JSON report' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the expected output JSON report is published' { [IO.File]::Exists($expectedJson) }
    $saved = [IO.File]::ReadAllText($expectedJson) | ConvertFrom-Json
    $matched = $saved.recipients | Where-Object pane_id -eq $east
    $missing = $saved.recipients | Where-Object pane_id -eq $west
    if ($saved.schema_version -ne 1 -or $saved.output_expectation.query -cne $expectedLine -or
        $matched.output_expectation -cne 'matched' -or $missing.output_expectation -cne 'missing' -or
        $missing.output.state -cne 'complete' -or !$missing.output.text.Contains('west-accepted check-each-host')) {
        throw 'The JSON export lost the expected output check, its recipient outcomes, or the captured output'
    }
    $submitArgs = @('submit-ssh-group-command','--window',"$groupWindow",'--tab',"$groupTab")
    $badScope = & $Cli --pid $app.Id @submitArgs --pane $west --pane $local 'must-not-send' | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $badScope.ok -or $badScope.error -notlike "*SSH pane $local*outside*") {
        throw 'The CLI accepted a local pane as an SSH group recipient'
    }
    Send-Raw $west 'alt'
    Pane-Status $west "Connected${dot}Alternate screen"
    $notReady = & $Cli --pid $app.Id @submitArgs --pane $east --pane $west 'must-not-send' | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $notReady.ok -or $notReady.error -notlike "*SSH pane $west*not ready*") {
        throw 'The CLI accepted a submission while one of its explicitly named SSH panes was unready'
    }
    $outsideReady = & $Cli --pid $app.Id @readyArgs --pane $local --timeout 15 | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $outsideReady.ok -or $outsideReady.error -notlike "*Requested SSH pane $local*outside*") {
        throw 'The explicit readiness wait accepted a pane outside the SSH roster'
    }
    $explicitOutput = Join-Path $State 'wait-explicit-group-ready.json'
    $explicitArgs = $readyArgs + @('--pane',"$west",'--pane',"$east",'--timeout','15')
    $explicitReadyJob = Start-Process -FilePath $Cli -ArgumentList (@('--pid',"$($app.Id)") + $explicitArgs) `
        -NoNewWindow -PassThru -RedirectStandardOutput $explicitOutput
    (Wait-CheckBox $westName On).Toggle()
    Set-HostFilter 'east'
    $null = Wait-Until 'the roster hides and unchecks West while a CLI wait is pending' {
        $hidden = (Roster-Snapshot).hosts | Where-Object pane_id -eq $west
        $hidden -and !$hidden.shown -and !$hidden.checked -and $hidden.state -ceq 'alternate'
    }
    if ($explicitReadyJob.HasExited) { throw 'The explicit wait ended while a requested pane was unready' }
    Send-Raw $west 'prompt'
    if (!$explicitReadyJob.WaitForExit(10000)) { throw 'The explicit readiness wait did not finish' }
    $explicit = [IO.File]::ReadAllText($explicitOutput) | ConvertFrom-Json
    $hiddenReady = $explicit.result.roster.hosts | Where-Object pane_id -eq $west
    if (!$explicit.ok -or ($explicit.result.ready_pane_ids -join ',') -cne "$west,$east" -or
        $hiddenReady.shown -or $hiddenReady.checked -or $hiddenReady.state -cne 'ready') {
        throw 'The explicit readiness wait followed the UI selection or lost its requested pane order'
    }
    Set-HostFilter ''
    Pane-Status $west "Connected${dot}Ready"
    (Wait-CheckBox $westName Off).Toggle()
    $null = Wait-CheckBox $westName On
    (Command-Editor).SetValue('untouched command draft')
    Set-HostFilter 'east'
    $beforeSubmit = Wait-Until 'the East filter is committed before leaving the group' {
        $roster = Roster-Snapshot
        if (($roster.hosts | Where-Object shown).pane_id -eq $east -and $roster.counts.shown -eq 1) { $roster }
    }
    $null = Wait-Until 'the command draft is committed before leaving the group' {
        (Command-Editor).Current.Value -ceq 'untouched command draft'
    }
    Press-Key 0x77
    Wait-Focus $east
    $submitForeground = [FixtureMessages]::GetForegroundWindow()
    $badPattern = & $Cli --pid $app.Id @submitArgs --pane $west --expect-output '[' --regex 'must-not-send' | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $badPattern.ok -or $badPattern.error -notlike '*invalid expected output*') {
        throw 'The CLI accepted an invalid expected-output regex'
    }
    $existingRecording = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'recording exists.txt'
    [IO.File]::WriteAllText($existingRecording, 'existing recording')
    $badRecording = & $Cli --pid $app.Id @submitArgs --pane $west --record-output $existingRecording 'must-not-send' | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $badRecording.ok -or $badRecording.error -notlike '*Could not prepare the recording*' -or
        [IO.File]::ReadAllText($existingRecording) -cne 'existing recording') {
        throw 'The CLI accepted an existing recording destination or changed its contents'
    }
    $launched = Control ($submitArgs + @('--pane',"$west",'cli-only  west'))
    if ($launched.window_id -ne $groupWindow -or $launched.tab_id -ne $groupTab -or $launched.run_id -ne ($everyHostRun + 1)) {
        throw 'The CLI did not create exactly one retained run for the accepted request'
    }
    $verified = (Control @('wait-ssh-group-run','--window',"$groupWindow",'--tab',"$groupTab",
        '--run',"$($launched.run_id)",'--timeout','15')).run
    if ($verified.command -cne 'cli-only  west' -or $verified.recipients.Count -ne 1 -or
        $null -ne $verified.output_expectation -or $null -ne $verified.recording -or
        $verified.recipients[0].pane_id -ne $west -or $verified.recipients[0].delivery.state -cne 'sent' -or
        $verified.recipients[0].execution.exit_code -ne 17 -or
        [FixtureMessages]::GetForegroundWindow() -ne $submitForeground) {
        throw 'The CLI submission changed focus or lost its recipient, command, or outcome'
    }
    $null = Wait-Until 'both SSH panes are ready for the checked CLI run' {
        ((Roster-Snapshot).hosts | Where-Object state -ceq 'ready' | Measure-Object).Count -eq 2
    }
    $cliPattern = '^west-accepted cli-expect$'
    $cliRecording = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'cli output.txt'
    $checked = Control ($submitArgs + @('--pane',"$west",'--pane',"$east",
        '--expect-output',$cliPattern,'--regex','--match-case','--record-output',$cliRecording,'cli-expect'))
    $cliCheck = (Control @('wait-ssh-group-run','--window',"$groupWindow",'--tab',"$groupTab",
        '--run',"$($checked.run_id)",'--timeout','15')).run
    $first, $second = $cliCheck.recipients
    if ($checked.run_id -ne ($launched.run_id + 1) -or $cliCheck.recipients.Count -ne 2 -or
        $cliCheck.output_expectation.query -cne $cliPattern -or !$cliCheck.output_expectation.regex -or
        !$cliCheck.output_expectation.match_case -or $first.pane_id -ne $west -or $second.pane_id -ne $east -or
        $first.output_expectation -cne 'matched' -or $second.output_expectation -cne 'missing') {
        throw "The checked CLI submission lost its rule, recipient order, or per-pane outcomes: $($cliCheck | ConvertTo-Json -Depth 8 -Compress)"
    }
    if ($cliCheck.recording.state -cne 'saved' -or $cliCheck.recording.path -cne $cliRecording -or
        $null -ne $cliCheck.recording.error) {
        throw "The CLI run settled before its recording finished: $($cliCheck.recording | ConvertTo-Json -Compress)"
    }
    $cliLines = [IO.File]::ReadAllLines($cliRecording)
    if ("$westName${dot}west-accepted cli-expect" -cnotin $cliLines -or
        "$eastName${dot}east-accepted cli-expect" -cnotin $cliLines -or
        "$westName${dot}Sent to SSH channel${dot}Exit 17" -cnotin $cliLines -or
        "$eastName${dot}Sent to SSH channel${dot}Exit 0" -cnotin $cliLines -or
        $cliLines[-1] -cne '[Group run settled]') {
        throw 'The completed CLI transcript lost output, host outcomes, or its settlement footer'
    }
    $reportArgs = @('save-ssh-group-report','--window',"$groupWindow",'--tab',"$groupTab",'--run')
    $cliReportPath = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'CLI group report.json'
    $retiredReport = & $Cli --pid $app.Id @reportArgs 99999 $cliReportPath | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $retiredReport.ok -or $retiredReport.error -notlike '*is not retained*' -or
        [IO.File]::Exists($cliReportPath)) {
        throw 'Exporting an unknown run created a destination or gave an ambiguous reply'
    }
    $reportRun = "$($checked.run_id)"
    $savedReport = Control ($reportArgs + @($reportRun,$cliReportPath))
    $jsonText = [IO.File]::ReadAllText($cliReportPath)
    $cliDoc = $jsonText | ConvertFrom-Json
    $docWest, $docEast = $cliDoc.recipients
    if ($savedReport.window_id -ne $groupWindow -or $savedReport.tab_id -ne $groupTab -or
        $savedReport.run_id -ne $checked.run_id -or $savedReport.path -cne $cliReportPath -or
        $savedReport.format -cne 'json' -or $cliDoc.schema_version -ne 1 -or $cliDoc.submission_id -ne $checked.run_id -or
        $cliDoc.output_expectation.query -cne $cliPattern -or $docWest.pane_id -ne $west -or
        $docWest.output_expectation -cne 'matched' -or $docWest.output.state -cne 'complete' -or
        $docWest.output.text -notlike '*west-accepted cli-expect*' -or $docEast.pane_id -ne $east -or
        $docEast.output_expectation -cne 'missing' -or $docEast.output.state -cne 'complete' -or
        $docEast.output.text -notlike '*east-accepted cli-expect*') {
        throw 'The CLI JSON report lost output, recipient order, or the output check'
    }
    $conflict = & $Cli --pid $app.Id @reportArgs $reportRun --format text $cliReportPath | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $conflict.ok -or [IO.File]::ReadAllText($cliReportPath) -cne $jsonText) {
        throw 'The CLI report replaced an existing destination without permission'
    }
    $textReport = Control ($reportArgs + @($reportRun,'--format','text','--overwrite',$cliReportPath))
    $reportText = [IO.File]::ReadAllText($cliReportPath)
    if ($textReport.format -cne 'text' -or !$reportText.StartsWith("Submitted${dot}cli-expect") -or
        !$reportText.Contains('Expected output matched') -or !$reportText.Contains('Expected output missing') -or
        !$reportText.Contains('east-accepted cli-expect') -or !$reportText.Contains('west-accepted cli-expect') -or
        [FixtureMessages]::GetForegroundWindow() -ne $submitForeground) {
        throw 'The background text export lost output or moved focus'
    }
    Wait-Focus $east
    Run-Palette 'SSH host group'
    $null = Wait-Until 'background submission preserved the displayed results' {
        Find-Element "Submitted${dot}check-each-host"
    }
    $draft = (Command-Editor).Current.Value
    if ($draft -cne 'untouched command draft') { throw "The background send changed the draft to '$draft'" }
    $afterSubmit = Roster-Snapshot
    if (($afterSubmit.hosts | Where-Object shown).pane_id -ne $east -or $afterSubmit.counts.shown -ne 1 -or
        $afterSubmit.counts.checked -ne $beforeSubmit.counts.checked) {
        throw 'The background send changed the roster filter or checked hosts'
    }
    Set-HostFilter ''
    Click 'Attention inbox, 1 unread'
    Click "Unread${dot}SSH host group${dot}Run $($checked.run_id)${dot}Expected output missing on 1 host${dot}$cliPattern"
    $null = Wait-Until 'the CLI check links back to its own results in the group' {
        (Find-Element "Submitted${dot}cli-expect") -and (Find-Element "Expected output missing${dot}$eastName")
    }
    Click 'Previous submission'
    $null = Wait-Until 'return to the command submitted without a check' { Find-Element "Submitted${dot}cli-only  west" }
    Click 'Previous submission'
    $null = Wait-Until 'return to the previous UI output check' { Find-Element "Submitted${dot}check-each-host" }
    Click 'Previous submission'
    $null = Wait-Until 'move back to the earlier missing-output check' { Find-Element "Submitted${dot}check-expected" }
    Click 'Previous submission'
    $null = Wait-Until 'return to the partially delivered recording' { Find-Element "Submitted${dot}record-one" }
    Wait-RecordingStatus 'Saved live output to ' $withheldPath
    try {
        $explorer = New-Object -ComObject Shell.Application
        $priorExplorerWindows = @($explorer.Windows() | ForEach-Object { $_.HWND })
    } catch { $explorer = $null }
    Click 'Show in File Explorer'
    $trace = Wait-Until 'Shell32 receives the saved output file' {
        if ([IO.File]::Exists($env:W11TERM_TEST_EXPLORER_TRACE)) {
            [IO.File]::ReadAllText($env:W11TERM_TEST_EXPLORER_TRACE) | ConvertFrom-Json
        }
    }
    if ($trace.action -cne 'select' -or ![FixtureDialog]::SameFile($withheldPath, $trace.path)) {
        throw "File Explorer received the wrong recording: $($trace | ConvertTo-Json -Compress)"
    }
    if ($trace.hresult -ge 0) {
        $null = Wait-Until 'recording selection was acknowledged' { Find-Element 'Selected in File Explorer' }
    }
    Run-Palette 'Close tab    Ctrl+Shift+W' -Enter
    Click 'Attention inbox, 0 unread'
    $null = Wait-Until 'the saved alert remains available after closing the group tab' {
        $expiredNotice = $readOutputNotice.Replace("Run $burstRunId${dot}", "Run $burstRunId (results unavailable)${dot}")
        $entry = Find-Element $expiredNotice $button
        $entry -and !$entry.Current.IsEnabled
    }
    Click $viewMatch
    $null = Wait-Until 'the expired run shows its saved alert excerpt and disables results navigation' {
        $results = Find-Element 'Open run results' $button
        (Find-Element "Run $burstRunId${dot}Saved alert excerpt") -and
            (Find-Element "Matched output${dot}$westName${dot}west record row 00") -and
            $results -and !$results.Current.IsEnabled
    }
} catch {
    throw "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
} finally {
    if ($followReader) { $followReader.Dispose() }
    if ($followJob) {
        if (!$followJob.HasExited) { $followJob.Kill(); $followJob.WaitForExit() }
        $followJob.Dispose()
    }
    if ($outputJob) {
        if (!$outputJob.HasExited) { $outputJob.Kill(); $outputJob.WaitForExit() }
        $outputJob.Dispose()
    }
    if ($readyJob) {
        if (!$readyJob.HasExited) { $readyJob.Kill(); $readyJob.WaitForExit() }
        $readyJob.Dispose()
    }
    if ($explicitReadyJob) {
        if (!$explicitReadyJob.HasExited) { $explicitReadyJob.Kill(); $explicitReadyJob.WaitForExit() }
        $explicitReadyJob.Dispose()
    }
    if ($logReader) { $logReader.Dispose() }
    if ($explorer) {
        foreach ($item in @($explorer.Windows())) {
            try {
                $path = $item.Document.Folder.Self.Path
                if ($item.HWND -notin $priorExplorerWindows -and [IO.Directory]::Exists($path) -and
                    [FixtureDialog]::DirectoryPath($path) -eq [FixtureDialog]::DirectoryPath($env:W11TERM_EXPORT_SMOKE_DIR)) {
                    $item.Quit()
                }
            } catch { }
        }
    }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
