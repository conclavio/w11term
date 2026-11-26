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
$ellipsis = [char]0x2026
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Search-Control {
    Wait-Until 'command history search field' { Find-Element 'Search command history' $edit }
}

function Wait-ForHistoryText([string]$Description, [scriptblock]$Condition) {
    try { $null = Wait-Until $Description $Condition } catch {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $names = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition) |
            ForEach-Object { $_.Current.Name } | Where-Object { $_ }
        throw "$_ Elements: $($names -join ' | ')"
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"History before"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $before = ((Control @('list')).sessions | Where-Object label -eq 'History before').id
    $null = Control @('wait-for-text','--pane',"$before",'--timeout','15','capture-ready')
    $beforeCommand = (Control @('list-commands','--pane',"$before")).commands[0].id
    Run-Palette 'Command history'
    Click 'Use output as reference'
    $referenceHeading = "Reference${dot}History before${dot}Pane $before${dot}Capture $beforeCommand${dot}exit 5${dot}check result"
    Wait-ForHistoryText "the selected earlier capture becomes '$referenceHeading'" { Find-Element $referenceHeading }

    $after = (Control @('open','--profile','History after','--activate')).pane
    Wait-Focus $after
    $null = Control @('wait-for-text','--pane',"$after",'--timeout','15','capture-ready')
    $afterCommand = (Control @('list-commands','--pane',"$after")).commands[0].id
    Run-Palette 'Command history'
    $value = (Search-Control).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    $value.SetValue('check result')
    Click 'Compare with reference'
    $selectedHeading = "Selected${dot}History after${dot}Pane $after${dot}Capture $afterCommand${dot}exit 0${dot}check result"
    $removed = "Removed line 2${dot}old result"
    $added = "Added line 2${dot}new result"
    Wait-ForHistoryText 'the changed lines and exact capture IDs are shown' {
        (Find-Element 'Command output differences') -and (Find-Element $referenceHeading) -and
            (Find-Element $selectedHeading) -and (Find-Element $removed) -and (Find-Element $added)
    }
    Click "Save full differences$ellipsis"
    $dialog = Wait-Until 'owned command differences save dialog' { Find-OwnedDialog 'Save command output differences' }
    $null = Wait-Until 'publish the full diff' { [FixtureDialog]::ClickButton($dialog, 1) }
    $path = Join-Path $State "command-diff-$before-$beforeCommand-to-$after-$afterCommand.txt"
    $null = Wait-Until 'published diff file' { [IO.File]::Exists($path) }
    $lines = [IO.File]::ReadAllLines($path)
    if ($lines.Count -ne 4 -or $lines[0] -cne $referenceHeading -or $lines[1] -cne $selectedHeading -or
        $lines[2] -cne $removed -or $lines[3] -cne $added) {
        throw "Unexpected exported differences: $($lines -join ' | ')"
    }

    Click 'Back to command history'
    $null = Wait-Until 'the same search and selection are restored' {
        $field = Find-Element 'Search command history' $edit
        $field -and $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq 'check result' -and
            (Find-Element "Selected, check result${dot}exit 0" $button)
    }
    $null = Control @('close-pane','--pane',"$before")
    $null = Wait-Until 'closing the source invalidates its reference' { Find-Element 'The reference capture is no longer retained.' }
    $expired = Find-Element 'Compare with reference' $button
    if ($expired.Current.IsEnabled) { throw 'An expired reference was still comparable' }
    Click 'Clear reference'
    $null = Wait-Until 'clear the expired reference' { !(Find-Element 'The reference capture is no longer retained.') }
} finally {
    Close-FixtureApp $app
}
