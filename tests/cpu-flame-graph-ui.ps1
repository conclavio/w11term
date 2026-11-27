[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$folded = Join-Path $State 'worker profile.folded'
$after = Join-Path $State 'after recording.folded'
$svgPath = Join-Path $State 'hot branch.svg'
$dot = ' ' + [char]0xb7 + ' '
$arrow = ' ' + [char]0x2192 + ' '
$callerArrow = ' ' + [char]0x2190 + ' '

function Click-GraphButton([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $localPane = ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id
    $remotePane = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $remotePane
    $null = Control @('wait-ssh-connected','--pane',"$remotePane",'--timeout','15')
    $sampling = @('--pane',"$remotePane",'--target-pid','42','--seconds','2')
    $cpuPlan = (Control (@('preview-cpu-profile') + $sampling)).plan
    $captured = Control (@('capture-cpu-profile') + $sampling + @(
        '--connection-generation',"$($cpuPlan.connection_generation)",
        '--process-start-ticks',"$($cpuPlan.process_start_ticks)",'--output-file',$folded
    ))
    if ($captured.summary.samples -ne 4) { throw 'Wrong CPU sample total returned by CLI' }
    $null = Control @('close-pane','--pane',"$remotePane")
    Wait-Focus $localPane

    Run-Palette 'CPU flame graph'
    Click-GraphButton 'Open folded stacks'
    $dialog = Wait-Until 'owned folded stacks dialog' { Find-OwnedDialog 'Open folded stacks' }
    $null = Wait-Until 'enter the folded stacks path' { [FixtureDialog]::SetFilename($dialog, $folded, '') }
    $null = Wait-Until 'open the profile' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'sample total with the SSH pane closed' { Find-Element '4 samples' }
    $edit = Wait-Until 'flame graph symbol search' {
        Find-Element 'Search flame graph symbols' ([System.Windows.Automation.ControlType]::Edit)
    }
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('HOT_LOOP')
    $null = Wait-Until 'weighted search coverage' { Find-Element ('1 matching frame' + $dot + '75.0% of samples') }
    Click-GraphButton 'Next symbol match'
    $null = Wait-Until 'hot frame has the expected caller and share' {
        (Find-Element 'Zoomed to hot_loop') -and
        (Find-Element ('hot_loop' + $dot + '3 samples' + $dot + '75.0% of capture')) -and
        (Find-Element 'Caller main')
    }
    Click-GraphButton 'Zoom to caller'
    $null = Wait-Until 'move up to the sampled caller' { Find-Element 'Zoomed to main' }
    Click-GraphButton 'Export SVG'
    $dialog = Wait-Until 'owned flame graph export dialog' { Find-OwnedDialog 'Export CPU flame graph' }
    $null = Wait-Until 'enter the SVG destination' {
        [FixtureDialog]::SetFilename($dialog, $svgPath, 'cpu-flame-graph.svg')
    }
    $null = Wait-Until 'save the zoomed graph' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'SVG exported' { (Find-Element 'Saved flame graph SVG') -and [IO.File]::Exists($svgPath) }
    $svg = [xml][IO.File]::ReadAllText($svgPath)
    $labels = $svg.GetElementsByTagName('title', 'http://www.w3.org/2000/svg') |
        ForEach-Object { $_.InnerText }
    if (!($labels | Where-Object { $_ -like 'hot_loop*3 samples*75.0% of capture*' }) -or
        ($labels | Where-Object { $_ -like '*wait_for_work*' })) {
        throw "The SVG does not preserve the zoomed branch and weights. $labels"
    }

    [IO.File]::WriteAllText($after, "worker;main;hot_loop 5`nworker;io;read_file 15`n")
    Click-GraphButton 'Compare profiles'
    $dialog = Wait-Until 'owned comparison dialog' { Find-OwnedDialog 'Open folded stacks' }
    $null = Wait-Until 'enter the after profile path' { [FixtureDialog]::SetFilename($dialog, $after, '') }
    $null = Wait-Until 'open the after profile' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'normalized comparison shows the two recordings and largest change' {
        (Find-Element ('Before' + $dot + 'worker profile.folded' + $dot + '4 samples')) -and
        (Find-Element ('After' + $dot + 'after recording.folded' + $dot + '20 samples')) -and
        (Find-Element ('Before 0 samples (0.00%)' + $dot + 'After 15 samples (75.00%)' + $dot + '+75.00 pp')) -and
        (Find-Element ('worker' + $arrow + 'io' + $arrow + 'read_file'))
    }
    $change = Wait-Until 'selectable hot loop change' {
        Find-Element ('-50.00 pp' + $dot + 'hot_loop' + $callerArrow + 'main') ([System.Windows.Automation.ControlType]::ListItem)
    }
    $change.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'selecting a change shows its counts and full call stack' {
        (Find-Element ('Before 3 samples (75.00%)' + $dot + 'After 5 samples (25.00%)' + $dot + '-50.00 pp')) -and
        (Find-Element ('worker' + $arrow + 'main' + $arrow + 'hot_loop'))
    }
    $edit = Wait-Until 'comparison symbol filter' {
        Find-Element 'Filter changed call stacks' ([System.Windows.Automation.ControlType]::Edit)
    }
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('MAIN')
    Click-GraphButton 'Apply filter'
    $null = Wait-Until 'filter finds an ancestor and counts a lower sample share' {
        (Find-Element ('1 of 3 changed stacks' + $dot + 'Page 1 of 1')) -and
        (Find-Element ('Before 3 samples (75.00%)' + $dot + 'After 5 samples (25.00%)' + $dot + '-50.00 pp')) -and
        (Find-Element ('worker' + $arrow + 'main' + $arrow + 'hot_loop'))
    }
    Click-GraphButton 'Back to flame graph'
    $null = Wait-Until 'return to the original zoom and symbol search' {
        (Find-Element 'Zoomed to main') -and
        (Find-Element ('1 matching frame' + $dot + '75.0% of samples' + $dot + 'Match 1'))
    }
    Click-GraphButton 'Reset zoom'
    $null = Wait-Until 'whole profile is visible again' {
        Find-Element ('All samples' + $dot + '4 samples' + $dot + '100.0% of capture')
    }
} finally {
    Close-FixtureApp $app
}
