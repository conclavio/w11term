# Drive the native GUI against the disposable OpenSSH/SSHFS guest. All test
# data lives below State; the guest's client key is outside the exported tree.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Gui,
    [Parameter(Mandatory)][string]$Cli,
    [Parameter(Mandatory)][string]$State,
    [Parameter(Mandatory)][int]$Port,
    [Parameter(Mandatory)][string]$Identity,
    [Parameter(Mandatory)][string]$Fingerprint,
    [switch]$CheckClipboard
)
$ErrorActionPreference = 'Stop'
if ($CheckClipboard -and ($env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_ENVIRONMENT -cne 'github-hosted')) {
    throw 'The clipboard check requires an ephemeral GitHub-hosted runner'
}
. (Join-Path $PSScriptRoot '../support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot '../support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
# ui-smoke changes only the folder picker's initial directory.
$env:W11TERM_FOLDER_SHARE_SMOKE_DIR = $State
$leaf = 'Mounted UI fixture'
$local = [IO.Directory]::CreateDirectory((Join-Path $State $leaf)).FullName
$token = [guid]::NewGuid().ToString('N')
$name = "ui-$token"
$expected = "/home/w11fixture/windows/$name"
$witness = "windows-$token.bin"
$bytes = [byte[]](0,255,27,0,195,169,26,13,10)
$source = Join-Path $local $witness
[IO.File]::WriteAllBytes($source, $bytes)
$profiles = Join-Path $State 'profiles.json'
$ssh = @{ host = '127.0.0.1'; port = $Port; user = 'w11fixture'; auth = 'identity'; identity = $Identity; host_key = $Fingerprint }
$catalog = @{ profiles = @{ 'guest-ssh' = $ssh } } | ConvertTo-Json -Depth 5
[IO.File]::WriteAllText($profiles, $catalog, [Text.UTF8Encoding]::new($false))
$button = [System.Windows.Automation.ControlType]::Button
function Enabled([string]$Name) {
    $element = Find-Element $Name $button
    if ($element -and $element.Current.IsEnabled) { $element }
}
function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled $Name" { Enabled $Name })
}
$log = Join-Path $State 'gui.log'
$app = Start-Process -FilePath $Gui -WorkingDirectory $State -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','guest-ssh') -WindowStyle Hidden -RedirectStandardError $log -PassThru
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $session = Wait-Until 'verified connected SSH pane' {
        (Control @('list')).sessions | Where-Object { $_.label -eq 'guest-ssh' -and $_.connection_generation -gt 0 }
    }
    $pane = $session.id
    $cwd = (Control @('sftp','--pane',"$pane",'realpath','.')).sftp.entries[0].filename
    if ($cwd -cne '/tmp') { throw "The guest must start SFTP away from the mount. Reported '$cwd'." }
    Click 'Tab menu'
    Invoke-Button (Wait-Until 'enabled shared folders menu item' {
        $item = Find-Element ('Shared folders' + [char]0x2026) ([System.Windows.Automation.ControlType]::MenuItem)
        if ($item -and $item.Current.IsEnabled) { $item }
    })
    Click ('Add folder' + [char]0x2026)
    $dialog = Wait-Until 'owned share picker' { Find-OwnedDialog 'Choose a folder to share' }
    Select-DialogItem $dialog $leaf
    $null = Wait-Until 'confirm selected directory' { [FixtureDialog]::ClickButton($dialog, 1) }
    $edit = Wait-Until 'mount name' { Find-Element 'Mount name' ([System.Windows.Automation.ControlType]::Edit) }
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($name)
    $null = Wait-Until 'share confirmation for the edited mount name' { Find-Element "Remember this folder and mount it read-only at ~/windows/$name" }
    (Wait-CheckBox 'Allow write access' Off).Toggle()
    $null = Wait-Until 'explicit writable consent' { Find-Element "Remember this folder and mount it read/write at ~/windows/$name" }
    Click 'Save folder'
    $copy = "Copy Linux path for $name"
    $open = "Open $name in SFTP drawer"
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    while (!(Find-Element $expected) -or !(Enabled $copy) -or !(Enabled $open)) {
        if ($app.HasExited -or [DateTime]::UtcNow -gt $deadline) {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            $type = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::ControlTypeProperty,[System.Windows.Automation.ControlType]::Text)
            $status = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$type) | ForEach-Object { $_.Current.Name })
            throw "SSHFS mount actions did not become available. Panel text: $($status -join ', ')"
        }
        Start-Sleep -Milliseconds 100
    }
    $null = Wait-Until 'saved write access' { Find-Element 'Read/write' }
    (Wait-CheckBox "Mount on connect for $name" On).Toggle()
    $null = Wait-CheckBox "Mount on connect for $name" Off
    if (!(Find-Element $expected) -or !(Enabled $open) -or !(Enabled $copy)) {
        throw 'Disabling mount on connect changed the live share or its actions'
    }
    if ($CheckClipboard) {
        . (Join-Path $PSScriptRoot '../support/clipboard.ps1')
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Click $copy
        $null = Wait-Until 'the exact Linux path on the native clipboard' {
            [FixtureClipboard]::ContainsNewText($before,$expected)
        }
        $null = Wait-Until 'copy leaves the row in Shared folders' { (Find-Element "Copied the Linux path for $name.") -and (Enabled $open) }
    }
    Click $open
    $null = Wait-Until 'Files drawer lists the Windows-only file at the absolute Linux mount path' {
        (Find-Element "w11fixture@127.0.0.1:$Port") -and (Find-Element $expected) -and (Enabled $witness)
    }
    Wait-Focus $pane
    $again = @((Control @('list')).sessions)
    if ($again.Count -ne 1 -or $again[0].connection_generation -ne $session.connection_generation) {
        throw 'Opening Files replaced the verified SSH session'
    }
    $download = Join-Path $State 'download.bin'
    $null = Control @('sftp','--pane',"$pane",'get',"$expected/$witness",$download)
    if (![Collections.StructuralComparisons]::StructuralEqualityComparer.Equals([IO.File]::ReadAllBytes($download), $bytes)) {
        throw 'SFTP did not read the Windows fixture through the Linux mount'
    }
    $upload = Join-Path $State 'upload.bin'
    $payload = [byte[]](240,159,146,190,0,255,10)
    [IO.File]::WriteAllBytes($upload, $payload)
    $null = Control @('sftp','--pane',"$pane",'put',$upload,"$expected/from-linux.bin")
    if (![Collections.StructuralComparisons]::StructuralEqualityComparer.Equals([IO.File]::ReadAllBytes((Join-Path $local 'from-linux.bin')), $payload)) {
        throw 'An upload through the Linux mount did not reach the selected Windows directory'
    }
    Write-Output "Native UI opened $expected in its originating SSH pane and verified a binary round trip"
} catch {
    $tail = Get-Content -LiteralPath $log -Tail 20 -ErrorAction SilentlyContinue
    throw "$_ at $($_.ScriptStackTrace). GUI log: $tail"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
