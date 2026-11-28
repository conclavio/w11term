[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$folded = Join-Path $State 'launched workload.folded'
$raw = [IO.Path]::ChangeExtension($folded, 'perf.data')
$repeated = Join-Path $State 'repeated workload.folded'
$compareOriginal = 'Compare with original profile'
$dot = ' ' + [char]0xb7 + ' '
$marker = [IO.File]::ReadAllText((Join-Path $State 'launch-marker'))
$wire = [IO.File]::ReadAllText((Join-Path $State 'launch-wire'))
$comboType = [System.Windows.Automation.ControlType]::ComboBox
$editType = [System.Windows.Automation.ControlType]::Edit
$listItemType = [System.Windows.Automation.ControlType]::ListItem
$expandPattern = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
$selectPattern = [System.Windows.Automation.SelectionItemPattern]::Pattern
$valuePattern = [System.Windows.Automation.ValuePattern]::Pattern

function Click-LaunchButton([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Set-LaunchField([string]$Name, [string]$Value) {
    $field = Wait-Until "field '$Name'" { Find-Element $Name $editType }
    $field.GetCurrentPattern($valuePattern).SetValue($Value)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $remote = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $remote
    $null = Control @('wait-ssh-connected','--pane',"$remote",'--timeout','15')
    Run-Palette 'CPU flame graph'
    Click-LaunchButton 'Capture from SSH'
    $null = Wait-Until 'launch command target' {
        if (Find-Element 'Linux executable' $editType) { return $true }
        $picker = Find-Element 'CPU profile target' $comboType
        if (!$picker) { return }
        try {
            $popup = $picker.GetCurrentPattern($expandPattern)
            if ($popup.Current.ExpandCollapseState -eq [System.Windows.Automation.ExpandCollapseState]::Collapsed) {
                $picker.SetFocus()
                $popup.Expand()
            } else {
                $choice = Find-Element 'Launch command' $listItemType
                if ($choice) { $choice.GetCurrentPattern($selectPattern).Select() }
            }
        } catch [System.Windows.Automation.ElementNotAvailableException] {}
    }
    Set-LaunchField 'Time limit in seconds' '2'
    Set-LaunchField 'Working directory (defaults to home)' '/tmp/work space'
    Set-LaunchField 'Arguments (JSON array)' '["one arg","it''s $literal","","--child"]'
    # The review button becomes enabled after the app applies the executable edit.
    Set-LaunchField 'Linux executable' './cpu worker'
    Click-LaunchButton 'Review CPU profile'
    try {
        $null = Wait-Until 'review verified the full executable and literal arguments' {
            (Find-Element 'Executable "/tmp/work space/cpu worker"') -and
            (Find-Element 'Arguments ["one arg","it''s $literal","","--child"]')
        }
    } catch {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $names = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition) |
            ForEach-Object { $_.Current.Name } |
            Where-Object { $_ } | Select-Object -Last 40
        $wireSuffix = if ([IO.File]::Exists($wire)) {
            $sent = [IO.File]::ReadAllText($wire)
            $sent.Substring([Math]::Max(0, $sent.Length - 240))
        } else { '(no launch diagnostic reached the SSH peer)' }
        throw "$_ Visible controls: $($names -join ' | '). Wire suffix: $wireSuffix"
    }
    if ([IO.File]::Exists($marker)) { throw 'Review started the workload' }
    $rawOption = Wait-Until 'raw perf recording option' {
        Find-Element 'Keep raw perf recording' ([System.Windows.Automation.ControlType]::CheckBox)
    }
    $rawOption.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
    $null = Wait-Until 'raw perf recording transfer acknowledged' {
        Find-Element 'Saves a .perf.data file next to the folded profile. The transfer can take up to 60 additional seconds.'
    }
    Click-LaunchButton 'Record CPU profile'
    $dialog = Wait-Until 'owned CPU profile destination dialog' { Find-OwnedDialog 'Save CPU profile' }
    $null = Wait-Until 'enter a destination for folded samples' {
        [FixtureDialog]::SetFilename($dialog, $folded, 'cpu-profile.folded')
    }
    if ([IO.File]::Exists($marker)) { throw 'The workload started before destination confirmation' }
    $null = Wait-Until 'confirm recording destination' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the command is running on the reviewed SSH pane' { [IO.File]::Exists($marker) }
    Click-LaunchButton 'Stop and save profile'
    $rawPrefix = 'Raw perf recording saved to '
    $profileText = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    try {
        $null = Wait-Until 'stopped command profile was saved and opened' {
            if (!(Find-Element 'Captured CPU profile') -or
                !(Find-Element 'Command stopped by request') -or
                !(Find-Element '2 samples') -or ![IO.File]::Exists($folded)) { return }
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $profileText)) {
                $name = $item.Current.Name
                if ($name.StartsWith($rawPrefix, [StringComparison]::Ordinal) -and
                    [FixtureDialog]::SameFile($raw, $name.Substring($rawPrefix.Length))) {
                    return $true
                }
            }
        }
    } catch {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $names = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $profileText) |
            ForEach-Object { $_.Current.Name } |
            Where-Object { $_ -match 'profile|perf|samples|command|captur' } | Select-Object -Last 24
        throw "$_. Folded file exists: $([IO.File]::Exists($folded)). Raw file exists: $([IO.File]::Exists($raw)). Workload started: $([IO.File]::Exists($marker)). Profile controls: $($names -join ' | ')"
    }
    $lines = [IO.File]::ReadAllLines($folded)
    if (($lines -join '|') -cne 'cpu worker;child 1|cpu worker;main 1') {
        throw "The folded profile did not contain the parent and child samples: $lines"
    }
    $actual = [IO.File]::ReadAllBytes($raw)
    $expected = [Text.Encoding]::ASCII.GetBytes('PERFILE2') +
        [byte[]]@(0,255,13,10) + [Text.Encoding]::ASCII.GetBytes('raw perf fixture') + [byte]0
    if (($actual -join ',') -cne ($expected -join ',')) { throw 'The raw perf recording was not preserved byte for byte' }
    $reportPath = "$folded.json"
    Click-LaunchButton ('Save capture report' + [char]0x2026)
    $dialog = Wait-OwnedDialog 'Save CPU profile report'
    $null = Wait-Until 'choose the command capture report destination' {
        [FixtureDialog]::SetFilename($dialog, $reportPath, [IO.Path]::GetFileName($reportPath))
    }
    $null = Wait-Until 'confirm the command capture report destination' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'command capture report is saved' { Find-Element 'Saved CPU profile report' }
    $proof = & $Cli verify-cpu-profile-report $folded $reportPath --perf-data $raw | ConvertFrom-Json
    $report = $proof.result.report
    $target = $report.reviewed.target
    $expectedArguments = @('one arg', 'it''s $literal', '', '--child')
    if ($LASTEXITCODE -ne 0 -or !$proof.ok -or $target.mode -ne 'command' -or
        $target.program -cne '/tmp/work space/cpu worker' -or $target.directory -cne '/tmp/work space' -or
        ($target.options.arguments -join [char]0) -cne ($expectedArguments -join [char]0) -or
        $report.workload.status -ne 'stopped' -or $report.samples -ne 2 -or
        !$report.reviewed.raw_perf_data_requested -or $report.raw_perf_data.bytes -ne $actual.Length -or
        ![FixtureDialog]::SameFile($raw, $proof.result.verified_perf_data_path)) {
        throw "Command capture report did not preserve the reviewed options and both files: $($proof | ConvertTo-Json -Depth 10 -Compress)"
    }
    if (Find-Element $compareOriginal $button) { throw 'A standalone profile has no original to compare' }
    [IO.File]::Delete($marker)
    Click-LaunchButton ('Repeat CPU profile' + [char]0x2026)
    $null = Wait-Until 'the repeat draft restores the literal command and requires fresh review' {
        $program = Find-Element 'Linux executable' $editType
        $argv = Find-Element 'Arguments (JSON array)' $editType
        $directory = Find-Element 'Working directory (defaults to home)' $editType
        $seconds = Find-Element 'Time limit in seconds' $editType
        $program -and $program.GetCurrentPattern($valuePattern).Current.Value -ceq './cpu worker' -and
            $argv -and $argv.GetCurrentPattern($valuePattern).Current.Value -ceq '["one arg","it''s $literal","","--child"]' -and
            $directory -and $directory.GetCurrentPattern($valuePattern).Current.Value -ceq '/tmp/work space' -and
            $seconds -and $seconds.GetCurrentPattern($valuePattern).Current.Value -ceq '2' -and
            !(Find-Element 'Record CPU profile' $button)
    }
    Click-LaunchButton 'Review CPU profile'
    $null = Wait-Until 'the fresh review retains the raw recording option' {
        (Find-Element 'Executable "/tmp/work space/cpu worker"') -and
            (Find-Element 'Saves a .perf.data file next to the folded profile. The transfer can take up to 60 additional seconds.')
    }
    Click-LaunchButton 'Record CPU profile'
    $dialog = Wait-OwnedDialog 'Save CPU profile'
    if ([IO.File]::Exists($marker)) {
        throw 'The repeated workload started before destination approval'
    }
    $null = Wait-Until 'enter a new destination for the repeated profile' {
        [FixtureDialog]::SetFilename($dialog, $repeated, 'cpu-profile.folded')
    }
    $null = Wait-Until 'confirm the repeated profile destination' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the repeated workload starts' { [IO.File]::Exists($marker) }
    Click-LaunchButton 'Stop and save profile'
    $null = Wait-Until 'the repeated profile and raw recording are saved' {
        [IO.File]::Exists($repeated) -and (Find-Element 'Captured CPU profile') -and
            [IO.File]::Exists([IO.Path]::ChangeExtension($repeated, 'perf.data'))
    }
    if ([IO.File]::ReadAllText($repeated) -cne [IO.File]::ReadAllText($folded) -or
        ([IO.File]::ReadAllBytes([IO.Path]::ChangeExtension($repeated, 'perf.data')) -join ',') -cne ($actual -join ',')) {
        throw 'The repeated profile did not preserve the folded stacks and raw recording'
    }
    $moved = Join-Path $State 'original profile moved.folded'
    [IO.File]::Move($folded, $moved)
    try {
        Click-LaunchButton $compareOriginal
        $null = Wait-Until 'the loaded original and repeated profiles are compared in order' {
            (Find-Element "Before${dot}launched workload.folded${dot}2 samples") -and
                (Find-Element "After${dot}repeated workload.folded${dot}2 samples") -and
                (Find-Element 'No changes in call stack sample shares.')
        }
    } finally {
        [IO.File]::Move($moved, $folded)
    }
    Click-LaunchButton 'Back to flame graph'
    $null = Wait-Until 'the repeated graph and its capture actions remain available' {
        (Find-Element 'repeated workload.folded') -and
            (Find-Element $compareOriginal $button) -and
            (Find-Element ('Repeat CPU profile' + [char]0x2026) $button)
    }
    Click-LaunchButton 'Open folded stacks'
    $dialog = Wait-OwnedDialog 'Open folded stacks'
    $null = Wait-Until 'enter the standalone profile path' { [FixtureDialog]::SetFilename($dialog, $folded, '') }
    $null = Wait-Until 'open the standalone profile' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'opening a standalone profile clears the capture actions' {
        (Find-Element 'launched workload.folded') -and
            !(Find-Element $compareOriginal $button) -and
            !(Find-Element ('Save capture report' + [char]0x2026) $button)
    }
} finally {
    Close-FixtureApp $app
}
