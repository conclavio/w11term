[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$output = Join-Path $State 'profile from triage.folded'
$marker = [IO.File]::ReadAllText((Join-Path $State 'capture-marker'))
$ticks = [IO.File]::ReadAllText((Join-Path $State 'profile-ticks-path'))
$repeat = 'Repeat CPU profile' + [char]0x2026
$dot = ' ' + [char]0xb7 + ' '
$measurement = [IO.File]::ReadAllText((Join-Path $State 'profile-measurement'))
$profileWorker = 'Profile worker' + $dot + 'PID 42' + $dot + '80.0% ' + $measurement
$valuePattern = [System.Windows.Automation.ValuePattern]::Pattern
$editType = [System.Windows.Automation.ControlType]::Edit

function Click-CaptureButton([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Wait-Disabled([string]$Name) {
    $null = Wait-Until "disabled '$Name'" {
        $item = Find-Element $Name $button
        $item -and !$item.Current.IsEnabled
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $localPane = ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id
    $remotePane = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $remotePane
    $null = Control @('wait-ssh-connected','--pane',"$remotePane",'--timeout','15')
    Run-Palette 'Linux performance triage'
    Click-CaptureButton 'Run triage'
    $null = Wait-Until 'process ranked by the recorded CPU measurement' {
        Find-Element $profileWorker $button
    }
    if (Find-Element ('Profile top-only' + $dot + 'PID 99' + $dot + '95.0% CPU in top snapshot') $button) {
        throw 'The top snapshot displaced available pidstat interval evidence'
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    Wait-Disabled $profileWorker
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click-CaptureButton $profileWorker
    $null = Wait-Until 'triage filled in the target process ID' {
        $item = Find-Element 'Linux process ID' $editType
        if ($item -and $item.GetCurrentPattern($valuePattern).Current.Value -eq '42') { $item }
    }
    $duration = Wait-Until 'CPU capture duration field' { Find-Element 'Duration in seconds' $editType }
    $duration.GetCurrentPattern($valuePattern).SetValue('2')
    Click-CaptureButton 'Review CPU profile'
    $null = Wait-Until 'live process identity and recording approval' {
        (Find-Element ('PID 42' + $dot + 'worker')) -and (Find-Element 'Record CPU profile' $button)
    }
    $rate = Wait-Until 'reviewed sampling frequency field' { Find-Element 'Sampling frequency in Hz' $editType }
    $rate.GetCurrentPattern($valuePattern).SetValue('100')
    $null = Wait-Until 'changing a capture setting clears the review' {
        !(Find-Element 'Record CPU profile' $button)
    }
    $rate.GetCurrentPattern($valuePattern).SetValue('99')
    Click-CaptureButton 'Review CPU profile'
    $null = Wait-Until 'recording approval after reviewing again' { Find-Element 'Record CPU profile' $button }
    if ([IO.File]::Exists($marker)) { throw 'Review started recording before approval' }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    Wait-Disabled 'Record CPU profile'
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click-CaptureButton 'Record CPU profile'
    $dialog = Wait-Until 'owned CPU profile save dialog' { Find-OwnedDialog 'Save CPU profile' }
    $null = Wait-Until 'enter the folded stack destination' {
        [FixtureDialog]::SetFilename($dialog, $output, 'cpu-profile.folded')
    }
    if ([IO.File]::Exists($marker)) { throw 'Recording started before choosing a destination' }
    $null = Wait-Until 'confirm the new profile file' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'recording started on the reviewed process' { [IO.File]::Exists($marker) }
    Click-CaptureButton 'Return to diagnostics overview'
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    Click-CaptureButton 'Open CPU flame graph activity'
    Click-CaptureButton 'Stop and save profile'
    $null = Wait-Until 'open the resulting graph and expose perf diagnostics' {
        (Find-Element 'Captured CPU profile') -and
        (Find-Element 'Stopped by request') -and
        (Find-Element '1 sample') -and (Find-Element 'Perf output')
    }
    if ([IO.File]::ReadAllText($output) -cne ('worker;main;hot_loop 1' + [char]10)) {
        throw 'The saved profile does not contain the reviewed stack'
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click-CaptureButton $repeat
    $null = Wait-Until 'the repeated PID draft restores its target and sampling' {
        $pidField = Find-Element 'Linux process ID' $editType
        $seconds = Find-Element 'Duration in seconds' $editType
        $pidField -and $pidField.GetCurrentPattern($valuePattern).Current.Value -eq '42' -and
            $seconds -and $seconds.GetCurrentPattern($valuePattern).Current.Value -eq '2' -and
            !(Find-Element 'Record CPU profile' $button)
    }
    [IO.File]::WriteAllText($ticks, '801')
    Click-CaptureButton 'Review CPU profile'
    $null = Wait-Until 'review rejects a recycled PID on the original SSH connection' {
        (Find-Element 'The original PID now belongs to a different process. Enter the PID you want to profile.') -and
            !(Find-Element 'Record CPU profile' $button)
    }
    Click-CaptureButton 'Back to flame graph'
    $null = Control @('close-pane','--pane',"$remotePane")
    Wait-Focus $localPane
    Click-CaptureButton 'Return to diagnostics overview'
    Wait-DiagnosticArtifact 'Saved CPU profile' $output 'Stopped by request'
    Click-CaptureButton 'Open CPU flame graph activity'
    $null = Wait-Until 'the completed graph survives closing SSH' {
        Find-Element ('All samples' + $dot + '1 sample' + $dot + '100.0% of capture')
    }
    Click-CaptureButton $repeat
    $null = Wait-Until 'an offline repeat restores sampling but leaves the PID empty' {
        $pidField = Find-Element 'Linux process ID' $editType
        $seconds = Find-Element 'Duration in seconds' $editType
        $review = Find-Element 'Review CPU profile' $button
        $pidField -and !$pidField.GetCurrentPattern($valuePattern).Current.Value -and
            $seconds -and $seconds.GetCurrentPattern($valuePattern).Current.Value -eq '2' -and
            $review -and !$review.Current.IsEnabled
    }
} finally {
    Close-FixtureApp $app
}
