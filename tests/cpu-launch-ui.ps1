[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$folded = Join-Path $State 'launched workload.folded'
$raw = [IO.Path]::ChangeExtension($folded, 'perf.data')
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
    $null = Wait-Until 'timeout profile was saved and opened' {
        (Find-Element 'Captured CPU profile') -and
        (Find-Element 'Command reached its time limit') -and (Find-Element '2 samples') -and
        (Find-Element "Raw perf recording saved to $raw")
    }
    $lines = [IO.File]::ReadAllLines($folded)
    if (($lines -join '|') -cne 'cpu worker;child 1|cpu worker;main 1') {
        throw "The folded profile did not contain the parent and child samples: $lines"
    }
    $actual = [IO.File]::ReadAllBytes($raw)
    $expected = [Text.Encoding]::ASCII.GetBytes('PERFILE2') +
        [byte[]]@(0,255,13,10) + [Text.Encoding]::ASCII.GetBytes('raw perf fixture') + [byte]0
    if (($actual -join ',') -cne ($expected -join ',')) { throw 'The raw perf recording was not preserved byte for byte' }
} finally {
    Close-FixtureApp $app
}
