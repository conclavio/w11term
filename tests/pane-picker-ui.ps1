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

function Wait-Preview([string]$Profile, [string]$Text) {
    $null = Wait-Until "preview of $Profile containing '$Text'" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $textNodes = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Text)
        foreach ($node in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $textNodes)) {
            $name = $node.Current.Name
            if ($name.StartsWith('Preview of ') -and $name.Contains($Profile) -and $name.Contains($Text)) { return $true }
        }
    }
}

try {
    $window = Wait-Until 'the fixture window' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
    $manifest = Get-ControlManifestPath $app.Id
    $null = Wait-Until 'the fixture control pipe' { Test-Path -LiteralPath $manifest }
    $panes = (Control @('list')).sessions
    $left = ($panes | Where-Object label -eq 'dev1').id
    $right = ($panes | Where-Object label -eq 'dev2').id
    $local = ($panes | Where-Object label -eq 'Windows PowerShell').id
    if ($panes.Count -ne 3 -or !$left -or !$right -or !$local) { throw "Unexpected workspace: $($panes | ConvertTo-Json -Compress)" }
    Wait-Focus $left
    foreach ($pane in $left,$right) { $null = Control @('wait-for-text','--pane',"$pane",'--timeout','10','ready') }

    # Give dev1 a remote title unrelated to its saved profile or address.
    function Send-RemoteOutput([string]$Text) {
        $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Text))
        $null = Control @('send-text','--pane',"$left",'--no-paste','--base64',$encoded)
    }
    $esc = [char]27
    Send-RemoteOutput "$esc]2;build-output$([char]7)"
    $null = Wait-Until 'remote window title' { (Control @('get-text','--pane',"$left")).title -eq 'build-output' }
    $cursor = (Control @('events','--pane',"$left",'--since','0','--timeout','0')).journal.next
    # ncurses xterm+alt+title enters/leaves the alternate screen before pushing/popping.
    Send-RemoteOutput "$esc[?1049h$esc[22;0;0t$esc]2;editor buffer$([char]7)"
    $null = Wait-Until 'editor title on the alternate screen' {
        $frame = Control @('get-text','--pane',"$left")
        $frame.alternate_screen -and $frame.title -ceq 'editor buffer'
    }
    Send-RemoteOutput "$esc[?1049l$esc[23;0;0t"
    $null = Wait-Until 'shell title restored on returning to the primary screen' {
        $frame = Control @('get-text','--pane',"$left")
        !$frame.alternate_screen -and $frame.title -ceq 'build-output'
    }
    $journal = (Control @('events','--pane',"$left",'--since',"$cursor",'--timeout','0')).journal
    $titles = @($journal.events | Where-Object kind -ceq 'title' | ForEach-Object { $_.data.title })
    if ($journal.lost -or $titles.Count -ne 2 -or $titles[0] -cne 'editor buffer' -or $titles[1] -cne 'build-output') {
        throw "Unexpected title events during editor entry/exit: $($titles -join ', ')"
    }

    # Ring and notify from the SSH split in the other tab. Verify the window-wide
    # badge updates while input stays on dev1, then select the exact sending split.
    $notice = [Text.Encoding]::UTF8.GetBytes("$([char]7)$([char]27)]9;compile finished$([char]27)\")
    $null = Control @('send-text','--pane',"$right",'--no-paste','--base64',[Convert]::ToBase64String($notice))
    $inbox = Wait-Until 'two unread alerts from background dev2' { Find-Element 'Attention inbox, 2 unread' }
    Wait-Focus $left
    Assert-Input $left 'before-inbox' @($right,$local)
    Invoke-Button $inbox
    $dot = [char]0x00b7
    $row = "dev2 $dot Pane $right $dot"
    $null = Wait-Until 'unread background bell' { Find-Element "Unread $dot $row Bell" }
    Invoke-Button (Wait-Until 'unread background message' { Find-Element "Unread $dot $row compile finished" })
    Wait-Focus $right
    Assert-Input $right 'after-inbox' @($left,$local)
    Invoke-Button (Wait-Until 'one remaining unread alert' { Find-Element 'Attention inbox, 1 unread' })
    $null = Wait-Until 'the visited row to be read' { Find-Element "Read $dot $row compile finished" }
    Invoke-Button (Wait-Until 'mark all read action' { Find-Element 'Mark all read' })
    $null = Wait-Until 'the bell to be read' { Find-Element "Read $dot $row Bell" }
    Invoke-Button (Wait-Until 'clear inbox action' { Find-Element 'Clear inbox' })
    $null = Wait-Until 'empty inbox' { Find-Element 'No bells or notifications' }
    $finished = [Text.Encoding]::UTF8.GetBytes("$([char]27)]9;build complete$([char]27)\")
    $null = Control @('send-text','--pane',"$left",'--no-paste','--base64',[Convert]::ToBase64String($finished))
    Invoke-Button (Wait-Until 'new notification while inbox is open' {
        Find-Element "Unread $dot dev1 $dot Pane $left $dot build complete"
    })
    Wait-Focus $left

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

    $null = Control @('send-text','--pane',"$left",'--no-paste','source-marker')
    $null = Control @('send-text','--pane',"$right",'--no-paste','hidden-marker')
    $original = Control @('get-text','--pane',"$right")
    $null = Open-PanePicker 'DEV2'
    foreach ($key in 0x44,0x45,0x56,0x32) { $null = Press-FocusedKey $key }
    $null = Wait-Until 'native keyboard input filters to DEV2' {
        $rows = @(Get-PickerRows)
        $rows.Count -eq 1 -and $rows[0] -like '*dev2*'
    }
    Wait-Preview 'dev2' 'hidden-marker'
    Wait-Focus $left
    $null = Control @('send-text','--pane',"$right",'--no-paste','-live')
    Wait-Preview 'dev2' 'hidden-marker-live'
    $grid = Control @('get-text','--pane',"$right")
    if ($grid.cols -ne $original.cols -or $grid.rows -ne $original.rows) { throw 'Preview resized the hidden pane' }
    $field = Find-Element 'Find pane'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('dev')
    Wait-Preview 'dev1' 'source-marker'
    Press-Key 40
    Wait-Preview 'dev2' 'hidden-marker-live'
    Press-Key 27
    $null = Wait-Until 'preview dismissal' { -not (Find-Element 'Find pane') }
    Wait-Focus $left
    Assert-Input $left 'after-preview' @($right,$local)

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
