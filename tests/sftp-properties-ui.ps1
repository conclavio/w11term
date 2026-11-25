[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
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
    throw "$($_.Exception.Message) at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
