[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/sftp-transfer-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_SFTP_DROP_SMOKE_ITEMS = Join-Path $State 'storage-items.json'
$profiles = Join-Path $State 'profiles.json'
$source = Join-Path $State 'sources'
$peer = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$writeGate = Join-Path $peer 'hold-transfer-write'
$button = [System.Windows.Automation.ControlType]::Button
$release = 'release ' + [char]0x96ea
[IO.File]::WriteAllText($env:W11TERM_SFTP_DROP_SMOKE_ITEMS, '[]')

function Drop-Items([string[]]$Names) {
    $paths = @($Names | ForEach-Object { Join-Path $source $_ })
    $json = ConvertTo-Json -InputObject $paths -Compress
    [IO.File]::WriteAllText($env:W11TERM_SFTP_DROP_SMOKE_ITEMS, $json)
    Click 'Drop fixture items'
}
function Wait-HeldWrite([string]$Path) {
    $pattern = '(?m)^OPEN ' + [regex]::Escape($Path) + '\r?\nWRITE 0\r?\nWRITE ([1-9][0-9]*)\r?$'
    return Wait-Until "admitted second WRITE for $Path" {
        $wire = [Text.Encoding]::UTF8.GetString((Read-PeerTrace $reader))
        $entry = [regex]::Match($wire, $pattern)
        if ($entry.Success) { [int]$entry.Groups[1].Value }
    }
}

$reader = $null
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $reader = Open-PeerTraceReader (Join-Path $peer 'transfer-trace')
    $window = Wait-InputWindow
    $fixture = Open-TransferFixture

    Drop-Items @('before-invalid', "$release/outside link")
    Wait-Status 'reparse point'
    Drop-Items @('z-lead.bin', $release, 'a-tail.bin')
    Wait-Status 'Uploaded a-tail\.bin.*3 of 3 items completed'
    $null = Wait-Until 'final listing contains the dropped file and directory' {
        (Find-Element 'a-tail.bin' $button) -and (Find-Element "$release/" $button)
    }

    Drop-Items @('replace.bin', 'after-collision')
    Wait-Status 'replace\.bin.*0 of 2 items completed'
    $existing = Join-Path $peer 'shell directory/replace.bin'
    if ([IO.File]::ReadAllText($existing) -cne 'old remote file') {
        throw 'The drop replaced a file while replacement was off'
    }
    (Wait-CheckBox 'Replace existing files' ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Wait-CheckBox 'Replace existing files' ([System.Windows.Automation.ToggleState]::On)
    [IO.File]::WriteAllText((Join-Path $source "$release/only on second drop"), 'must not merge')
    Drop-Items @('replace.bin', $release, 'after-collision')
    Wait-Status "$release.*already exists.*1 of 3 items completed"

    [IO.File]::WriteAllBytes($writeGate, [byte[]]@())
    Drop-Items @('before-cancel', 'interrupted', 'after-cancel')
    $offset = Wait-HeldWrite '/shell directory/interrupted/nested/payload.bin'
    $proof = 'if ($folderTransferPid -eq $PID) { "drop-" + "responsive" }' + [char]13
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($proof))
    $null = Control @('send-text','--pane',"$($fixture.LocalPane)",'--no-paste','--base64',$encoded)
    $null = Control @('wait-for-text','--pane',"$($fixture.LocalPane)",'--timeout','10','drop-responsive')
    Wait-Focus $fixture.Ssh.id
    Click 'Cancel transfer'
    $null = Wait-Until 'active transfer cancelled while second WRITE is pending' {
        $cancel = Find-Element 'Cancel transfer' $button
        $cancel -and !$cancel.Current.IsEnabled
    }
    [IO.File]::Delete($writeGate)
    Wait-Status "^Cancelled upload of /shell directory/interrupted.*$($offset * 2) bytes acknowledged.*remote folder may be incomplete.*1 of 3 items completed"

    [IO.File]::WriteAllBytes($writeGate, [byte[]]@())
    Drop-Items @('before-stale.bin', 'queued folder', 'after-stale')
    $null = Wait-HeldWrite '/shell directory/before-stale.bin'
    # The worker has captured the original file IDs and is blocked on a different source.
    $queued = [IO.Path]::GetFullPath((Join-Path $source 'queued folder'))
    $saved = [IO.Path]::GetFullPath((Join-Path $source 'saved original folder'))
    $fixtureRoot = [IO.Path]::GetFullPath($source) + [IO.Path]::DirectorySeparatorChar
    if (!$queued.StartsWith($fixtureRoot) -or !$saved.StartsWith($fixtureRoot)) { throw 'Invalid fixture path' }
    [IO.Directory]::Move($queued, $saved)
    $null = [IO.Directory]::CreateDirectory($queued)
    [IO.File]::WriteAllText((Join-Path $queued 'unselected replacement'), 'must not upload')
    [IO.File]::Delete($writeGate)
    Wait-Status 'dropped item changed.*1 of 3 items completed'

    Confirm-TransferShells $fixture 'mixed-drop-live'
} catch {
    $wire = if ($reader) { [Text.Encoding]::UTF8.GetString((Read-PeerTrace $reader)) }
    throw "$_ at $($_.ScriptStackTrace). Server packets: $wire"
} finally {
    [IO.File]::Delete($writeGate)
    if ($reader) { $reader.Dispose() }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
