[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_TEST_COPY_FILE = Join-Path $State 'group-result-clipboard.txt'
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
function Result-FilterPopup([string]$Expected) {
    Wait-Until "submission result filter '$Expected'" {
        $entry = Find-Element 'Filter submission results' ([System.Windows.Automation.ControlType]::ComboBox)
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
function Set-ResultFilter([string]$Name) {
    (Result-FilterPopup '').Expand()
    $choice = Wait-Until "result filter '$Name'" {
        $entry = Find-Element $Name ([System.Windows.Automation.ControlType]::ListItem)
        $provider = $null
        if ($entry -and $entry.Current.IsEnabled -and !$entry.Current.IsOffscreen -and
            $entry.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$provider)) {
            $provider
        }
    }
    $choice.Select()
    $popup = Result-FilterPopup $Name
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
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

$launchArgs = @('--profiles',('"' + $profiles + '"'),'--profile','East',
    '--tab','West','--tab','"Local witness"','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $launchArgs -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
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
    $null = Control @('wait-for-text','--pane',"$west",'--timeout','15','west$')
    Press-Key 0x77 # F8 selects the East tab in the fixture keymap.
    Wait-Focus $east
    Press-Key 0x10 # An unencoded Shift must not consume the verified prompt.
    Run-Palette 'SSH host group'
    Pane-Status $east "Connected${dot}Ready"
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
    $eastAddress = "EaST 127.0.0.1:$($profileData.East.port)"
    Set-HostFilter $eastAddress
    $null = Wait-Until 'only East is shown' {
        (Find-Element "Focus pane $east" $button) -and !(Find-Element "Focus pane $west" $button)
    }
    $null = Wait-Until 'the hidden selection is disclosed' {
        Find-Element '1 selected host is hidden by the filter.'
    }
    Submit 'one' 'Send to 1 host'
    Outcome $eastName 'Sent to SSH channel'
    Pane-Status $east "Connected${dot}Ready"
    Click 'Deselect shown'
    $null = Wait-CheckBox $eastName Off
    Set-HostFilter ''
    $null = Wait-CheckBox $westName On
    $null = Wait-CheckBox $eastName Off
    Click 'Select shown'
    $null = Wait-CheckBox $eastName On
    Submit 'both' 'Send to 2 hosts'
    Outcome $eastName 'Sent to SSH channel'
    Outcome $westName 'Sent to SSH channel'
    Outcome $eastName 'Exit 0'
    Outcome $westName 'Running'
    $null = Wait-Until 'East output in the group' { Find-Element 'east-accepted both' }
    Result-Progress "1 of 2 settled${dot}0 waiting${dot}1 running${dot}1 succeeded${dot}0 need attention"
    $liveButton = 'Live output' + [char]0x2026
    Click $liveButton
    $null = Wait-Until 'completed East output in the live feed' {
        (Find-Element 'SSH group live output') -and
            (Find-Element 'east-accepted both') -and (Find-Element "Focus output pane $east" $button)
    }
    [IO.File]::WriteAllText((Join-Path $State 'release-west-preview'), '')
    $null = Wait-Until 'running West output appears above the earlier East output' {
        $westLine = Find-Element 'west live first'
        $eastLine = Find-Element 'east-accepted both'
        $westLine -and $eastLine -and $westLine.Current.BoundingRectangle.Top -lt $eastLine.Current.BoundingRectangle.Top
    }
    Click 'Pause output'
    $null = Wait-Until 'output is paused' { Find-Element 'Resume output' $button }
    [IO.File]::WriteAllText((Join-Path $State 'release-west-paused'), '')
    $null = Control @('wait-for-text','--pane',"$west",'--timeout','15','west while paused')
    $until = [DateTime]::UtcNow.AddMilliseconds(650)
    do {
        if (Find-Element 'west while paused') { throw 'Output changed while paused' }
        Start-Sleep -Milliseconds 120
    } while ([DateTime]::UtcNow -lt $until)
    Click 'Resume output'
    $null = Wait-Until 'output received while paused is available on resume' { Find-Element 'west while paused' }
    Click "Focus output pane $west"
    Wait-Focus $west
    Run-Palette 'SSH host group'
    $null = Wait-Until 'the same output view returns after focusing its source pane' {
        (Find-Element 'SSH group live output') -and (Find-Element 'west while paused')
    }
    Click 'Back to results'
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
    Result-Progress "2 of 2 settled${dot}0 waiting${dot}0 running${dot}1 succeeded${dot}1 needs attention"
    Set-HostFilter 'east'
    $null = Wait-Until 'West has left the roster' { !(Find-Element "Focus pane $west" $button) }
    $null = Wait-Until 'the full East output is retained' { Find-Element "View output for pane $east" $button }
    Click 'Copy results'
    $null = Wait-Until 'the current report contains full output and both host outcomes' {
        $report = Read-ClipboardFixture
        $report -and $report.StartsWith("Submitted${dot}while-slow") -and
            $report.Contains("$eastName${dot}Sent to SSH channel") -and
            $report.Contains("  Exit 0") -and $report.Contains("east-report-tail") -and
            $report.Contains("$westName${dot}Not sent${dot}Waiting for a fresh prompt")
    }
    $null = Wait-Until 'copy confirmation' { Find-Element 'Copied results' }
    Set-HostFilter ''
    Click 'Previous submission'
    $null = Wait-Until 'earlier submission' { Find-Element "Submitted${dot}both" }
    Outcome $westName 'Running'
    [IO.File]::WriteAllText((Join-Path $State 'release-west-result'), '')
    Outcome $westName 'Exit 17'
    Click $liveButton
    $null = Wait-Until 'completed West output in the earlier live feed' {
        (Find-Element 'west-accepted both') -and (Find-Element 'west while paused')
    }
    Click 'Back to results'
    Result-Progress "2 of 2 settled${dot}0 waiting${dot}0 running${dot}1 succeeded${dot}1 needs attention"
    Set-ResultFilter 'Needs attention'
    $null = Wait-Until 'show only the failed result' {
        (Find-Element "Focus result pane $west" $button) -and !(Find-Element "Focus result pane $east" $button)
    }
    Click 'Copy results'
    $null = Wait-Until 'the earlier report contains its own outputs and exit statuses' {
        $report = Read-ClipboardFixture
        $report -and $report.StartsWith("Submitted${dot}both") -and
            $report.Contains("  Exit 0") -and $report.Contains('east-accepted both') -and
            $report.Contains("  Exit 17") -and $report.Contains('west-accepted both') -and
            $report.Contains('west while paused') -and
            !$report.Contains('east-report-tail')
    }
    Click "View output for pane $west"
    $null = Wait-Until 'full West output opens the matching command history entry' {
        Find-Element "Selected, both${dot}exit 17" $button
    }
    Run-Palette 'SSH host group'
    Outcome $westName 'Exit 17'
    Click 'Next submission'
    $null = Wait-Until 'later submission restored' { Find-Element "Submitted${dot}while-slow" }
    Send-Raw $west 'part'
    Pane-Status $west "Connected${dot}Waiting for a fresh prompt"
    Submit 'while-edit' 'Send to 2 hosts'
    Outcome $eastName 'Sent to SSH channel'
    Outcome $westName "Not sent${dot}Waiting for a fresh prompt"
    Send-Raw $west 'alt'
    Pane-Status $west "Connected${dot}Alternate screen"
    Submit 'while-alt' 'Send to 2 hosts'
    Outcome $eastName 'Sent to SSH channel'
    Outcome $westName "Not sent${dot}Alternate screen"
    Send-Raw $west 'prompt'
    Pane-Status $west "Connected${dot}Ready"
    $previous = (Control @('inspect','--pane',"$west")).connection_generation
    Send-Raw $west 'drop'
    $null = Wait-Until 'replacement SSH transport' {
        (Control @('inspect','--pane',"$west")).connection_generation -gt $previous
    }
    Pane-Status $west "Connected${dot}Prompt unverified"
    Submit 'while-reconnecting' 'Send to 2 hosts'
    Outcome $eastName 'Sent to SSH channel'
    Outcome $westName "Not sent${dot}Prompt unverified"
    Send-Raw $west 'prompt'
    Pane-Status $west "Connected${dot}Ready"
    Submit 'after-reconnect' 'Send to 2 hosts'
    Outcome $eastName 'Exit 0'
    Outcome $westName 'Exit 17'
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
    Click 'Interrupt selected'
    Outcome $westName 'Ctrl+C sent'
    Outcome $westName 'Exit 130'
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
    (Wait-CheckBox $eastName Off).Toggle()
    $null = Wait-CheckBox $eastName On
    Click 'Interrupt selected'
    Outcome $eastName 'Ctrl+C sent'
    Outcome $eastName 'Exit 130'
    Submit 'interrupt-both' 'Send to 2 hosts'
    Outcome $eastName 'Running'
    Outcome $westName 'Running'
    Click 'Interrupt selected'
    Outcome $eastName 'Exit 130'
    Outcome $westName 'Exit 130'
    Click 'Previous submission'
    $null = Wait-Until 'show the previous interrupt' { Find-Element "Submitted${dot}interrupt-pair" }
    Click 'Previous submission'
    $null = Wait-Until 'show the command to load' { Find-Element "Submitted${dot}after-reconnect" }
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
    Click "Reconnect SSH pane $west"
    Pane-Status $west "Connected${dot}Ready"
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
} catch {
    throw "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
