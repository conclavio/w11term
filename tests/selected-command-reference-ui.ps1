param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$catalog = Join-Path $env:W11TERM_DATA_DIR 'command-reference/pages'
foreach ($platform in 'linux','windows') {
    $pages = Join-Path $catalog $platform
    $null = [IO.Directory]::CreateDirectory($pages)
    $label = if ($platform -ceq 'linux') { 'Linux' } else { 'Windows' }
    [IO.File]::WriteAllText((Join-Path $pages 'git-log.md'), "# $label Git review manual`n`n> Review Git history.`n")
    [IO.File]::WriteAllText((Join-Path $pages 'fleet-report.md'), "# $label fleet runbook`n`n> Investigate a fleet outage.`n")
}

$edit = [System.Windows.Automation.ControlType]::Edit
$document = [System.Windows.Automation.ControlType]::Document
$value = [System.Windows.Automation.ValuePattern]::Pattern
$textPattern = [System.Windows.Automation.TextPattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '
$command = 'sudo -n /usr/bin/git log -5 --note=' + ('r' * 108) + '/end'

function Terminal-Text {
    $surface = Find-Element "Terminal${dot}palette-ssh" $document
    if ($surface) { $surface.GetCurrentPattern($textPattern) }
}

function Lookup-Selection([string]$Text) {
    $selection = Wait-Until "select terminal text '$Text'" {
        $provider = Terminal-Text
        if ($provider) { $provider.DocumentRange.FindText($Text, $false, $false) }
    }
    $selection.Select()
    $null = Wait-Until 'the terminal has applied the selection' {
        $provider = Terminal-Text
        if ($provider) {
            (@($provider.GetSelection() | ForEach-Object { $_.GetText(-1) }) -join '') -ceq $Text
        }
    }
    Invoke-NativeMenuAction 'Tab menu' 'Look up selected command'
}

function Reference-Query {
    $field = Find-Element 'Search command reference' $edit
    if ($field) { $field.GetCurrentPattern($value).Current.Value }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile','palette-ssh','--cols','120') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $remote = (Sessions | Where-Object label -eq 'palette-ssh').id
    Wait-Text $remote 'selection-reference-ready'

    Lookup-Selection $command
    $null = Wait-Until 'the full SSH selection resolves to the Linux subcommand manual' {
        (Find-Element 'Linux Git review manual') -and (Reference-Query) -ceq 'git-log'
    }

    Select-NativeComboItem 'Command reference platform' 'Windows'
    $field = Find-Element 'Search command reference' $edit
    $field.GetCurrentPattern($value).SetValue('fleet outage')
    $null = Wait-Until 'the manual platform override is in use' {
        (Find-Element 'Windows fleet runbook') -and (Reference-Query) -ceq 'fleet outage'
    }

    Lookup-Selection $command
    $null = Wait-Until 'a new selected lookup respects the explicit Windows override' {
        (Find-Element 'Windows Git review manual') -and (Reference-Query) -ceq 'git-log'
    }

    Lookup-Selection 'fleet outage'
    $null = Wait-Until 'selected task text still searches the chosen platform' {
        (Find-Element 'Windows fleet runbook') -and (Reference-Query) -ceq 'fleet outage'
    }
} finally {
    Close-FixtureApp $app
}
