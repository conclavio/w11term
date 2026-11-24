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
function Set-OneAtATime {
    $combo = [System.Windows.Automation.ControlType]::ComboBox
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $field = Wait-Until 'parallel command limit' { Find-Element 'Parallel SSH commands' $combo }
    $popup = $field.GetCurrentPattern($expand)
    $popup.Expand()
    $option = Wait-Until 'visible one-at-a-time limit' {
        Find-Element '1 at a time' ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $option.GetCurrentPattern($item).Select()
    $null = Wait-Until 'one-at-a-time command limit selected' {
        $entry = Find-Element 'Parallel SSH commands' $combo
        $selected = $entry.GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
        $selected.Count -eq 1 -and $selected[0].Current.Name -ceq '1 at a time'
    }
    $popup = (Find-Element 'Parallel SSH commands' $combo).GetCurrentPattern($expand)
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
    Set-OneAtATime
    Set-Command 'roll'
    Click 'Send to 3 hosts'
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started roll')
    $null = Wait-Until 'East and North are queued' {
        (Find-Element "$($labels.East)${dot}Queued") -and (Find-Element "$($labels.North)${dot}Queued")
    }
    $null = Control @('send-text','--pane',"$($panes.North)",'--no-paste','x')
    Click 'Attention inbox, 0 unread'
    $null = Wait-Until 'the group is in the background' { Find-Element 'No bells or notifications' }
    New-Item -ItemType File -Path (Join-Path $State 'release-roll') | Out-Null
    $null = Control @('wait-for-text','--pane',"$($panes.East)",'--timeout','10','East finished roll')
    $tab = Wait-Until 'the group tab' { Find-Element $group ([System.Windows.Automation.ControlType]::TabItem) }
    $tab.GetCurrentPattern($item).Select()
    $null = Wait-Until 'the stale queued prompt was withheld' {
        Find-Element "$($labels.North)${dot}Not sent${dot}The verified prompt changed before the command was sent."
    }

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
        [IO.File]::Exists($log) -and [IO.File]::ReadAllText($log).Contains('Cancelled before sending.')
    }
    (Wait-CheckBox 'Stop queue on failure' Off).Toggle()
    $null = Wait-CheckBox 'Stop queue on failure' On
    Set-Command 'halt-on-failure'
    Click 'Send to 2 hosts'
    $null = Control @('wait-for-text','--pane',"$($panes.West)",'--timeout','10','West started halt-on-failure')
    $null = Wait-Until 'East is queued until West settles' {
        Find-Element "$($labels.East)${dot}Queued"
    }
    New-Item -ItemType File -Path (Join-Path $State 'release-halt-on-failure') | Out-Null
    $null = Wait-Until 'the failure withholds the remaining command' {
        Find-Element "$($labels.East)${dot}Not sent${dot}Queue stopped after an unsuccessful host."
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
