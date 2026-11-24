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
function Set-Command([string]$Command) {
    $field = Wait-Until 'group command' { Find-Element 'Group command' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Command)
}
function Select-GroupOption([string]$Name, [string]$Label) {
    $combo = [System.Windows.Automation.ControlType]::ComboBox
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $field = Wait-Until "enabled '$Name'" { $entry = Find-Element $Name $combo; if ($entry -and $entry.Current.IsEnabled) { $entry } }
    $popup = $field.GetCurrentPattern($expand)
    $popup.Expand()
    $option = Wait-Until "visible option '$Label'" {
        Find-Element $Label ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $option.GetCurrentPattern($item).Select()
    $null = Wait-Until "'$Name' selected '$Label'" {
        $entry = Find-Element $Name $combo
        $selected = $entry.GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
        $selected.Count -eq 1 -and $selected[0].Current.Name -ceq $Label
    }
    $popup = (Find-Element $Name $combo).GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}

$launch = @('--profiles', ('"' + $profiles + '"'), '--demo', '--cols', '100', '--rows', '36')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    Run-Palette "Open SSH group${dot}Fleet" -Enter
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
    New-Item -ItemType File -Path (Join-Path $State 'release-roll') | Out-Null
    $null = Control @('wait-for-text','--pane',"$($panes.East)",'--timeout','10','East started roll')
    $tab = Wait-Until 'the group tab' { Find-Element $group ([System.Windows.Automation.ControlType]::TabItem) }
    $tab.GetCurrentPattern($item).Select()
    $null = Wait-Until 'North was tried before East despite the new roster sort' {
        Find-Element "$($labels.North)${dot}Not sent${dot}The verified prompt changed before the command was sent."
    }
    New-Item -ItemType File -Path (Join-Path $State 'release-east-roll') | Out-Null
    $null = Control @('wait-for-text','--pane',"$($panes.East)",'--timeout','10','East finished roll')
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
    Click 'Cancel queued commands'
    $null = Wait-Until 'East was canceled before sending' {
        Find-Element "$($labels.East)${dot}Not sent${dot}Cancelled before sending."
    }
    New-Item -ItemType File -Path (Join-Path $State 'release-keep-first') | Out-Null
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West finished keep-first')
    $log = Join-Path $State 'ssh-group-output-2.txt'
    $null = Wait-Until 'the recording includes the cancelled delivery' {
        try {
            $stream = [IO.File]::Open($log, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
        } catch [IO.FileNotFoundException] {
            return
        }
        $reader = [IO.StreamReader]::new($stream)
        try { $reader.ReadToEnd().Contains('Cancelled before sending.') } finally { $reader.Dispose() }
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

    Set-Command 'inspect'
    Click 'Send to 2 hosts'
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started inspect')
    Click 'Pause queue'
    $null = Wait-Until 'the next run is paused' { Find-Element 'Command queue paused' }
    Click 'Attention inbox, 0 unread'
    $null = Wait-Until 'the paused group is in the background' { Find-Element 'No bells or notifications' }
    New-Item -ItemType File -Path (Join-Path $State 'release-inspect') | Out-Null
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West finished inspect')
    $tab = Wait-Until 'the group tab after pausing' { Find-Element $group ([System.Windows.Automation.ControlType]::TabItem) }
    $tab.GetCurrentPattern($item).Select()
    $null = Wait-Until 'the queue remains paused with a free slot' {
        (Find-Element 'Command queue paused') -and (Find-Element "$($labels.West)${dot}Exit 0") -and
            (Find-Element "$($labels.East)${dot}Queued")
    }
    New-Item -ItemType File -Path (Join-Path $State 'resume-inspect') | Out-Null
    Click 'Resume queue'
    $null = Wait-Until 'the queue resumed' { !(Find-Element 'Command queue paused') }
    $null = Wait-Until 'East was sent the resumed command' {
        Find-Element "$($labels.East)${dot}Sent to SSH channel"
    }
    $null = Control @('wait-for-text','--pane',"$($panes.East)",'--timeout','10','East finished inspect')

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
    New-Item -ItemType File -Path (Join-Path $State 'release-east-canary-ok') | Out-Null
    $canaryNotice = "$group${dot}Run 5${dot}Canary ready for approval${dot}$($labels.East)"
    Click "Unread${dot}$canaryNotice"
    $null = Wait-Until 'the canary notice opens the run requiring approval' {
        (Find-Element "$($labels.East)${dot}Exit 0") -and
            (Find-Element "$($labels.West)${dot}Queued") -and
            (Find-Element "$($labels.East)${dot}Canary succeeded. Review its result before continuing.") -and
            (Find-Element 'Approve remaining hosts' $button)
    }
    New-Item -ItemType File -Path (Join-Path $State 'release-canary-ok') | Out-Null
    New-Item -ItemType File -Path (Join-Path $State 'approve-canary-ok') | Out-Null
    Click 'Approve remaining hosts'
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West finished canary-ok')
    Click 'Attention inbox, 1 unread'
    $null = Wait-Until 'the canary notice stays read while the run completion is unread' {
        Find-Element "Read${dot}$canaryNotice" $button
    }
    Click "Unread${dot}$group${dot}Run 5${dot}Group run finished${dot}2 succeeded${dot}0 need attention"

    Set-Command 'canary-fails'
    Click 'Run canary first'
    Click 'Attention inbox, 1 unread'
    Click "Unread${dot}$group${dot}Run 6${dot}Canary did not report a successful exit${dot}$($labels.East)"
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
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
