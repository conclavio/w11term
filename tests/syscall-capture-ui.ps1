param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$destination = Join-Path $State 'stopped capture.strace'
$started = [IO.File]::ReadAllText((Join-Path $State 'syscall-start-marker'))
$saveLabel = 'Trace and save' + [char]0x2026
$saveSettings = 'Save settings' + [char]0x2026
$loadSettings = 'Load settings' + [char]0x2026
$settingsFile = Join-Path $State 'Syscall capture settings.json'
$cliSettings = Join-Path $State 'CLI trace settings.json'
$captureReport = Join-Path $State 'Stopped capture report.json'
$renamed = Join-Path $State 'Renamed saved trace.strace'
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$textType = [System.Windows.Automation.ControlType]::Text
$byText = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $textType)
$rawId = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'syscall-original-text')
$dot = ' ' + [char]0xb7 + ' '

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Set-Field([string]$Name, [string]$Text) {
    $field = Wait-Until "'$Name' editor" { Find-Element $Name $edit }
    $field.SetFocus()
    $field = Wait-Until "focused '$Name' editor" {
        $item = Find-Element $Name $edit
        if ($item -and $item.Current.HasKeyboardFocus) { $item }
    }
    $field.GetCurrentPattern($value).SetValue($Text)
    $null = Wait-Until "'$Name' contains the entered value" {
        (Find-Element $Name $edit).GetCurrentPattern($value).Current.Value -ceq $Text
    }
}

function Find-TraceText([string]$Prefix) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $byText)) {
        if ($item.Current.Name.StartsWith($Prefix)) { $item; return }
    }
}

function Choose-Settings([string]$Path, [switch]$Save) {
    $title = if ($Save) { 'Save syscall capture settings' } else { 'Open syscall capture settings' }
    if ($Save) { Click $saveSettings } else { Click $loadSettings }
    $dialog = Wait-OwnedDialog $title
    if (!$Path) {
        $null = Wait-Until 'cancel the settings file picker' { [FixtureDialog]::ClickButton($dialog, 2) }
        return
    }
    $null = Wait-Until 'enter the settings filename' {
        if ($Save) { [FixtureDialog]::SetCommonDialogFilename($dialog, $Path) }
        else { [FixtureDialog]::SetFilename($dialog, $Path, '') }
    }
    $null = Wait-Until 'accept the settings filename' { [FixtureDialog]::ClickButton($dialog, 1) }
}

function Choose-Report([string]$Path, [string]$Trace, [switch]$Save) {
    Set-NativeExpansion 'Syscall capture report' $true
    $ellipsis = [char]0x2026
    if ($Save) { Click "Save capture report$ellipsis" } else { Click "Verify capture report$ellipsis" }
    $title = if ($Save) { 'Save syscall capture report' } else { 'Open syscall capture report' }
    $dialog = Wait-OwnedDialog $title
    $suggestion = [IO.Path]::GetFileName($Trace) + '.json'
    $null = Wait-Until "enter the filename in '$title'" {
        [FixtureDialog]::SetFilename($dialog, $Path, $suggestion) -or
            [FixtureDialog]::SetCommonDialogFilename($dialog, $Path)
    }
    $null = Wait-Until 'accept the report filename' { [FixtureDialog]::ClickButton($dialog, 1) }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $local = (Sessions | Where-Object label -eq 'Palette PowerShell').id
    $remote = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $remote
    $null = Control @('wait-ssh-connected','--pane',"$remote",'--timeout','15')
    Run-Palette 'Syscall trace viewer'
    Click ('Capture over SSH' + [char]0x2026)
    Set-Field 'Working directory (optional)' '/home/test'
    Set-Field 'Arguments (JSON array)' '["one argument","$literal","--flag"]'
    Set-Field 'Time limit (seconds)' '30'
    Set-Field 'Trace output limit (MiB)' '1'
    Set-Field 'Linux executable' '/bin/sh'
    Select-NativeComboItem 'Syscall preset' 'Files and descriptors'
    Click 'Review syscall capture'
    $null = Wait-Until 'command and initial bounds were reviewed' {
        (Find-Element 'Executable /bin/sh') -and
            (Find-Element 'Working directory /home/test') -and
            (Find-Element "30 seconds${dot}1 MiB${dot}128 bytes per string") -and
            (Find-TraceText 'Host key SHA256:')
    }
    if ([IO.File]::Exists($started)) { throw 'Review launched the remote command' }

    Choose-Settings $settingsFile -Save
    $null = Wait-Until 'the settings file is saved and the existing review is still available' {
        $prefix = "Saved syscall capture settings$dot"
        $notice = Find-TraceText $prefix
        $review = Find-Element $saveLabel $button
        $notice -and $review -and $review.Current.IsEnabled -and
            [FixtureDialog]::SameFile($settingsFile, $notice.Current.Name.Substring($prefix.Length))
    }
    $export = [IO.File]::ReadAllText($settingsFile)
    $settings = $export | ConvertFrom-Json
    if ($settings.schema_version -ne 1 -or
        @($settings.PSObject.Properties.Name).Count -ne 2 -or
        @($settings.options.PSObject.Properties.Name).Count -ne 7 -or
        $settings.options.program -cne '/bin/sh' -or
        $settings.options.directory -cne '/home/test' -or
        @($settings.options.arguments).Count -ne 3 -or
        $settings.options.arguments[0] -cne 'one argument' -or
        $settings.options.arguments[1] -cne '$literal' -or
        $settings.options.arguments[2] -cne '--flag' -or
        $settings.options.seconds -ne 30 -or $settings.options.max_mib -ne 1 -or
        $settings.options.string_size -ne 128 -or $settings.options.filter -cne 'files') {
        throw 'Windows exported different syscall inputs or included execution state'
    }
    $cliPreview = (Control @('preview-syscall-capture','--pane',"$remote",'--settings-file',$settingsFile)).plan
    if ($cliPreview.options.filter -cne 'files' -or
        ($cliPreview.options.arguments | ConvertTo-Json -Compress) -cne ($settings.options.arguments | ConvertTo-Json -Compress) -or
        $cliPreview.options.string_size -ne 128) {
        throw 'The CLI preview did not restore the Windows settings'
    }

    Choose-Settings $settingsFile -Save
    $null = Wait-Until 'the export refuses to replace an existing settings file' {
        Find-TraceText 'Saving new syscall capture settings'
    }
    if ([IO.File]::ReadAllText($settingsFile) -cne $export) { throw 'Saving replaced a settings file' }
    Choose-Settings
    $null = Wait-Until 'cancelling the settings picker preserves the review' {
        $review = Find-Element $saveLabel $button
        $review -and $review.Current.IsEnabled
    }
    $invalid = Join-Path $State 'invalid capture settings.json'
    $settings | Add-Member -NotePropertyName 'review_id' -NotePropertyValue 'stale-review'
    [IO.File]::WriteAllText($invalid, ($settings | ConvertTo-Json -Depth 8))
    Choose-Settings $invalid
    $null = Wait-Until 'the rejected settings leave the original review available' {
        $error = Find-TraceText 'Reading syscall capture settings JSON'
        $review = Find-Element $saveLabel $button
        $error -and $review -and $review.Current.IsEnabled
    }

    $createdSettings = & $Cli save-syscall-capture-settings --program /bin/sh --directory /home/test `
        --arg 'one argument' --arg '$literal' --arg '--flag' --seconds 30 --max-mib 1 --string-size 64 `
        --filter all --output-file $cliSettings | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or !$createdSettings.ok -or
        ![FixtureDialog]::SameFile($cliSettings, $createdSettings.result.path)) {
        throw 'Could not make the CLI settings fixture'
    }
    Choose-Settings $cliSettings
    $null = Wait-Until 'loading CLI settings replaces inputs and clears the previous review' {
        $prefix = "Loaded syscall capture settings$dot"
        $notice = Find-TraceText $prefix
        $field = Find-Element 'Maximum string bytes' $edit
        $combo = Find-Element 'Syscall preset' ([System.Windows.Automation.ControlType]::ComboBox)
        $preset = if ($combo) {
            $combo.GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
        }
        $notice -and [FixtureDialog]::SameFile($cliSettings, $notice.Current.Name.Substring($prefix.Length)) -and
            $field -and $field.GetCurrentPattern($value).Current.Value -ceq '64' -and
            $preset -and $preset[0].Current.Name -ceq 'All calls' -and
            !(Find-Element $saveLabel $button)
    }
    if ([IO.File]::Exists($started)) { throw 'Saving or loading settings started the trace' }
    Click 'Review syscall capture'
    $null = Wait-Until 'new string size was reviewed' {
        Find-Element "30 seconds${dot}1 MiB${dot}64 bytes per string"
    }
    Set-Field 'Maximum string bytes' '96'
    $null = Wait-Until 'editing a bound invalidates the loaded settings review' { !(Find-Element $saveLabel $button) }
    Set-Field 'Maximum string bytes' '64'
    Click 'Review syscall capture'
    Click $saveLabel
    $dialog = Wait-Until 'owned syscall trace save dialog' { Find-OwnedDialog 'Save syscall trace' }
    if ([IO.File]::Exists($started)) { throw 'The command started before the destination was approved' }
    $null = Wait-Until 'enter the trace output path' { [FixtureDialog]::SetFilename($dialog, $destination, 'syscall-trace.strace') }
    $null = Wait-Until 'confirm the trace output path' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'remote trace worker started' { [IO.File]::Exists($started) }
    $null = Control @('focus-pane','--pane',"$local")
    Wait-Focus $local
    Run-Palette 'Diagnostics overview'
    $null = Wait-Until 'running syscall capture is visible from PowerShell' {
        (Find-Element 'Open Syscall capture activity' $button) -and (Find-Element 'Running')
    }
    Click 'Open Syscall capture activity'
    Click 'Stop syscall capture'
    $null = Wait-Until 'verified stopped trace is displayed in the viewer' {
        if (![IO.File]::Exists($destination)) { return }
        $saved = Find-TraceText 'Saved syscall trace'
        if (!$saved -or !$saved.Current.Name.Contains('Stopped by request')) { return }
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $original = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $rawId)
        $original -and $original.Current.Name.Contains('ENOENT')
    }
    $null = Control @('focus-pane','--pane',"$local")
    Wait-Focus $local
    Run-Palette 'Diagnostics overview'
    Wait-DiagnosticArtifact 'Saved syscall trace' $destination 'Stopped by request'
    Click 'Open Syscall capture activity'
    Choose-Report $captureReport $destination -Save
    $null = Wait-Until 'the new capture report is saved' {
        $prefix = "Saved syscall capture report$dot"
        $notice = Find-TraceText $prefix
        $notice -and [FixtureDialog]::SameFile($captureReport, $notice.Current.Name.Substring($prefix.Length))
    }
    $verified = & $Cli verify-syscall-capture-report $destination $captureReport | ConvertFrom-Json
    $report = $verified.result.report
    if ($LASTEXITCODE -ne 0 -or !$verified.ok -or $report.stop_reason -cne 'stopped' -or
        $report.reviewed.program -cne '/bin/sh' -or $report.reviewed.directory -cne '/home/test' -or
        $report.reviewed.options.string_size -ne 64) {
        throw 'The Windows report did not verify with the reviewed options in the CLI'
    }

    [IO.File]::Copy($destination, $renamed)
    Click ('Open syscall trace' + [char]0x2026)
    $dialog = Wait-OwnedDialog 'Open syscall trace'
    $null = Wait-Until 'enter the renamed trace path' { [FixtureDialog]::SetFilename($dialog, $renamed, '') }
    $null = Wait-Until 'open the renamed trace' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the renamed trace is open' { Find-TraceText 'Renamed saved trace.strace' }
    Set-NativeExpansion 'Syscall capture report' $true
    if (Find-Element ('Save capture report' + [char]0x2026) $button) {
        throw 'An imported trace inherited report export from the original capture'
    }
    Choose-Report $captureReport $renamed
    $null = Wait-Until 'the renamed trace matches the report and shows the recorded details' {
        (Find-TraceText "Trace file matches report$dot") -and
            (Find-Element 'Executable /bin/sh') -and
            (Find-Element 'Working directory /home/test') -and
            (Find-TraceText 'Recorded SSH ') -and
            (Find-Element "SHA-256 $($report.sha256)")
    }
    $modified = [IO.File]::ReadAllBytes($renamed)
    $modified[0] = [byte]($modified[0] -bxor 1)
    [IO.File]::WriteAllBytes($renamed, $modified)
    Choose-Report $captureReport $renamed
    $null = Wait-Until 'a modified trace is rejected and clears the old verification' {
        (Find-TraceText 'Trace SHA-256 differs from the syscall capture report') -and
            !(Find-TraceText 'Recorded SSH ')
    }
} finally {
    Close-FixtureApp $app
}
