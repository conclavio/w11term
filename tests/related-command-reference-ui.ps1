param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$catalog = Join-Path $env:W11TERM_DATA_DIR 'command-reference/pages'
function Write-Page([string]$Platform, [string]$Name, [string]$Title, [string]$Summary, [string]$Related) {
    $dir = Join-Path $catalog $Platform
    $null = [IO.Directory]::CreateDirectory($dir)
    $markdown = "# $Title`n`n> $Summary`n"
    if ($Related) { $markdown += "> See also: $Related.`n" }
    [IO.File]::WriteAllText((Join-Path $dir "$Name.md"), $markdown)
}
Write-Page 'linux' 'a-review' 'First shipping guide' 'Audit tidal manifests.'
Write-Page 'linux' 'z-review' 'Selected shipping guide' 'Audit tidal manifests.' '`port inventory`, `unlisted helper`, `z-review`'
Write-Page 'common' 'port-inventory' 'Portable inventory guide' 'List harbor stock.' '`carrier-route`'
Write-Page 'linux' 'carrier-route' 'Linux carrier guide' 'Trace a shipping route.'
Write-Page 'common' 'carrier-route' 'Common carrier guide' 'Trace a shipping route.'
Write-Page 'windows' 'carrier-route' 'Windows carrier guide' 'Trace a shipping route.'

$edit = [System.Windows.Automation.ControlType]::Edit
$combo = [System.Windows.Automation.ControlType]::ComboBox
$link = [System.Windows.Automation.ControlType]::Hyperlink
$value = [System.Windows.Automation.ValuePattern]::Pattern
$selection = [System.Windows.Automation.SelectionPattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '
$back = 'Back to previous manual'

function Search-Value {
    $field = Find-Element 'Search command reference' $edit
    if ($field) { $field.GetCurrentPattern($value).Current.Value }
}
function Follow([string]$Command) {
    Invoke-Button (Wait-Until "offline link for '$Command'" {
        Find-Element "Open manual for $Command" $link
    })
}
function Back {
    Invoke-Button (Wait-Until 'back to the previous offline manual' { Find-Element $back $button })
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $pane = (Sessions | Where-Object label -ceq 'palette-ssh').id
    Wait-Text $pane 'related-reference-ready'
    Invoke-NativeMenuAction 'Tab menu' 'Command reference'
    $field = Wait-Until 'the command reference search field is available' { Find-Element 'Search command reference' $edit }
    $field.GetCurrentPattern($value).SetValue('tidal manifests')
    Select-NativeComboItem 'Matching commands' "Selected shipping guide${dot}Linux"
    $null = Wait-Until 'the selected manual offers its related commands' {
        (Find-Element 'Selected shipping guide') -and (Find-Element 'See also') -and
            (Find-Element 'unlisted helper') -and (Find-Element 'z-review')
    }
    if ((Find-Element 'Open manual for unlisted helper' $link) -or (Find-Element 'Open manual for z-review' $link)) {
        throw 'A missing or self-referential manual was offered as a link'
    }

    Follow 'port inventory'
    $null = Wait-Until 'the related command uses the Common fallback' {
        (Find-Element 'Portable inventory guide') -and (Find-Element "Common${dot}Custom page") -and
            (Search-Value) -ceq 'port-inventory'
    }
    Follow 'carrier-route'
    $null = Wait-Until 'navigation from a Common page retains Linux preference' {
        (Find-Element 'Linux carrier guide') -and (Find-Element "Linux${dot}Custom page") -and
            (Search-Value) -ceq 'carrier-route'
    }
    Back
    $null = Wait-Until 'the first Back restores the Common manual' {
        (Find-Element 'Portable inventory guide') -and (Search-Value) -ceq 'port-inventory'
    }
    Back
    $null = Wait-Until 'the second Back restores the exact search and its nondefault result' {
        $matching = Find-Element 'Matching commands' $combo
        if ($matching) {
            $choice = $matching.GetCurrentPattern($selection).Current.GetSelection()
            (Search-Value) -ceq 'tidal manifests' -and (Find-Element 'Selected shipping guide') -and
                $choice.Count -eq 1 -and $choice[0].Current.Name -ceq "Selected shipping guide${dot}Linux" -and
                !(Find-Element $back $button)
        }
    }
} finally {
    Close-FixtureApp $app
}
