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
    $hostStateFilter = 'Filter host state'
    Select-ComboValue $hostStateFilter 'Connecting'
    $null = Wait-Until 'the connecting filter isolates West during its shell request' {
        (Find-Element "Focus pane $west" $button) -and !(Find-Element "Focus pane $east" $button) -and
            !(Find-Element "Reconnect SSH pane $west" $button)
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
    Submit 'one' 'Send to 1 host'
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
            (Find-Element "Newest first${dot}0 of 2 retained lines") -and !(Find-Element 'east-accepted both')
    }
    Click 'Pause output'
    $null = Wait-Until 'output is paused' { Find-Element 'Resume output' $button }
    [IO.File]::WriteAllText((Join-Path $State 'release-west-paused'), '')
    $null = Control @('wait-for-text','--pane',"$west",'--timeout','15','west while paused')
    $null = Wait-Until 'the recorder keeps writing while the display is paused' {
        [Text.Encoding]::UTF8.GetString((Read-PeerTrace $logReader)).Contains("$westName$($dot)west while paused")
    }
    (Command-Editor $outputSearch).SetValue('WHILE')
    $until = [DateTime]::UtcNow.AddMilliseconds(650)
    do {
        if ((Find-Element 'west while paused') -or
            !(Find-Element "Paused${dot}newest first${dot}0 of 2 retained lines")) {
            throw 'Filtered output changed while paused'
        }
        Start-Sleep -Milliseconds 120
    } while ([DateTime]::UtcNow -lt $until)
    Click 'Resume output'
    $null = Wait-Until 'matching output received while paused is available on resume' {
        (Find-Element "Newest first${dot}1 of 3 retained lines") -and
            (Find-Element 'west while paused') -and !(Find-Element 'west live first')
    }
    Click "Focus output pane $west"
    Wait-Focus $west
    [IO.File]::WriteAllText((Join-Path $State 'release-west-away'), '')
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
    Click 'Clear output filters'
    $null = Wait-Until 'clearing the filters restores all retained output' {
        (Find-Element 'east-accepted both') -and (Find-Element 'west live first') -and
            (Find-Element 'Newest first')
    }
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
    Click 'Previous submission'
    $null = Wait-Until 'earlier submission' { Find-Element "Submitted${dot}both" }
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
    Click 'Next submission'
    $null = Wait-Until 'later submission restored' { Find-Element "Submitted${dot}while-slow" }
    Click 'Attention inbox, 1 unread'
    Click "Unread${dot}SSH host group${dot}Run 2${dot}Group run finished${dot}1 succeeded${dot}1 needs attention"
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
    Select-ComboValue $hostStateFilter 'Ready'
    Send-Raw $west 'prompt'
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
    $nextName = "ssh-group-output-$([int]$Matches[1] + 1).txt"
    $burstPath = $newLogs[0].FullName
    $logReader = Open-PeerTraceReader $burstPath
    $null = Wait-Until 'the recorder is still open with West awaiting its result' {
        $captured = [Text.Encoding]::UTF8.GetString((Read-PeerTrace $logReader))
        $captured.Contains("$eastName$($dot)east record row 79") -and !$captured.Contains('[Group run settled]')
    }
    Click "Focus pane $west"
    Wait-Focus $west
    [IO.File]::WriteAllText((Join-Path $State 'release-record-fast'), '')
    $null = Wait-Until 'the recording ends automatically with the terminal tab focused' {
        [Text.Encoding]::UTF8.GetString((Read-PeerTrace $logReader)).Contains('[Group run settled]')
    }
    $logReader.Dispose()
    $logReader = $null
    Run-Palette 'SSH host group'
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
} catch {
    throw "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
} finally {
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
