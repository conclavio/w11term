[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$verifyClipboard = $env:GITHUB_ACTIONS -ceq 'true' -and $env:RUNNER_ENVIRONMENT -ceq 'github-hosted'
if ($verifyClipboard) { . (Join-Path $PSScriptRoot 'support/clipboard.ps1') }
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$picker = Join-Path $State 'picker'
$env:W11TERM_SFTP_PICKER_SMOKE_DIR = $picker
$null = New-Item -ItemType Directory -Path $picker
$profiles = Join-Path $State 'profiles.json'
$fixture = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$button = [System.Windows.Automation.ControlType]::Button
$properties = 'Properties' + [char]0x2026
$snow = [char]0x96ea
$filename = "pilot $snow.bin"
$parent = '/shell directory'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled $Name" {
        $b = Find-Element $Name $button
        if ($b -and $b.Current.IsEnabled) { $b }
    })
}
function Open-Properties([string[]]$Expected) {
    Click $properties
    $field = Wait-Until 'read-only file properties displayed' {
        $b = Find-Element 'File properties'
        if ($b -and $b.Current.AutomationId -eq 'sftp-properties-text') { $b }
    }
    $pattern = $field.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
    $lines = $pattern.DocumentRange.GetText(-1).Replace("`r`n","`n").Replace("`r","`n").Split([char]10)
    foreach ($line in $Expected) {
        if ($lines -cnotcontains $line) { throw "Missing property '$line'. Displayed: $($lines -join ' | ')" }
    }
    $value = $null
    if ($field.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern,[ref]$value) -and !$value.Current.IsReadOnly) { throw 'Properties exposed writable file metadata' }
    if (Find-Element 'Edit' $button) { throw 'Properties offered Edit' }
    $checksum = Find-Element 'Calculate SHA-256' $button
    if ([bool]$checksum -ne ($Expected -contains 'Type  Regular file')) {
        throw 'The SHA-256 action must be available only for confirmed regular files'
    }
    Click 'Close properties'
    $null = Wait-Until 'properties dialog dismissed' { !(Find-Element 'Close properties' $button) }
}
function Shell-Pid([string]$Tag) {
    $line = "'$Tag=' + " + '$PID'
    foreach ($ch in $line.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    Press-Key 13
    $pattern = "$Tag=([0-9]+)"
    $reply = Control @('wait-for-text','--pane',"$localPane",'--regex','--timeout','15',$pattern)
    return [int]([regex]::Match($reply.text,$pattern).Groups[1].Value)
}
function Download-WithChecksum([string]$Destination, [string]$Expected) {
    Click 'Use for verified download'
    $field = Wait-Until 'prefilled expected SHA-256' { Find-Element 'Expected SHA-256' }
    if ($field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -cne $Expected) {
        throw 'Verified download did not reuse the calculated digest'
    }
    (Wait-CheckBox 'Resume interrupted download' ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Wait-CheckBox 'Resume interrupted download' ([System.Windows.Automation.ToggleState]::On)
    Click 'Choose destination'
    $dialog = Wait-Until 'owned verified download save picker' { Find-OwnedDialog 'Save As' }
    $null = Wait-Until 'enter verified download destination' {
        [FixtureDialog]::SetFilename($dialog, $Destination, $filename)
    }
    $null = Wait-Until 'accept verified download destination' { [FixtureDialog]::ClickButton($dialog, 1) }
}
function Select-ComparisonFile([string]$Path) {
    $title = 'Choose a local file to compare'
    Click ('Compare with local file' + [char]0x2026)
    $dialog = Wait-Until 'owned checksum comparison picker' { Find-OwnedDialog $title }
    if ($Path) {
        $null = Wait-Until 'enter local comparison file' { [FixtureDialog]::SetFilename($dialog, $Path, '') }
    }
    $id = if ($Path) { 1 } else { 2 }
    $null = Wait-Until 'dismiss comparison picker' { [FixtureDialog]::ClickButton($dialog, $id) }
    $null = Wait-Until 'comparison picker closed' { !(Find-OwnedDialog $title) }
}
function Get-Digest([string]$Path) {
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { -join ($hasher.ComputeHash([IO.File]::ReadAllBytes($Path)) | ForEach-Object { $_.ToString('x2') }) }
    finally { $hasher.Dispose() }
}
function Assert-Comparison([string]$Result, [string]$Path) {
    $summary = "$Result " + [char]0x00b7 + " " + [IO.Path]::GetFileName($Path)
    $null = Wait-Until $summary {
        $status = Find-Element $summary
        if ($status -and $status.Current.AutomationId -eq 'sftp-local-checksum-status') { $status }
    }
    $field = Wait-Until 'selectable local SHA-256 digest' {
        $element = Find-Element 'Local SHA-256 digest'
        if ($element -and $element.Current.AutomationId -eq 'sftp-local-checksum') { $element }
    }
    $expected = Get-Digest $Path
    $actual = $field.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1).Trim()
    if ($actual -cne "Local SHA-256  $expected") { throw 'The dialog displayed the wrong local digest' }
}
function Wait-TransferOutcome([string]$Outcome, [string]$Message) {
    $null = Wait-Until "transfer result $Outcome" {
        $events = (Control @('events','--pane',"$remote",'--since','0','--timeout','0')).journal.events
        $events | Where-Object { $_.kind -ceq 'sftp-transfer' -and
            $_.data.outcome -ceq $Outcome -and $_.data.message -like "*$Message*" }
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    $ssh = (Control @('list')).sessions | Where-Object id -eq $remote
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','20','PS ')
    $pidBefore = Shell-Pid 'properties-before'
    $null = Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    Run-Palette 'Open files here'
    $null = Wait-Until 'directory listed, Properties disabled before selection' {
        $b = Find-Element $properties $button
        (Find-Element $filename $button) -and $b -and !$b.Current.IsEnabled
    }

    # Start the wire trace after the listing and change the file outside the SSH
    # connection. The old directory entry reported one byte.
    [IO.File]::WriteAllBytes((Join-Path $fixture 'properties-trace'),[byte[]]@())
    $disk = Join-Path (Join-Path $fixture 'shell directory') $filename
    [IO.File]::WriteAllBytes($disk,[byte[]](0..31))
    [IO.File]::SetLastWriteTimeUtc($disk,[DateTime]::new(2001,2,3,4,5,6,[DateTimeKind]::Utc))
    Click $filename
    Open-Properties @("Path  $parent/$filename",'Type  Regular file','Size  32 bytes','Modified  2001-02-03 04:05:06 UTC','Permissions  0777','Owner UID  Not reported','Group GID  Not reported')
    [IO.File]::WriteAllBytes($disk,[byte[]](0..56))
    # Reopening without selecting a row proves the same selection survived Close.
    Open-Properties @("Path  $parent/$filename",'Type  Regular file','Size  57 bytes')
    Click 'hangar/'
    Open-Properties @("Path  $parent/hangar",'Type  Directory')
    Click 'telemetry-link'
    Open-Properties @("Path  $parent/telemetry-link",'Type  Symbolic link',("Link target  ../archive/" + [char]0x822a + "/$snow"))
    Click 'sparse-meta'
    Open-Properties @("Path  $parent/sparse-meta",'Type  Not reported','Size  Not reported','Modified  Not reported','Accessed  Not reported','Permissions  Not reported','Owner UID  Not reported','Group GID  Not reported')
    if ([IO.File]::ReadAllText((Join-Path $fixture 'transfer-trace')).Length -ne 0) {
        throw 'Viewing properties opened a file before requesting its checksum'
    }
    $expectedDigest = Get-Digest $disk
    Click $filename
    Click $properties
    Click 'Calculate SHA-256'
    $digestField = Wait-Until 'selectable SHA-256 digest in the dialog' {
        $b = Find-Element 'SHA-256 digest'
        if ($b -and $b.Current.AutomationId -eq 'sftp-checksum-text') { $b }
    }
    $digestText = $digestField.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1).Trim()
    if ($digestText -cne $expectedDigest) { throw 'The checksum dialog displayed the wrong digest' }
    if ($verifyClipboard) {
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Click 'Copy SHA-256'
        $null = Wait-Until 'exact SHA-256 digest copied' {
            [FixtureClipboard]::ContainsNewText($before, $expectedDigest)
        }
    } elseif (!(Find-Element 'Copy SHA-256' $button)) {
        throw 'The checksum dialog lacked a copy action'
    }
    $matching = Join-Path $picker "local $snow [match].bin"
    $different = Join-Path $picker 'different.bin'
    [IO.File]::Copy($disk, $matching)
    [IO.File]::WriteAllBytes($different, [byte[]](1..57))
    Select-ComparisonFile $matching
    Assert-Comparison 'Match' $matching
    Select-ComparisonFile $different
    Assert-Comparison 'Different' $different
    Select-ComparisonFile ''
    Assert-Comparison 'Different' $different
    $downloaded = Join-Path $picker 'verified.bin'
    Download-WithChecksum $downloaded $expectedDigest
    Wait-TransferOutcome 'completed' 'SHA-256 verified'
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($downloaded)) -cne
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($disk))) {
        throw 'The locally published file did not match the bytes hashed'
    }
    $sha = Control @('sftp','--pane',"$remote",'sha256',"$parent/$filename",'--max-bytes','57')
    if ($sha.sftp.sha256 -cne $expectedDigest -or $sha.sftp.bytes -ne 57) {
        throw 'The CLI returned an unexpected checksum or byte count'
    }
    Click $properties
    Click 'Calculate SHA-256'
    $null = Wait-Until 'digest available before the server file changes' { Find-Element 'SHA-256 digest' }
    [IO.File]::WriteAllBytes($disk,[byte[]](1..57))
    $rejected = Join-Path $picker 'changed.bin'
    Download-WithChecksum $rejected $expectedDigest
    Wait-TransferOutcome 'failed' 'SHA-256 mismatch'
    if ([IO.File]::Exists($rejected)) { throw 'A changed remote file was installed locally' }
    $actions = Find-Element 'SFTP actions'
    $close = [System.Windows.Automation.AndCondition]::new(
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty,'Close'),
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty,$button))
    # The dialog returns focus to the file drawer. Use its Close button, scoped
    # to the drawer so it cannot invoke the window caption's Close button.
    Invoke-Button (Wait-Until 'drawer Close button' { $actions.FindFirst([System.Windows.Automation.TreeScope]::Descendants,$close) })
    $null = Wait-Until 'drawer closed' { !(Find-Element $properties $button) }
    Wait-Focus $remote
    foreach ($ch in 'ssh-properties-still-live'.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ssh-properties-still-live')
    $current = (Control @('list')).sessions | Where-Object id -eq $remote
    if ($current.connection_generation -ne $ssh.connection_generation -or $current.tab_id -ne $ssh.tab_id) { throw 'Properties replaced the SSH pane' }
    $null = Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $localPane
    if ((Shell-Pid 'properties-after') -ne $pidBefore) { throw 'Properties interrupted the neighboring PowerShell' }
} catch {
    $failure = $_
    $packets = Get-Content -LiteralPath (Join-Path $fixture 'transfer-trace') -Raw -ErrorAction SilentlyContinue
    $metadata = Get-Content -LiteralPath (Join-Path $fixture 'properties-trace') -Raw -ErrorAction SilentlyContinue
    throw "$($failure.Exception.Message) at $($failure.ScriptStackTrace). Transfer packets: $packets Metadata packets: $metadata"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
