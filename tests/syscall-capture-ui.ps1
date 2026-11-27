param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$destination = Join-Path $State 'stopped capture.strace'
$started = [IO.File]::ReadAllText((Join-Path $State 'syscall-start-marker'))
$saveLabel = 'Trace and save' + [char]0x2026
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
    Click 'Review syscall capture'
    $null = Wait-Until 'command and initial bounds were reviewed' {
        (Find-Element 'Executable /bin/sh') -and
            (Find-Element 'Working directory /home/test') -and
            (Find-Element "30 seconds${dot}1 MiB${dot}128 bytes per string") -and
            (Find-TraceText 'Host key SHA256:')
    }
    if ([IO.File]::Exists($started)) { throw 'Review launched the remote command' }

    Set-Field 'Maximum string bytes' '64'
    $null = Wait-Until 'editing a bound invalidates the first review' { !(Find-Element $saveLabel $button) }
    Click 'Review syscall capture'
    $null = Wait-Until 'new string size was reviewed' {
        Find-Element "30 seconds${dot}1 MiB${dot}64 bytes per string"
    }
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
    $null = Wait-Until 'overview shows the saved trace and original stop reason' {
        $summary = Find-TraceText 'Saved syscall trace'
        $summary -and $summary.Current.Name.Contains('Stopped by request') -and
            $summary.Current.Name.EndsWith($destination)
    }
} finally {
    Close-FixtureApp $app
}
