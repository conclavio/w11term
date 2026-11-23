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
function Command-Editor {
    Wait-Until 'group command editor' {
        $element = Find-Element 'Group command' $edit
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
    Submit 'while-slow' 'Send to 2 hosts'
    Outcome $eastName 'Exit 0'
    Outcome $westName "Not sent${dot}Waiting for a fresh prompt"
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
    $null = Wait-Until 'distinct West output in the earlier submission' { Find-Element 'west-accepted both' }
    Click 'Copy results'
    $null = Wait-Until 'the earlier report contains its own outputs and exit statuses' {
        $report = Read-ClipboardFixture
        $report -and $report.StartsWith("Submitted${dot}both") -and
            $report.Contains("  Exit 0") -and $report.Contains('east-accepted both') -and
            $report.Contains("  Exit 17") -and $report.Contains('west-accepted both') -and
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
    Set-HostFilter ''
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On
    Click "Focus result pane $west"
    Wait-Focus $west
} catch {
    throw "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
