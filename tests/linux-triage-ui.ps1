[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$combo = [System.Windows.Automation.ControlType]::ComboBox
$expandPattern = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
$selectPattern = [System.Windows.Automation.SelectionItemPattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '
$report = Join-Path $State 'captured.json'
$afterReport = Join-Path $State 'after triage.json'
$comparisonReport = Join-Path $State 'saved triage comparison.txt'
$command = [IO.File]::ReadAllText((Join-Path $State 'triage-command-path'))

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Show-KernelCheck {
    $null = Wait-Until 'kernel log permission result' {
        if (Find-Element 'Operation not permitted') { return $true }
        $samples = Find-Element 'Triage samples' $combo
        if (!$samples) { return }
        try {
            $popup = $samples.GetCurrentPattern($expandPattern)
            if ($popup.Current.ExpandCollapseState -eq [System.Windows.Automation.ExpandCollapseState]::Collapsed) {
                $samples.SetFocus()
                $popup.Expand()
            } else {
                $choice = Find-Element "dmesg${dot}Permission denied" ([System.Windows.Automation.ControlType]::ListItem)
                if ($choice) { $choice.GetCurrentPattern($selectPattern).Select() }
            }
        } catch [System.Windows.Automation.ElementNotAvailableException] {
            # Reacquire the popup when WinUI replaces its automation elements.
        }
    }
    $null = Wait-Until 'sample status and original command shown' {
        (Find-Element "Permission denied${dot}Exit 1") -and (Find-Element 'dmesg -T | tail -n 50')
    }
}

function Show-CpuEvidence {
    Click 'Show evidence for Peak CPU busy'
    $null = Wait-Until 'finding opens the exact live vmstat row' {
        (Find-Element "Evidence${dot}vmstat output line 3") -and (Find-Element '3 0 0 8 40 12 46 1 1')
    }
}

function Select-TriageChange([string]$Name) {
    $change = Wait-Until "triage change '$Name'" {
        Find-Element $Name ([System.Windows.Automation.ControlType]::ListItem)
    }
    $change.GetCurrentPattern($selectPattern).Select()
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $local = ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id
    $remote = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $remote
    $null = Control @('wait-ssh-connected','--pane',"$remote",'--timeout','15')
    Run-Palette 'Linux performance triage'
    $null = Wait-Until 'triage plan with reviewed limits and commands' {
        (Find-Element "60-second deadline${dot}16 KiB per output stream") -and (Find-Element 'Run triage' $button)
    }
    if ([IO.File]::Exists($command)) { throw 'Review opened an exec channel before confirmation' }
    $null = Control @('focus-pane','--pane',"$local")
    Wait-Focus $local
    $null = Wait-Until 'cannot run from a different pane' {
        $run = Find-Element 'Run triage' $button
        $run -and !$run.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$remote")
    Wait-Focus $remote
    Click 'Run triage'
    $null = Wait-Until 'remote probes started' { [IO.File]::Exists($command) }
    $null = Wait-Until 'finished probes appear while the final report is pending' {
        (Find-Element '3 of 10 checks finished') -and (Find-Element 'Stop and keep report' $button)
    }
    Click 'Return to diagnostics overview'
    $null = Control @('focus-pane','--pane',"$local")
    Wait-Focus $local
    $null = Wait-Until 'probe count remains visible from PowerShell' {
        Find-Element '3 of 10 checks finished'
    }
    Click 'Open Linux performance triage activity'
    Click 'Stop and keep report'
    $null = Wait-Until 'stopped collection keeps its completed checks' {
        (Find-Element '7 of 10 checks complete') -and (Find-Element 'Stopped by request')
    }
    Click 'Return to diagnostics overview'
    $null = Wait-Until 'overview shows the completed performance report' {
        Find-Element ('Collected Linux performance report' + [char]10 + 'Stopped by request')
    }
    Click 'Open Linux performance triage activity'
    Show-CpuEvidence
    Show-KernelCheck
    Click 'Save report'
    $save = Wait-Until 'owned triage save dialog' { Find-OwnedDialog 'Save Linux triage report' }
    $null = Wait-Until 'name the local triage report' { [FixtureDialog]::SetFilename($save, $report, 'linux-triage.json') }
    $null = Wait-Until 'save through native dialog' { [FixtureDialog]::ClickButton($save, 1) }
    $null = Wait-Until 'saved report published' { (Find-Element 'Saved Linux triage report') -and [IO.File]::Exists($report) }
    $saved = [IO.File]::ReadAllText($report) | ConvertFrom-Json
    if ($saved.version -ne 1 -or !$saved.collection.stopped_by_user -or
        $saved.collection.samples[1].status -cne 'permission-denied' -or
        $saved.collection.samples[8].status -cne 'incomplete') { throw 'Wrong report persisted' }
    $null = Control @('close-pane','--pane',"$remote")
    Wait-Focus $local
    Click 'Close performance triage'
    Run-Palette 'Linux performance triage'
    Click 'Open report'
    $open = Wait-Until 'owned triage open dialog' { Find-OwnedDialog 'Open Linux triage report' }
    $null = Wait-Until 'enter the saved report path' { [FixtureDialog]::SetFilename($open, $report, '') }
    $null = Wait-Until 'open through native dialog' { [FixtureDialog]::ClickButton($open, 1) }
    $null = Wait-Until 'saved triage reopened without the SSH pane' {
        (Find-Element 'Opened Linux triage report') -and (Find-Element 'Stopped by request')
    }
    Show-CpuEvidence
    Show-KernelCheck
    $saved.collection.samples[1].status = 'ok'
    $saved.collection.samples[1].exit_status = 0
    $saved.collection.samples[1].stdout.text = '[Mon] system started'
    $saved.collection.samples[1].stderr.text = ''
    $vmstat = $saved.collection.samples | Where-Object id -eq 'vmstat'
    $vmstat.stdout.text = $vmstat.stdout.text.Replace('3 0 0 8 40 12 46 1 1','3 0 0 8 70 14 14 1 1')
    ($saved.target.probes | Where-Object id -eq 'mpstat').sampling = 'Updated interval sampling'
    [IO.File]::WriteAllText($afterReport, ($saved | ConvertTo-Json -Depth 16))
    Click 'Compare reports'
    $open = Wait-Until 'owned after report dialog' { Find-OwnedDialog 'Choose after triage report' }
    $null = Wait-Until 'enter the after report path' { [FixtureDialog]::SetFilename($open, $afterReport, '') }
    $null = Wait-Until 'compare from the native dialog' { [FixtureDialog]::ClickButton($open, 1) }
    $null = Wait-Until 'both source reports and the coverage change' {
        (Find-Element "Before${dot}captured.json") -and
        (Find-Element "After${dot}after triage.json") -and
        (Find-Element "Before${dot}Permission denied${dot}Exit 1") -and
        (Find-Element "After${dot}OK${dot}Exit 0") -and
        (Find-Element 'Operation not permitted')
    }
    Select-TriageChange 'mpstat check or sampling changed'
    $null = Wait-Until 'sampling changes are visible with the probe commands' {
        (Find-Element 'Updated interval sampling') -and (Find-Element "After${dot}Utility missing${dot}Exit 127")
    }
    Select-TriageChange "Peak CPU busy${dot}+32.0 pp"
    $null = Wait-Until 'paired CPU values with the original interval lines' {
        (Find-Element "Before${dot}52% user + system") -and
        (Find-Element "After${dot}84% user + system") -and
        (Find-Element '3 0 0 8 40 12 46 1 1') -and
        (Find-Element '3 0 0 8 70 14 14 1 1')
    }
    Click ('Export comparison' + [char]0x2026)
    $save = Wait-Until 'owned comparison export dialog' { Find-OwnedDialog 'Save Linux triage comparison' }
    $null = Wait-Until 'enter the triage comparison export path' {
        [FixtureDialog]::SetFilename($save, $comparisonReport, 'linux-triage-comparison.txt')
    }
    $null = Wait-Until 'save the comparison as text' { [FixtureDialog]::ClickButton($save, 1) }
    $null = Wait-Until 'triage comparison export published' { [IO.File]::Exists($comparisonReport) }
    $export = [IO.File]::ReadAllText($comparisonReport)
    if (!$export.Contains("Before${dot}captured.json") -or !$export.Contains("After${dot}after triage.json") -or
        !$export.Contains("Before${dot}Permission denied${dot}Exit 1") -or
        !$export.Contains("After${dot}OK${dot}Exit 0") -or
        !$export.Contains('1 | Operation not permitted') -or
        !$export.Contains("Sampling${dot}Updated interval sampling") -or
        !$export.Contains("Before${dot}52% user + system${dot}vmstat stdout line 3") -or
        !$export.Contains("After${dot}84% user + system${dot}vmstat stdout line 3") -or
        !$export.Contains("Delta${dot}+32.0 pp") -or
        !$export.Contains('3 | 3 0 0 8 40 12 46 1 1') -or !$export.Contains('3 | 3 0 0 8 70 14 14 1 1') -or
        $export.Contains("Before${dot}uptime") -or $export.Contains($State) -or
        $export.Contains($saved.target.host_key_fingerprint)) {
        throw "The export did not preserve the paired changes and source evidence. $export"
    }
    $cliOutput = Join-Path $State 'offline triage comparison.txt'
    $null = & $Cli compare-linux-triage $report $afterReport --format text --output-file $cliOutput
    if ($LASTEXITCODE -ne 0 -or [Convert]::ToBase64String([IO.File]::ReadAllBytes($cliOutput)) -cne
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($comparisonReport))) {
        throw 'CLI triage comparison differs from the viewer export for the same reports'
    }
    $jsonBase = Join-Path $State 'structured triage comparison'
    Click ('Export comparison' + [char]0x2026)
    $save = Wait-Until 'owned JSON export dialog' { Find-OwnedDialog 'Save Linux triage comparison' }
    $null = Wait-Until 'select JSON triage comparison' {
        [FixtureDialog]::SelectFileType($save, 'JSON report (*.json)')
    }
    $null = Wait-Until 'enter the JSON comparison export path' {
        [FixtureDialog]::SetFilename($save, $jsonBase, 'linux-triage-comparison.txt') -or
            [FixtureDialog]::SetFilename($save, $jsonBase, 'linux-triage-comparison.json')
    }
    $null = Wait-Until 'save the comparison as JSON' { [FixtureDialog]::ClickButton($save, 1) }
    $null = Wait-Until 'JSON report is published with the selected extension' {
        [IO.File]::Exists("$jsonBase.json")
    }
    $cliJson = Join-Path $State 'offline triage comparison.json'
    $null = & $Cli compare-linux-triage $report $afterReport --output-file $cliJson
    if ($LASTEXITCODE -ne 0 -or [Convert]::ToBase64String([IO.File]::ReadAllBytes($cliJson)) -cne
        [Convert]::ToBase64String([IO.File]::ReadAllBytes("$jsonBase.json"))) {
        throw 'CLI triage comparison JSON differs from the Windows export'
    }
    Click ('Review comparison with OpenRouter' + [char]0x2026)
    $null = Wait-Until 'comparison remains open beside the paired review preview' {
        $field = Find-Element 'OpenRouter request preview' ([System.Windows.Automation.ControlType]::Edit)
        if (!$field -or !$field.Current.IsEnabled) { return }
        $preview = $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
        $preview.Contains('Linux performance triage comparison') -and
            $preview.Contains('Before vmstat stdout line 3: 3 0 0 8 40 12 46 1 1') -and
            $preview.Contains('After vmstat stdout line 3: 3 0 0 8 70 14 14 1 1') -and
            $preview.Contains('Before dmesg: Permission denied') -and
            $preview.Contains('+32.0 pp') -and
            !$preview.Contains('captured.json') -and !$preview.Contains('after triage.json') -and
            !$preview.Contains($saved.target.host_key_fingerprint) -and
            (Find-Element 'Triage report changes')
    }
    if (Find-Element 'Send reviewed request' $button) { throw 'Opening a comparison created a reviewed request' }
    Click 'Back to report'
    Show-KernelCheck
    Click ('Review report with OpenRouter' + [char]0x2026)
    try {
        $null = Wait-Until 'review pane summarizes the saved report and selected sample' {
            $field = Find-Element 'OpenRouter request preview' ([System.Windows.Automation.ControlType]::Edit)
            if (!$field -or !$field.Current.IsEnabled) { return }
            $preview = $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
            $preview.Contains('Locally observed findings') -and $preview.Contains('Peak CPU busy') -and
                $preview.Contains('dmesg stderr excerpt') -and $preview.Contains('Operation not permitted') -and
                (Find-Element 'Triage samples' $combo)
        }
    } catch {
        $field = Find-Element 'OpenRouter request preview' ([System.Windows.Automation.ControlType]::Edit)
        $text = if ($field) { $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value }
        throw "$_ Preview exists: $($null -ne $field). Enabled: $($field.Current.IsEnabled). Triage open: $($null -ne (Find-Element 'Triage samples' $combo)). Preview: $text"
    }
    if (Find-Element 'Send reviewed request' $button) { throw 'Opening the report created a reviewed request' }
    Click 'Return to diagnostics overview'
    Click 'Reopen after triage.json'
    $null = Wait-Until 'recent comparison source reopens as its own local triage report' {
        (Find-Element 'Opened Linux triage report') -and (Find-Element '8 of 10 checks complete')
    }
    Click 'Return to diagnostics overview'
    Choose-DiagnosticFileAction 'captured.json' 'Use as baseline'
    $null = Wait-Until 'saved triage baseline selected' { Find-Element "Baseline${dot}captured.json" }
    Choose-DiagnosticFileAction 'after triage.json' 'Compare with baseline'
    $null = Wait-Until 'saved triage comparison reopens both reports' {
        (Find-Element "Before${dot}captured.json") -and (Find-Element "After${dot}after triage.json") -and
        (Find-Element "Before${dot}Permission denied${dot}Exit 1") -and (Find-Element "After${dot}OK${dot}Exit 0")
    }
    Click 'Back to report'
    $null = Wait-Until 'triage returns to the baseline report' { Find-Element '7 of 10 checks complete' }
} finally {
    Close-FixtureApp $app
}
