# Invoked by the native SFTP/SSH integration suite with two live loopback shells.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--workspace', 'picker') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')

function Assert-Input([int]$Pane, [string]$Marker, [int[]]$OtherPanes) {
    foreach ($letter in $Marker.ToCharArray()) { Post-Key 0x102 ([uint32]$letter) }
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','5',$Marker)
    foreach ($other in $OtherPanes) {
        if ((Control @('get-text','--pane',"$other")).text.Contains($Marker)) {
            throw "Keystrokes intended for pane $Pane also reached $other"
        }
    }
}

try {
    $window = Wait-Until 'the fixture window' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
    $manifest = Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json"
    $null = Wait-Until 'the fixture control pipe' { Test-Path -LiteralPath $manifest }
    $panes = (Control @('list')).sessions
    $left = ($panes | Where-Object label -eq 'dev1').id
    $right = ($panes | Where-Object label -eq 'dev2').id
    $local = ($panes | Where-Object label -eq 'Windows PowerShell').id
    if ($panes.Count -ne 3 -or !$left -or !$right -or !$local) { throw "Unexpected workspace: $($panes | ConvertTo-Json -Compress)" }
    Wait-Focus $left
    foreach ($pane in $left,$right) { $null = Control @('wait-for-text','--pane',"$pane",'--timeout','10','ready') }

    # Give dev1 a remote title unrelated to its saved profile or address.
    $osc = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("$([char]27)]2;build-output$([char]7)"))
    $null = Control @('send-text','--pane',"$left",'--no-paste','--base64',$osc)
    $null = Wait-Until 'remote window title' { (Control @('get-text','--pane',"$left")).title -eq 'build-output' }

    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    Invoke-Button (Wait-Until 'inspector menu item' { Find-Element 'Terminal inspector: Off' })
    $null = Wait-Until 'initial primary screen' { Find-Element 'Screen   Primary' }
    $paste = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes("$([char]27)[?2004h"))
    $null = Control @('send-text','--pane',"$left",'--no-paste','--base64',$paste)
    $null = Wait-Until 'mode change without any screen output' { Find-Element 'Bracketed paste   On' }
    $modes = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes("$([char]27)[?1049h$([char]27)[?1003;1006h$([char]27)[>3u"))
    $null = Control @('send-text','--pane',"$left",'--no-paste','--base64',$modes)
    foreach ($row in 'Screen   Alternate','Mouse reporting   all motion (SGR cells)','Bracketed paste   On','Keyboard   Kitty 3 (disambiguate, event types)') {
        $null = Wait-Until "inspector row $row" { Find-Element $row }
    }
    # Labels can cover any cell, including cells behind the inspector.
    Press-Key 0x77
    $null = Wait-Until 'inspector to yield the viewport to keyboard hints' { -not (Find-Element 'Close terminal inspector') }
    Press-Key 27
    $null = Wait-Until 'inspector to return on Escape' { (Find-Element 'Close terminal inspector') -and (Find-Element 'Screen   Alternate') }
    Wait-Focus $left

    Picker-Query 'DEV2'
    Press-Key 13
    Wait-Focus $right
    $null = Wait-Until 'inspector follows focused pane' { (Find-Element 'Bracketed paste   Off') -and (Find-Element 'Screen   Primary') }
    Assert-Input $right 'after-enter' @($left,$local)

    Picker-Query 'build-output'
    Press-Key 27
    $null = Wait-Until 'picker dismissal' { -not (Find-Element 'Find pane') }
    Wait-Focus $right
    Assert-Input $right 'after-escape' @($left,$local)

    Picker-Query 'build-output'
    Press-Key 13
    Wait-Focus $left
    $null = Wait-Until 'inspector returns to original pane modes' { Find-Element 'Bracketed paste   On' }
    Invoke-Button (Wait-Until 'close inspector' { Find-Element 'Close terminal inspector' })
    $null = Wait-Until 'inspector dismissal' { -not (Find-Element 'Close terminal inspector') }
    Picker-Query 'navigator@127.0.0.1'
    Press-Key 13
    Wait-Focus $right
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
