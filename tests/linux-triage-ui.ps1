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
    $null = Wait-Until 'collection summary' { Find-Element '7 of 10 checks complete' }
    Show-CpuEvidence
    Show-KernelCheck
    Click 'Save report'
    $save = Wait-Until 'owned triage save dialog' { Find-OwnedDialog 'Save Linux triage report' }
    $null = Wait-Until 'name the local triage report' { [FixtureDialog]::SetFilename($save, $report, 'linux-triage.json') }
    $null = Wait-Until 'save through native dialog' { [FixtureDialog]::ClickButton($save, 1) }
    $null = Wait-Until 'saved report published' { (Find-Element 'Saved Linux triage report') -and [IO.File]::Exists($report) }
    $saved = [IO.File]::ReadAllText($report) | ConvertFrom-Json
    if ($saved.version -ne 1 -or $saved.collection.samples[1].status -cne 'permission-denied') { throw 'Wrong report persisted' }
    $null = Control @('close-pane','--pane',"$remote")
    Wait-Focus $local
    Click 'Close performance triage'
    Run-Palette 'Linux performance triage'
    Click 'Open report'
    $open = Wait-Until 'owned triage open dialog' { Find-OwnedDialog 'Open Linux triage report' }
    $null = Wait-Until 'enter the saved report path' { [FixtureDialog]::SetFilename($open, $report, '') }
    $null = Wait-Until 'open through native dialog' { [FixtureDialog]::ClickButton($open, 1) }
    $null = Wait-Until 'saved triage reopened without the SSH pane' { Find-Element 'Opened Linux triage report' }
    Show-CpuEvidence
    Show-KernelCheck
} finally {
    Close-FixtureApp $app
}
