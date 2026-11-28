param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

foreach ($platform in 'linux','windows') {
    $pages = Join-Path $env:W11TERM_DATA_DIR "command-reference/pages/$platform"
    $null = [IO.Directory]::CreateDirectory($pages)
    $title = if ($platform -ceq 'linux') { 'Linux audit log manual' } else { 'Windows Git log manual' }
    [IO.File]::WriteAllText((Join-Path $pages 'git-log.md'), "# $title`n`n> Review Git history.`n")
}
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '
$recorded = 'sudo -n /usr/bin/git log -5 --review-token sample-word'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $button
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}

function Reference-Query {
    $field = Find-Element 'Search command reference' $edit
    if ($field) { $field.GetCurrentPattern($value).Current.Value }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $remote = (Sessions | Where-Object label -eq 'palette-ssh').id
    Wait-Text $remote 'history-reference-ready'
    Press-Key 0x76
    $palette = Wait-Until 'command palette search field' { Find-Element 'Search commands' $edit }
    $palette.GetCurrentPattern($value).SetValue('Command history')
    Click 'Command history'
    $selected = "Selected, $recorded${dot}exit 0"
    $null = Wait-Until 'the recorded invocation is selected in history' { Find-Element $selected $button }
    Click 'Look up command manual'
    $null = Wait-Until 'the Linux subcommand page opens beside its history entry' {
        (Find-Element 'Linux audit log manual') -and
            (Find-Element "Linux${dot}Custom page") -and
            (Find-Element "Command history${dot}Pane $remote") -and
            (Find-Element $selected $button) -and
            (Get-NativeTextLabels | Where-Object { $_.StartsWith('retained git output') }) -and
            (Reference-Query) -ceq 'git-log'
    }
    if (Find-Element 'Windows Git log manual') { throw 'The lookup used the wrong platform' }

    $historySearch = Wait-Until 'the command history filter is still available' {
        Find-Element 'Search command history' $edit
    }
    $historySearch.GetCurrentPattern($value).SetValue('custom-report')
    $null = Wait-Until 'a command without a manual is selected' {
        Find-Element "Selected, custom-report report-argument${dot}exit 0" $button
    }
    Click 'Look up command manual'
    $null = Wait-Until 'a missing manual leaves the executable ready for search' {
        (Find-Element 'No matching command pages') -and (Reference-Query) -ceq 'custom-report'
    }
    Click 'Close command reference'
    $null = Wait-Until 'the history selection remains after closing the manual' {
        (Find-Element "Selected, custom-report report-argument${dot}exit 0" $button) -and
            !(Find-Element 'Search command reference' $edit)
    }
} finally {
    Close-FixtureApp $app
}
