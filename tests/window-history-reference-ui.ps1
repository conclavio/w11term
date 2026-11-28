param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

foreach ($platform in 'linux','windows') {
    $pages = Join-Path $env:W11TERM_DATA_DIR "command-reference/pages/$platform"
    $null = [IO.Directory]::CreateDirectory($pages)
    $label = if ($platform -ceq 'linux') { 'Linux' } else { 'Windows' }
    [IO.File]::WriteAllText((Join-Path $pages 'git-log.md'), "# $label log history manual`n`n> Review Git history.`n")
}

$edit = [System.Windows.Automation.ControlType]::Edit
$combo = [System.Windows.Automation.ControlType]::ComboBox
$value = [System.Windows.Automation.ValuePattern]::Pattern
$selection = [System.Windows.Automation.SelectionPattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '
$find = 'Find command history' + [char]0x2026
$manual = 'Look up command manual'
$query = 'audit-token'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $button -Visible
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}

function Picker-Search { Find-Element 'Search commands across panes' $edit }

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $remote = (Sessions | Where-Object label -ceq 'palette-ssh').id
    Wait-Text $remote 'window-history-reference-ready'
    $local = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $local
    Wait-Text $local 'PS '

    Press-Key 0x76
    $palette = Wait-Until 'command palette search field' { Find-Element 'Search commands' $edit }
    $palette.GetCurrentPattern($value).SetValue($find)
    Click $find
    $field = Wait-Until 'window command history search' { Picker-Search }
    $field.GetCurrentPattern($value).SetValue('missing-retained-command')
    $null = Wait-Until 'no selection offers a manual with no matching history' {
        $lookup = Find-Element $manual $button
        (Find-Element 'No matching commands in this window') -and $lookup -and !$lookup.Current.IsEnabled
    }

    (Wait-CheckBox 'Include retained command output' ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    Select-NativeComboItem 'Window command status' 'Failed'
    (Picker-Search).GetCurrentPattern($value).SetValue($query)
    $ssh = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.'palette-ssh'
    $source = "palette-ssh${dot}$($ssh.user)@$($ssh.host):$($ssh.port)${dot}Pane $remote"
    $newer = "latest output audit-token${dot}${source}${dot}exit 2${dot}git status --short"
    $older = "earlier output audit-token${dot}${source}${dot}exit 5${dot}git log -5"
    $success = "successful output audit-token${dot}${source}${dot}exit 0${dot}git diff"
    $null = Wait-Until 'both output matches are shown in newest-first order' {
        $first = Find-Element $newer $button -Visible
        $second = Find-Element $older $button -Visible
        $first -and $second -and $first.Current.BoundingRectangle.Top -lt $second.Current.BoundingRectangle.Top -and
            !(Find-Element $success $button)
    }
    (Picker-Search).SetFocus()
    Wait-PickerEditor 'Search commands across panes' 'focus filtered history'
    Press-Key 0x28
    Click $manual

    $selected = "Selected, git log -5${dot}exit 5"
    $null = Wait-Until 'the highlighted SSH result opens with its Linux manual and captured output' {
        !(Picker-Search) -and (Find-Element "Command history${dot}Pane $remote") -and
            (Find-Element $selected $button) -and (Find-Element 'Linux log history manual') -and
            (Get-NativeTextLabels | Where-Object { $_.StartsWith('earlier output audit-token') })
    }
    if (Find-Element 'Windows log history manual') { throw 'The lookup used the PowerShell origin platform' }
    Click 'Close command reference'
    Click 'Back to search results'
    Wait-Focus $local
    $null = Wait-Until 'the same search keeps its query, filters, and both results' {
        $field = Picker-Search
        $status = Find-Element 'Window command status' $combo
        if ($field -and $status) {
            $choice = $status.GetCurrentPattern($selection).Current.GetSelection()
            $field.GetCurrentPattern($value).Current.Value -ceq $query -and $choice[0].Current.Name -ceq 'Failed' -and
                (Find-Element $newer $button -Visible) -and (Find-Element $older $button -Visible) -and
                !(Find-Element $success $button)
        }
    }
    $null = Wait-CheckBox 'Include retained command output' ([System.Windows.Automation.ToggleState]::On)
    Wait-PickerEditor 'Search commands across panes' 'focus restored history'
    Press-Key 0x0d
    $null = Wait-Until 'Enter opens the same restored capture with no further selection' {
        !(Picker-Search) -and (Find-Element $selected $button)
    }
} finally {
    Close-FixtureApp $app
}
