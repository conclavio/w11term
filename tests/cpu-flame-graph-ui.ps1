[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$folded = Join-Path $State 'worker profile.folded'
$after = Join-Path $State 'after recording.folded'
$svgPath = Join-Path $State 'hot branch.svg'
$inspectedSvg = Join-Path $State 'after hot branch.svg'
$diffAfterSvg = Join-Path $State 'diff after.svg'
$diffBeforeSvg = Join-Path $State 'diff before.svg'
$comparisonPath = Join-Path $State 'filtered CPU comparison.json'
$dot = ' ' + [char]0xb7 + ' '
$arrow = ' ' + [char]0x2192 + ' '
$callerArrow = ' ' + [char]0x2190 + ' '
$env:W11TERM_TEST_COPY_FILE = Join-Path $State 'diagnostic-path-copy.txt'

function Click-GraphButton([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Export-DifferentialSvg([string]$Path) {
    Click-GraphButton 'Export SVG'
    $dialog = Wait-Until 'owned differential graph export dialog' { Find-OwnedDialog 'Export CPU flame graph' }
    $null = Wait-Until 'enter the differential graph destination' {
        [FixtureDialog]::SetFilename($dialog, $Path, 'cpu-flame-graph.svg')
    }
    $null = Wait-Until 'save the differential graph' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'differential SVG written' { (Find-Element 'Saved flame graph SVG') -and [IO.File]::Exists($Path) }
    [xml][IO.File]::ReadAllText($Path)
}

function Find-SvgFrame([xml]$Svg, [string]$Symbol) {
    foreach ($title in $Svg.GetElementsByTagName('title', 'http://www.w3.org/2000/svg')) {
        if ($title.InnerText.StartsWith("$Symbol ") -and $title.ParentNode.LocalName -eq 'g') {
            return ,$title.ParentNode
        }
    }
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
    $null = Wait-Until 'the added stack can be inspected only in the after recording' {
        $inspectBefore = Find-Element 'Inspect before flame graph' $button
        $inspectAfter = Find-Element 'Inspect after flame graph' $button
        $inspectBefore -and !$inspectBefore.Current.IsEnabled -and
            $inspectAfter -and $inspectAfter.Current.IsEnabled
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
    $edit = Find-Element 'Filter changed call stacks' ([System.Windows.Automation.ControlType]::Edit)
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('read_file')
    $null = Wait-Until 'the draft filter is ready to apply' {
        $apply = Find-Element 'Apply filter' $button
        $apply -and $apply.Current.IsEnabled
    }
    Click-GraphButton 'Inspect after flame graph'
    $null = Wait-Until 'the after graph opens at the selected frame with its own samples' {
        (Find-Element 'After profile') -and (Find-Element 'after recording.folded') -and
            (Find-Element 'CPU flame graph chart') -and (Find-Element 'Zoomed to hot_loop') -and
            (Find-Element ("hot_loop${dot}5 samples${dot}25.0% of capture")) -and (Find-Element 'Caller main')
    }
    Click-GraphButton 'Export SVG'
    $dialog = Wait-Until 'owned inspected graph export dialog' { Find-OwnedDialog 'Export CPU flame graph' }
    $null = Wait-Until 'enter the inspected graph export destination' {
        [FixtureDialog]::SetFilename($dialog, $inspectedSvg, 'cpu-flame-graph.svg')
    }
    $null = Wait-Until 'save the inspected source graph' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'inspected SVG written' { (Find-Element 'Saved flame graph SVG') -and [IO.File]::Exists($inspectedSvg) }
    $inspected = [IO.File]::ReadAllText($inspectedSvg)
    if (!$inspected.Contains('hot_loop') -or !$inspected.Contains('5 samples') -or $inspected.Contains('read_file')) {
        throw 'The inspected SVG did not use the after recording and selected branch'
    }
    $sourceSearch = Find-Element 'Search flame graph symbols' ([System.Windows.Automation.ControlType]::Edit)
    $sourceSearch.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('read_file')
    Click-GraphButton 'Next symbol match'
    $null = Wait-Until 'the inspected after graph can navigate independently' { Find-Element 'Zoomed to read_file' }
    Click-GraphButton 'Back to comparison'
    $null = Wait-Until 'return preserves the applied filter, draft, and stack selection' {
        $filter = Find-Element 'Filter changed call stacks' ([System.Windows.Automation.ControlType]::Edit)
        $filter -and $filter.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq 'read_file' -and
            (Find-Element "1 of 3 changed stacks${dot}Page 1 of 1") -and
            (Find-Element "Before 3 samples (75.00%)${dot}After 5 samples (25.00%)${dot}-50.00 pp")
    }
    Click-GraphButton 'Inspect before flame graph'
    $null = Wait-Until 'the before graph opens at the same call path in the baseline' {
        (Find-Element 'Before profile') -and (Find-Element 'worker profile.folded') -and
            (Find-Element 'Zoomed to hot_loop') -and (Find-Element "hot_loop${dot}3 samples${dot}75.0% of capture")
    }
    Click-GraphButton 'Back to comparison'
    Click-GraphButton ('Export comparison' + [char]0x2026)
    $dialog = Wait-Until 'owned comparison save dialog' { Find-OwnedDialog 'Save CPU profile comparison' }
    $null = Wait-Until 'enter the comparison destination' {
        [FixtureDialog]::SetFilename($dialog, $comparisonPath, 'cpu-profile-comparison.json')
    }
    $null = Wait-Until 'save the applied comparison' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'comparison JSON published' { [IO.File]::Exists($comparisonPath) }
    $exported = [IO.File]::ReadAllText($comparisonPath) | ConvertFrom-Json
    $row = $exported.changes[0]
    if ($exported.schema_version -ne 1 -or $exported.filter -cne 'MAIN' -or
        $exported.before.file_name -cne 'worker profile.folded' -or $exported.before.total_samples -ne 4 -or
        $exported.after.file_name -cne 'after recording.folded' -or $exported.after.total_samples -ne 20 -or
        $exported.total_changed_stacks -ne 3 -or $exported.changes.Count -ne 1 -or
        ($row.stack -join ';') -cne 'worker;main;hot_loop' -or
        $row.before_samples -ne 3 -or $row.after_samples -ne 5 -or
        $row.before_percent -ne 75 -or $row.after_percent -ne 25 -or $row.delta_percentage_points -ne -50) {
        throw "The export does not describe the applied CPU comparison. $([IO.File]::ReadAllText($comparisonPath))"
    }
    $cliPath = Join-Path $State 'offline CPU comparison.json'
    $null = & $Cli compare-cpu-profiles $folded $after --filter MAIN --output-file $cliPath
    if ($LASTEXITCODE -ne 0 -or [Convert]::ToBase64String([IO.File]::ReadAllBytes($cliPath)) -cne
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($comparisonPath))) {
        throw 'CLI comparison differs from the GUI export with the same profiles and filter'
    }
    Click-GraphButton 'Differential flame graph'
    $null = Wait-Until 'differential opens with the after recording widths' {
        (Find-Element 'Differential CPU flame graph') -and (Find-Element 'After profile') -and
            (Find-Element 'CPU flame graph chart') -and
            (Find-Element "All samples${dot}20 samples${dot}100.0% of capture")
    }
    $svgAfter = Export-DifferentialSvg $diffAfterSvg
    $readFrame = Find-SvgFrame $svgAfter 'read_file'
    $hotFrame = Find-SvgFrame $svgAfter 'hot_loop'
    if (!$readFrame -or $readFrame.title -notmatch 'Before 0.+After 15.+\+75\.00 pp' -or
        $readFrame.rect.fill -cne 'rgb(255,111,99)' -or
        !$hotFrame -or $hotFrame.title -notmatch 'Before 3.+After 5.+-50\.00 pp' -or
        (Find-SvgFrame $svgAfter 'wait_for_work')) {
        throw 'The after differential SVG has incorrect widths, colors, or exact-stack deltas'
    }
    $diffSearch = Find-Element 'Search flame graph symbols' ([System.Windows.Automation.ControlType]::Edit)
    $diffSearch.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('hot_loop')
    Click-GraphButton 'Next symbol match'
    $null = Wait-Until 'zoomed differential frame reports both exact-stack counts' {
        (Find-Element 'Zoomed to hot_loop') -and
        (Find-Element "Exact stack${dot}Before 3${dot}After 5${dot}-50.00 pp")
    }
    Click-GraphButton 'Before widths'
    $null = Wait-Until 'before geometry includes its own sample total' {
        (Find-Element 'Before profile') -and (Find-Element "All samples${dot}4 samples${dot}100.0% of capture")
    }
    $svgBefore = Export-DifferentialSvg $diffBeforeSvg
    $removed = Find-SvgFrame $svgBefore 'wait_for_work'
    $hotBefore = Find-SvgFrame $svgBefore 'hot_loop'
    if (!$removed -or $removed.title -notmatch 'Before 1.+After 0.+-25\.00 pp' -or
        $removed.rect.fill -cne 'rgb(141,175,229)' -or
        $hotBefore.rect.fill -cne $hotFrame.rect.fill -or
        (Find-SvgFrame $svgBefore 'read_file')) {
        throw 'The before differential SVG did not reveal removed paths with the same color scale'
    }
    foreach ($side in @('before', 'after')) {
        $nativeSvg = if ($side -eq 'before') { $diffBeforeSvg } else { $diffAfterSvg }
        $cliSvg = Join-Path $State "offline diff $side.svg"
        $null = & $Cli compare-cpu-profiles $folded $after --format svg --widths $side --output-file $cliSvg
        if ($LASTEXITCODE -ne 0 -or [Convert]::ToBase64String([IO.File]::ReadAllBytes($cliSvg)) -cne
            [Convert]::ToBase64String([IO.File]::ReadAllBytes($nativeSvg))) {
            throw "The offline $side differential SVG differs from the Windows viewer export"
        }
    }
    Click-GraphButton 'After widths'
    $null = Wait-Until 'the after width view retains its own zoom' { Find-Element 'Zoomed to hot_loop' }
    Click-GraphButton 'Back to comparison'
    $null = Wait-Until 'after closing differential the stack selection is unchanged' {
        (Find-Element "1 of 3 changed stacks${dot}Page 1 of 1") -and
            (Find-Element "Before 3 samples (75.00%)${dot}After 5 samples (25.00%)${dot}-50.00 pp")
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
    Click-GraphButton 'Return to diagnostics overview'
    $null = Wait-Until 'both source profiles have been remembered' {
        (Find-Element 'Reopen worker profile.folded' $button) -and
        (Find-Element 'Reopen after recording.folded' $button)
    }
    Choose-DiagnosticFileAction 'worker profile.folded' 'Pin'
    $null = Wait-Until 'a baseline was saved as pinned' {
        (Find-Element 'Pinned files') -and (Find-Element 'Reopen worker profile.folded' $button)
    }
    Close-FixtureApp $app
    $app = $null
    $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui-restart.log')
    $window = Wait-InputWindow
    $restartedPane = ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id
    Wait-Text $restartedPane 'PS '
    Run-Palette 'Diagnostics overview'
    $null = Wait-Until 'pinned files still appear after restarting' { Find-Element 'Pinned files' }
    Choose-DiagnosticFileAction 'worker profile.folded' 'Copy path'
    $null = Wait-Until 'copy puts the exact pinned path on the clipboard' {
        $copied = Read-ClipboardFixture
        $copied -and [FixtureDialog]::SameFile($copied, $folded)
    }
    Click-GraphButton 'Reopen worker profile.folded'
    $null = Wait-Until 'recent CPU profile reopened after a full restart' {
        (Find-Element '4 samples') -and !(Find-Element 'Diagnostics overview panel')
    }
    Click-GraphButton 'Return to diagnostics overview'
    Choose-DiagnosticFileAction 'worker profile.folded' 'Use as baseline'
    $null = Wait-Until 'pinned CPU profile is the baseline' { Find-Element "Baseline${dot}worker profile.folded" }
    Choose-DiagnosticFileAction 'after recording.folded' 'Compare with baseline'
    $null = Wait-Until 'saved CPU comparison uses both source paths and the expected side' {
        (Find-Element "Before${dot}worker profile.folded${dot}4 samples") -and
        (Find-Element "After${dot}after recording.folded${dot}20 samples") -and
        (Find-Element "Before 0 samples (0.00%)${dot}After 15 samples (75.00%)${dot}+75.00 pp")
    }
    Click-GraphButton 'Back to flame graph'
    $null = Wait-Until 'the baseline is the current flame graph' { Find-Element '4 samples' }
    Click-GraphButton 'Return to diagnostics overview'
    Choose-DiagnosticFileAction 'worker profile.folded' 'Unpin'
    $null = Wait-Until 'unpin moves the baseline into recent files' {
        !(Find-Element 'Pinned files') -and (Find-Element 'Reopen worker profile.folded' $button)
    }
    Choose-DiagnosticFileAction 'worker profile.folded' 'Clear baseline'
    $null = Wait-Until 'clearing the baseline leaves its file in history' {
        !(Find-Element "Baseline${dot}worker profile.folded") -and (Find-Element 'Reopen worker profile.folded' $button)
    }
    Choose-DiagnosticFileAction 'after recording.folded' 'Forget'
    $null = Wait-Until 'forget removes the persisted entry and keeps the artifact' {
        if (Find-Element 'Reopen after recording.folded' $button) { return }
        $catalog = Join-Path $env:W11TERM_DATA_DIR 'recent-diagnostics.json'
        $history = [IO.File]::ReadAllText($catalog) | ConvertFrom-Json
        ($history.entries.Count -eq 1) -and [IO.File]::Exists($after)
    }
} finally {
    if ($app) { Close-FixtureApp $app }
}
