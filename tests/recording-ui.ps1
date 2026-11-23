[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$folder = Join-Path $State 'captures'
$env:W11TERM_RECORDING_SMOKE_DIR = $folder
$profiles = Join-Path $State 'profiles.json'
$timed = Join-Path $folder ('Flight ' + [char]0x96ea + ' log.vt')
$plain = Join-Path $folder 'ordinary.vt'
$cancelled = Join-Path $folder 'cancelled.vt'
$esc = [char]27

function Recording-Dialog([string]$Title, [switch]$Menu) {
    $label = $Title + [char]0x2026
    if ($Menu) {
        Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
        $group = Wait-Until 'Recordings submenu' { Find-Element 'Recordings' ([System.Windows.Automation.ControlType]::MenuItem) }
        $group.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
        Invoke-Button (Wait-Until "$label menu item" { Find-Element $label ([System.Windows.Automation.ControlType]::MenuItem) })
    } else {
        $field = Open-Palette $label
        $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($label)
        $entry = Wait-Until "enabled palette command $label" {
            $item = Find-Element $label ([System.Windows.Automation.ControlType]::Button)
            if ($item -and $item.Current.IsEnabled) { $item }
        }
        Invoke-Button $entry
    }
    return Wait-Until "native $Title picker owned by this window" { Find-OwnedDialog $Title }
}
function Set-RecordingPath([IntPtr]$Dialog, [string]$Destination) {
    $null = Wait-Until 'save picker filename initialized' {
        [FixtureDialog]::SetFilename($Dialog, $Destination, 'session.vt')
    }
}
function Close-Picker([IntPtr]$Dialog, [string]$Title, [int]$Button) {
    $null = Wait-Until "$Title button $Button ready" { [FixtureDialog]::ClickButton($Dialog, $Button) }
    $null = Wait-Until "$Title picker dismissed" { !(Find-OwnedDialog $Title) }
}
function Select-Recording([IntPtr]$Dialog, [string]$Path) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($Dialog)
    $byPattern = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::IsSelectionItemPatternAvailableProperty, $true)
    $names = @([IO.Path]::GetFileName($Path), [IO.Path]::GetFileNameWithoutExtension($Path))
    $name = Wait-Until "picker to list $($names[0])" {
        foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$byPattern)) {
            if ($item.Current.Name -cin $names) { return $item.Current.Name }
        }
    }
    Select-DialogItem $Dialog $name
}
function Recording-Events([int]$Pane, [string]$Kind) {
    (Control @('events','--pane',"$Pane",'--since','0','--timeout','0')).journal.events |
        Where-Object kind -ceq $Kind
}
function Wait-RecordingStatus([string]$Prefix, [string]$Path) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $last = @{ names = @() }
    try {
        $null = Wait-Until "$Prefix$Path" {
            $last.names = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.Condition]::TrueCondition) | ForEach-Object { $_.Current.Name } |
                Where-Object { $_ -like 'Recording pane *' -or $_ -like 'Saved recording for pane *' })
            foreach ($name in $last.names) {
                if ($name.StartsWith($Prefix) -and [FixtureDialog]::SameFile($Path,$name.Substring($Prefix.Length))) { return $true }
            }
        }
    } catch { throw "$($_.Exception.Message). Visible recording statuses: $($last.names -join ' | ')" }
}
function Send-Remote([string]$Text) {
    $b64 = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($Text))
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64',$b64)
}
function Wait-Text([int]$Pane, [string]$Text) {
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15',$Text)
}
function Get-ShellPid([string]$Tag) {
    $line = "'$Tag=' + " + '$PID'
    foreach ($ch in $line.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    Press-Key 13
    $pattern = "$Tag=([0-9]+)"
    $reply = Control @('wait-for-text','--pane',"$localPane",'--regex','--timeout','15',$pattern)
    return [int]([regex]::Match($reply.text,$pattern).Groups[1].Value)
}
function Open-Recording([string]$Path, [string]$Marker) {
    $previous = @((Control @('list')).sessions.id)
    $picker = Recording-Dialog 'Open recording'
    Select-Recording $picker $Path
    Close-Picker $picker 'Open recording' 1
    $newPane = Wait-Until 'one newly focused replay tab' {
        $sessions = @((Control @('list')).sessions)
        $new = @($sessions | Where-Object { $_.id -notin $previous })
        if ($sessions.Count -eq $previous.Count + 1 -and $new.Count -eq 1 -and $new[0].active) { $new[0] }
    }
    if ($newPane.tab_id -in @($originalTab, $localTab)) { throw 'Replay replaced an existing tab' }
    Wait-Text $newPane.id $Marker
    return $newPane.id
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','72','--rows','20') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $ssh = (Control @('list')).sessions[0]
    $remote = $ssh.id
    $originalTab = $ssh.tab_id
    Wait-Text $remote 'ready'

    $dialog = Recording-Dialog 'Start timed recording'
    Set-RecordingPath $dialog $cancelled
    Close-Picker $dialog 'Start timed recording' 2
    if (@(Get-ChildItem -LiteralPath $folder).Count -or @(Recording-Events $remote 'recording-started').Count) {
        throw 'Canceling the save picker started a capture'
    }
    $dialog = Recording-Dialog 'Open recording' -Menu
    Close-Picker $dialog 'Open recording' 2
    if (@((Control @('list')).sessions).Count -ne 1) { throw 'Canceling the open picker created a tab' }

    $dialog = Recording-Dialog 'Start timed recording'
    Set-RecordingPath $dialog $timed
    Send-Remote 'before-save-picker-accepted'
    Wait-Text $remote 'before-save-picker-accepted'
    # Switch the active tab through the control pipe while the native picker is open.
    $localPane = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $localPane
    $localTab = ((Control @('list')).sessions | Where-Object id -eq $localPane).tab_id
    Close-Picker $dialog 'Start timed recording' 1
    $null = Wait-Until 'GUI recording starts on the original SSH pane' {
        $events = @(Recording-Events $remote 'recording-started')
        $events.Count -eq 1 -and [FixtureDialog]::SameFile($timed, [string]$events[0].data.path)
    }
    Wait-Text $localPane 'PS '
    Wait-Focus $localPane
    $shellPid = Get-ShellPid 'recording-before'
    Send-Remote "${esc}[2J${esc}[HSSH-TIMED-CAPTURE"
    Wait-Text $remote 'SSH-TIMED-CAPTURE'
    $stopped = Control @('record','--pane',"$remote",'--stop')
    if ($stopped.recording -or ![FixtureDialog]::SameFile($timed, $stopped.path) -or
        !(Test-Path -LiteralPath ([IO.Path]::ChangeExtension($timed,'vt.jsonl')))) { throw 'CLI did not finalize the GUI timed recording' }
    $raw = [IO.File]::ReadAllText($timed)
    if (!$raw.Contains('SSH-TIMED-CAPTURE') -or $raw.Contains('before-save-picker-accepted') -or $raw.Contains('recording-before')) {
        throw 'GUI capture included output outside the selected SSH pane or recording interval'
    }

    $null = Control @('record','--pane',"$remote",$plain)
    Send-Remote "${esc}[2J${esc}[HSSH-RAW-CAPTURE"
    Wait-Text $remote 'SSH-RAW-CAPTURE'
    $null = Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    Wait-RecordingStatus "Recording pane $remote to " $plain
    Run-Palette 'Stop recording'
    $null = Wait-Until 'GUI finalizes the CLI recording' { @(Recording-Events $remote 'recording-stopped').Count -eq 2 }
    Wait-RecordingStatus "Saved recording for pane $remote to " $plain
    if (![IO.File]::ReadAllText($plain).Contains('SSH-RAW-CAPTURE') -or
        (Test-Path -LiteralPath ([IO.Path]::ChangeExtension($plain,'vt.jsonl')))) { throw 'GUI changed the CLI recording format' }

    $timedPane = Open-Recording $timed 'SSH-TIMED-CAPTURE'
    $null = Wait-Until 'the indexed recording opens in timed mode' {
        ((Control @('list')).sessions | Where-Object id -eq $timedPane).status -like 'Timed replay complete*'
    }
    $plainPane = Open-Recording $plain 'SSH-RAW-CAPTURE'
    $plainStatus = ((Control @('list')).sessions | Where-Object id -eq $plainPane).status
    if ($plainStatus -like 'Timed replay*') { throw "Unindexed VT capture opened in timed mode: $plainStatus" }

    $null = Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $localPane
    if ((Get-ShellPid 'recording-after') -ne $shellPid) { throw 'Opening a recording restarted PowerShell' }
    if (@(Recording-Events $localPane 'recording-started').Count) { throw 'PowerShell was recorded during the SSH capture' }
    Send-Remote 'ssh-still-live-after-open'
    Wait-Text $remote 'ssh-still-live-after-open'
} catch {
    throw "$($_.Exception.Message) at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
