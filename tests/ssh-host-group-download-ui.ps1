[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_SFTP_PICKER_SMOKE_DIR = Join-Path $State 'picker'
$picked = Join-Path $env:W11TERM_SFTP_PICKER_SMOKE_DIR 'collected'
$profiles = Join-Path $State 'profiles.json'
$config = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
$eastRoot = [IO.File]::ReadAllText((Join-Path $State 'east-root'))
$gate = Join-Path $eastRoot 'hold-transfer-read'
$dot = ' ' + [char]0xb7 + ' '
$ellipsis = [char]0x2026
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Set-Field([string]$Name, [string]$Text) {
    $value = Wait-Until "editable '$Name'" {
        $item = Find-Element $Name $edit
        $provider = $null
        if ($item -and $item.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $value.SetValue($Text)
}
function View-Text {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $type = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $type)) {
        $item.Current.Name
    }
}
function Wait-Result([string]$Prefix) {
    $null = Wait-Until "result '$Prefix'" {
        foreach ($text in (View-Text)) {
            if ($text.StartsWith($Prefix, [StringComparison]::Ordinal)) { return $true }
        }
    }
}
function Choose-Parent {
    Click "Choose local folder$ellipsis"
    $dialog = Wait-Until 'owned download folder picker' { Find-OwnedDialog 'Choose a download destination folder' }
    Select-DialogItem $dialog 'collected'
    $null = Wait-Until 'accept the selected folder' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'selected folder is shown' {
        foreach ($text in (View-Text)) {
            if ($text.EndsWith('\collected', [StringComparison]::OrdinalIgnoreCase) -and
                [IO.Directory]::Exists($text) -and
                [FixtureDialog]::DirectoryPath($text) -eq [FixtureDialog]::DirectoryPath($picked)) {
                return $true
            }
        }
    }
}

$launch = @('--profiles',('"' + $profiles + '"'),'--profile','"Fetch East"','--tab','"Fetch West"',
    '--tab','"Fetch Missing"','--tab','"Fetch Stale"','--tab','Witness','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
$reader = $null
try {
    $reader = Open-PeerTraceReader (Join-Path $eastRoot 'transfer-trace')
    $window = Wait-InputWindow
    $sessions = @((Control @('list')).sessions)
    if ($sessions.Count -ne 5) { throw 'Expected five fixture SSH panes' }
    $targets = @{}
    $panes = @{}
    foreach ($session in $sessions) {
        $name = $session.label
        $port = $config.$name.port
        $panes[$name] = $session.id
        $targets[$name] = "$name${dot}test@127.0.0.1:${port}${dot}Pane $($session.id)"
        $null = Control @('wait-for-text','--pane',"$($session.id)",'--timeout','15','ready')
    }
    Run-Palette 'SSH host group'
    Click 'Select shown'
    foreach ($target in $targets.Values) { $null = Wait-CheckBox $target On }
    Set-Field 'Host filter' 'Fetch'
    $null = Wait-Until 'Witness is hidden by the roster filter' {
        Find-Element '1 selected host is hidden by the filter.'
    }
    Click "Download file$ellipsis"
    $null = Wait-Until 'download review includes the four shown hosts' {
        (Find-Element 'Download from selected SSH hosts') -and
        (Find-Element $targets['Fetch East']) -and (Find-Element $targets['Fetch West']) -and
        (Find-Element $targets['Fetch Missing']) -and (Find-Element $targets['Fetch Stale']) -and
        !(Find-Element $targets['Witness'])
    }
    Choose-Parent
    Set-Field 'Remote file path' '../snapshot.bin'
    $null = Wait-Until 'relative path cannot start a download' {
        $submit = Find-Element 'Download from 4 hosts' $button
        $submit -and !$submit.Current.IsEnabled
    }
    Set-Field 'Remote file path' '/reports/snapshot.bin'
    $stale = $sessions | Where-Object label -eq 'Fetch Stale'
    $null = Control @('send-text','--pane',"$($stale.id)",'--no-paste','--base64','BA==')
    $token = Wait-Until 'the reviewed shell exits' { (Control @('events','--pane',"$($stale.id)")).restart_token }
    $null = Control @('restart','--pane',"$($stale.id)",'--exit-token',"$token")
    $null = Wait-Until 'the same pane has a replacement SSH generation' {
        $current = (Control @('list')).sessions | Where-Object id -eq $stale.id
        $current.connection_generation -gt $stale.connection_generation
    }
    Click 'Download from 4 hosts'
    $null = Wait-Until 'group download results' { Find-Element 'SSH group download results' }
    $eastFile = "pane-$($panes['Fetch East'])-snapshot.bin"
    $westFile = "pane-$($panes['Fetch West'])-snapshot.bin"
    Wait-Result "$($targets['Fetch East'])${dot}Downloaded${dot}84000 bytes${dot}$eastFile"
    Wait-Result "$($targets['Fetch West'])${dot}Downloaded${dot}42000 bytes${dot}$westFile"
    Wait-Result "$($targets['Fetch Missing'])${dot}Download failed${dot}"
    Wait-Result "$($targets['Fetch Stale'])${dot}The SSH connection changed; download was not started."
    $directories = @(Get-ChildItem -LiteralPath $picked -Directory -Filter 'ssh-group-download-*')
    if ($directories.Count -ne 1) { throw 'Expected a new batch folder in the chosen parent' }
    if ((Get-Item -LiteralPath (Join-Path $directories[0].FullName $eastFile)).Length -ne 84000 -or
        (Get-Item -LiteralPath (Join-Path $directories[0].FullName $westFile)).Length -ne 42000) {
        throw 'The bytes were not saved under the expected pane IDs'
    }
    Click 'Back to host group'
    Set-Field 'Host filter' 'Fetch East'
    $null = Wait-CheckBox $targets['Fetch East'] On
    Click "Download file$ellipsis"
    Choose-Parent
    [IO.File]::WriteAllText($gate, '')
    $start = (Read-PeerTrace $reader).Length
    Set-Field 'Remote file path' '/reports/cancel.bin'
    Click 'Download from 1 host'
    $null = Wait-Until 'a second read is waiting in the cancelled batch' {
        $trace = Read-PeerTrace $reader
        $wire = [Text.Encoding]::UTF8.GetString($trace, $start, $trace.Length - $start)
        $wire.Contains('OPEN /reports/cancel.bin') -and $wire -match '(?m)^READ [1-9][0-9]*\r?$'
    }
    Click 'Cancel downloads'
    $null = Wait-Until 'cancellation reaches the batch' { Find-Element "Cancelling downloads$ellipsis" $button }
    [IO.File]::Delete($gate)
    Wait-Result "$($targets['Fetch East'])${dot}Cancelled${dot}discarded "
    Click 'Back to host group'
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    [IO.File]::Delete($gate)
    if ($reader) { $reader.Dispose() }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
