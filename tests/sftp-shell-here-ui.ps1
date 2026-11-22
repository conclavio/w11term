[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","focus_left":"F6"}}')
$profiles = Join-Path $State 'profiles.json'
$peer = [IO.File]::ReadAllText((Join-Path $State 'peer-root'))
$gate = Join-Path $peer 'hold-opendir'
$bookmarks = 'Bookmarks' + [char]0x2026
$target = "/Pilot's Work"
$action = 'Open shell here'
$button = [System.Windows.Automation.ControlType]::Button
$text = [System.Windows.Automation.ControlType]::Text

function Click([string]$Label) {
    Invoke-Button (Wait-Until "enabled '$Label'" {
        $entry = Find-Element $Label $button
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
}
function Listing([string]$Path, [string]$Entry) {
    $null = Wait-Until "listing $Path containing '$Entry'" {
        (Find-Element $Path $text) -and (Find-Element $Entry $button)
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','128','--rows','32') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $original = (Control @('list')).sessions | Where-Object label -eq 'palette-ssh'
    $source = $original.id
    $null = Control @('wait-for-text','--pane',"$source",'--timeout','15','ready')
    $field = Open-Palette 'current directory'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('current directory')
    $null = Wait-Until 'no OSC 7 directory available for the source shell' {
        $item = Find-Element 'Split right in current directory' $button
        $item -and !$item.Current.IsEnabled
    }
    Press-Key 27
    $null = Wait-Until 'palette dismissed' { !(Find-Element 'Search commands' ([System.Windows.Automation.ControlType]::Edit)) }
    Run-Palette 'SFTP files'
    Listing '/' "Pilot's Work/"
    Click "Pilot's Work/"
    Click 'Open folder'
    Listing $target 'bookmark-witness.txt'
    Click $bookmarks
    Click 'Bookmark this folder'
    $null = Wait-Until 'bookmark saved' { Find-Element "Open bookmark $target" $button }
    Click 'Back to files'
    Click 'deeper/'
    Click 'Open folder'
    $null = Wait-Until 'server holding the next real directory reply' { [IO.File]::Exists((Join-Path $peer 'opendir-paused')) }
    $null = Wait-Until 'shell action disabled while the new listing is unconfirmed' {
        $item = Find-Element $action $button
        $item -and !$item.Current.IsEnabled
    }
    [IO.File]::Delete($gate)
    Listing "$target/deeper" 'child-witness.txt'
    Click $bookmarks
    Click "Open bookmark $target"
    Listing $target 'bookmark-witness.txt'
    Click $action
    $child = Wait-Until 'new active right split beside the original SSH pane' {
        $sessions = @((Control @('list')).sessions)
        $a = $sessions | Where-Object id -eq $source
        $b = $sessions | Where-Object id -ne $source
        if ($sessions.Count -eq 2 -and $a.visible -and $b.visible -and $b.active -and
            $a.connection_generation -eq $original.connection_generation -and $b.tab_id -eq $original.tab_id -and
            $a.cols -lt $original.cols -and $b.cols -lt $original.cols -and [Math]::Abs($a.rows - $b.rows) -le 1) { $b.id }
    }
    $null = Control @('wait-for-text','--pane',"$child",'--timeout','15','workspace-command-started')
    $null = Control @('send-text','--pane',"$source",'--no-paste','parent-live')
    $null = Control @('wait-for-text','--pane',"$source",'--timeout','15','parent-live')
    Wait-Focus $child
    foreach ($c in 'child-live'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$child",'--timeout','15','child-live')
    if ((Control @('get-text','--pane',"$source")).text.Contains('workspace-command-started') -or
        (Control @('get-text','--pane',"$child")).text.Contains('parent-live')) { throw 'Shell output crossed channels' }
    Press-Key 0x75
    Wait-Focus $source
    Listing $target 'bookmark-witness.txt'
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    [IO.File]::Delete($gate)
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
