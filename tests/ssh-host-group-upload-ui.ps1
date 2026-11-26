[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_SFTP_PICKER_SMOKE_DIR = Join-Path $State 'picker'
$file = Join-Path $env:W11TERM_SFTP_PICKER_SMOKE_DIR 'fleet-source'
$profiles = Join-Path $State 'profiles.json'
$config = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
$dot = ' ' + [char]0xb7 + ' '
$ellipsis = [char]0x2026
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

function Click([string]$Name) {
    $invoke = Wait-Until "enabled '$Name'" {
        $element = Find-Element $Name $button
        $provider = $null
        if ($element -and $element.Current.IsEnabled -and $element.TryGetCurrentPattern(
                [System.Windows.Automation.InvokePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $invoke.Invoke()
}
function Set-Field([string]$Name, [string]$Text) {
    $value = Wait-Until "editable '$Name'" {
        $element = Find-Element $Name $edit
        $provider = $null
        if ($element -and $element.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $value.SetValue($Text)
}
function View-Text {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $type = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    foreach ($element in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $type)) {
        $element.Current.Name
    }
}
function View-ContainsPrefix([string]$Prefix) {
    foreach ($name in (View-Text)) {
        if ($name.StartsWith($Prefix, [StringComparison]::Ordinal)) { return $true }
    }
    return $false
}

$launch = @('--profiles',('"' + $profiles + '"'),'--profile','"Fleet East"','--tab','"Fleet West"',
    '--tab','"Fleet Stale"','--tab','Witness','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $sessions = @((Control @('list')).sessions)
    if ($sessions.Count -ne 4) { throw 'Expected four fixture SSH panes' }
    $targets = @{}
    foreach ($session in $sessions) {
        $name = $session.label
        $port = $config.$name.port
        $targets[$name] = "$name${dot}test@127.0.0.1:${port}${dot}Pane $($session.id)"
        $null = Control @('wait-for-text','--pane',"$($session.id)",'--timeout','15','ready')
    }
    Run-Palette 'SSH host group'
    Click 'Select shown'
    foreach ($name in $targets.Values) { $null = Wait-CheckBox $name On }
    Set-Field 'Host filter' 'Fleet'
    $null = Wait-Until 'Witness is hidden by the roster filter' { Find-Element '1 selected host is hidden by the filter.' }
    Invoke-NativeMenuAction 'Roster actions' "Upload files$ellipsis"
    $null = Wait-Until 'upload review includes only shown hosts' {
        (Find-Element 'Upload to selected SSH hosts') -and
        (Find-Element $targets['Fleet East']) -and
        (Find-Element $targets['Fleet West']) -and
        (Find-Element $targets['Fleet Stale']) -and
        !(Find-Element $targets['Witness'])
    }
    Click "Choose local files$ellipsis"
    $picker = Wait-Until 'owned file picker' { Find-OwnedDialog 'Choose files to upload' }
    Select-DialogItem $picker 'fleet-source'
    $null = Wait-Until 'confirm the selected file' { [FixtureDialog]::ClickButton($picker, 1) }
    $null = Wait-Until 'close the file picker' { !(Find-OwnedDialog 'Choose files to upload') }
    $size = "${dot}97000 bytes"
    $null = Wait-Until 'selected file and size are shown' {
        foreach ($name in (View-Text)) {
            # The Shell may expand the runner's short TEMP path before returning it.
            if ($name.EndsWith($size, [StringComparison]::Ordinal) -and
                [FixtureDialog]::SameFile($file, $name.Substring(0, $name.Length - $size.Length))) {
                return $true
            }
        }
    }
    Set-Field 'Remote file path' '../report.bin'
    $null = Wait-Until 'relative path is ineligible' {
        $submit = Find-Element 'Upload to 3 hosts' $button
        $submit -and !$submit.Current.IsEnabled
    }
    Set-Field 'Remote file path' '/incoming/report.bin'

    $stale = $sessions | Where-Object label -eq 'Fleet Stale'
    $null = Control @('send-text','--pane',"$($stale.id)",'--no-paste','--base64','BA==')
    $token = Wait-Until 'old SSH session exits while the upload is reviewed' {
        (Control @('events','--pane',"$($stale.id)")).restart_token
    }
    $null = Control @('restart','--pane',"$($stale.id)",'--exit-token',"$token")
    $null = Wait-Until 'replacement SSH session in the same pane' {
        $current = (Control @('list')).sessions | Where-Object id -eq $stale.id
        $current.connection_generation -gt $stale.connection_generation
    }
    Click 'Upload to 3 hosts'
    $null = Wait-Until 'per-host upload results and preserved selection' {
        (Find-Element 'SSH group upload results') -and
        (Find-Element "$($targets['Fleet East'])${dot}Uploaded${dot}97000 bytes") -and
        (View-ContainsPrefix "$($targets['Fleet West'])${dot}Upload failed${dot}") -and
        (Find-Element "$($targets['Fleet Stale'])${dot}The SSH connection changed; upload was not started.")
    }
    Click 'Back to host group'
    $null = Wait-CheckBox $targets['Fleet East'] On
    (Wait-CheckBox $targets['Fleet Stale'] On).Toggle()
    $null = Wait-CheckBox $targets['Fleet Stale'] Off
    Invoke-NativeMenuAction 'Roster actions' "Upload files$ellipsis"
    Click "Choose local files$ellipsis"
    $picker = Wait-Until 'owned multi-file picker' { Find-OwnedDialog 'Choose files to upload' }
    Select-DialogItem $picker 'fleet-source'
    Select-DialogItem $picker 'second-source' -Add
    $null = Wait-Until 'confirm both selected files' { [FixtureDialog]::ClickButton($picker, 1) }
    $null = Wait-Until 'review shows both selected files' { Find-Element '2 local files' }
    Set-Field 'Remote directory' '../incoming'
    $null = Wait-Until 'relative directory is ineligible' {
        $submit = Find-Element 'Upload to 2 hosts' $button
        $submit -and !$submit.Current.IsEnabled
    }
    Set-Field 'Remote directory' '/incoming/'
    Click 'Upload to 2 hosts'
    $null = Wait-Until 'each host and file has its own upload outcome' {
        (Find-Element "3 files uploaded${dot}0 pending${dot}1 skipped, failed or cancelled") -and
        (Find-Element "$($targets['Fleet East'])${dot}fleet-source${dot}Uploaded${dot}97000 bytes") -and
        (Find-Element "$($targets['Fleet East'])${dot}second-source${dot}Uploaded${dot}11 bytes") -and
        (View-ContainsPrefix "$($targets['Fleet West'])${dot}fleet-source${dot}Upload failed${dot}") -and
        (Find-Element "$($targets['Fleet West'])${dot}second-source${dot}Uploaded${dot}11 bytes")
    }
    Click 'Back to host group'
    Set-Field 'Host filter' ''
    $null = Wait-CheckBox $targets['Witness'] On
} catch {
    throw "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
