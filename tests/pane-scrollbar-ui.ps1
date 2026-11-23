[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_UI_SMOKE = '1'
$env:W11TERM_SCROLLBAR_SMOKE = $State
$gate = [IO.File]::ReadAllText((Join-Path $State 'output-gate'))
$profiles = Join-Path $State 'profiles.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","focus_left":"F6","focus_right":"F8"},"interaction":{"copyOnSelect":false}}')
$esc = [char]27
$no = [System.Windows.Automation.ScrollAmount]::NoAmount

function Reader([string]$Name) {
    Wait-Until "native terminal document for '$Name'" {
        Find-Element ('Terminal ' + [char]0xb7 + ' ' + $Name) ([System.Windows.Automation.ControlType]::Document)
    }
}
function Visible($Document) {
    $pattern = $Document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
    $pattern.GetVisibleRanges()[0].GetText(-1)
}
function Top-WireRow($Document) {
    $content = Visible $Document
    if ($content -notmatch '^wire-(\d{3})') { throw "No numbered top row in '$content'" }
    [int]$Matches[1]
}
function Send-To([int]$Pane, [string]$Text) {
    $b64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Text))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$b64)
}
function Gesture([int]$Pane, [string]$Gesture) {
    $pending = Join-Path $State 'staged-pointer'
    $request = Join-Path $State 'rail-pointer'
    [IO.File]::WriteAllText($pending, "$Pane $Gesture")
    [IO.File]::Move($pending, $request)
    $null = Wait-Until "completion of '$Gesture' on pane $Pane" { ![IO.File]::Exists($request) }
}
function Geometry {
    $items = (Control @('list')).sessions | Sort-Object id | Select-Object id,tab_id,window_id,cols,rows,connection_generation
    $items | ConvertTo-Json -Compress
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','124','--rows','24') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $ssh = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','wire-199')
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'neighboring PowerShell' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $local
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    Send-To $local ('function global:prompt { "PS fixture> " }; $global:railPid=$PID; 1..80 | % { "local-row-{0:D3}" -f $_ }; "local-end-"+"ready"' + [char]13)
    $null = Control @('wait-for-text','--pane',"$local",'--regex','--timeout','15','local-end-ready *\r?\nPS fixture>')
    $localBefore = Visible (Reader 'Palette PowerShell')
    Press-Key 0x75
    Wait-Focus $ssh
    $document = Reader 'palette-ssh'
    $scroll = $document.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
    $null = Wait-Until 'live scrollable SSH viewport' {
        $scroll.Current.VerticallyScrollable -and $scroll.Current.VerticalScrollPercent -eq 100 -and (Visible $document).Contains('wire-199')
    }
    if ($scroll.Current.HorizontallyScrollable -or $scroll.Current.HorizontalScrollPercent -ne -1 -or
        $scroll.Current.VerticalViewSize -le 0 -or $scroll.Current.VerticalViewSize -ge 100) { throw 'Unexpected scroll range on primary screen' }
    $geometry = Geometry
    $scroll.SetScrollPercent(-1, 0)
    $null = Wait-Until 'oldest retained numbered row' { (Top-WireRow $document) -eq 0 -and $scroll.Current.VerticalScrollPercent -eq 0 }
    $scroll.Scroll($no, [System.Windows.Automation.ScrollAmount]::SmallIncrement)
    $null = Wait-Until 'one row below the oldest' { (Top-WireRow $document) -eq 1 }
    $scroll.SetScrollPercent(-1, 50)
    $null = Wait-Until 'middle of SSH history' {
        $top = Top-WireRow $document
        $top -gt 60 -and $top -lt 130 -and [Math]::Abs($scroll.Current.VerticalScrollPercent - 50) -le 1
    }
    $contextLabel = 'Jump to command output ' + [char]0xb7 + ' fixture build'
    $contextButton = [System.Windows.Automation.ControlType]::Button
    Invoke-Button (Wait-Until 'command context for the numbered output' {
        Find-Element $contextLabel $contextButton
    })
    $null = Wait-Until 'command context jumps to the beginning without changing the grid' {
        (Top-WireRow $document) -eq 0 -and !(Find-Element $contextLabel $contextButton)
    }
    if ((Geometry) -cne $geometry) { throw 'Command context resized or moved a pane' }
    $scroll.SetScrollPercent(-1, 90)
    $null = Wait-Until 'viewport beyond the completed command has no header' {
        (Top-WireRow $document) -gt 145 -and !(Find-Element $contextLabel $contextButton)
    }
    $scroll.SetScrollPercent(-1, 50)
    $null = Wait-Until 'scrolled command context is restored' {
        [Math]::Abs($scroll.Current.VerticalScrollPercent - 50) -le 1 -and
            (Find-Element $contextLabel $contextButton)
    }
    $anchored = Visible $document
    [IO.File]::Delete($gate)
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','wire-202')
    if ((Visible $document) -cne $anchored) { throw 'Unsolicited SSH output moved the scrolled viewport' }
    $scroll.SetScrollPercent(-1, 100)
    $null = Wait-Until 'new live SSH tail' { (Visible $document).Contains('wire-202') -and $scroll.Current.VerticalScrollPercent -eq 100 }
    Gesture $ssh 'drag-up'
    $null = Wait-Until 'thumb drag to oldest row' { (Top-WireRow $document) -eq 0 }
    Gesture $ssh 'drag-down'
    $null = Wait-Until 'thumb drag to live output' { (Visible $document).Contains('wire-202') }
    $liveTop = Top-WireRow $document
    $rows = ((Control @('list')).sessions | Where-Object id -eq $ssh).rows
    Gesture $ssh 'page-up'
    $null = Wait-Until 'one track click scrolls up a page' {
        (Top-WireRow $document) -eq ($liveTop - $rows) -and $scroll.Current.VerticalScrollPercent -lt 100
    }
    $scroll.SetScrollPercent(-1, 0)
    $null = Wait-Until 'SSH thumb at oldest before clicking in background' { (Top-WireRow $document) -eq 0 }
    Press-Key 0x77
    Wait-Focus $local
    $localDoc = Reader 'Palette PowerShell'
    if ((Visible $localDoc) -cne $localBefore) { throw 'Scrolling SSH moved the PowerShell viewport' }
    Gesture $ssh 'drag-down'
    Wait-Focus $local
    if ((Visible $localDoc) -cne $localBefore) { throw 'Background SSH scrollbar changed the focused pane' }
    Press-Key 0x75
    Wait-Focus $ssh
    $document = Reader 'palette-ssh'
    $scroll = $document.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
    $null = Wait-Until 'background thumb brought only SSH back to live' {
        $scroll.Current.VerticalScrollPercent -eq 100 -and (Visible $document).Contains('wire-202')
    }
    Send-To $ssh ($esc + '[?1049h' + $esc + '[Halternate-proof')
    $null = Wait-Until 'alternate screen exposes no scrollable history' {
        (Visible $document).Contains('alternate-proof') -and !$scroll.Current.VerticallyScrollable -and
            $scroll.Current.VerticalScrollPercent -eq -1 -and $scroll.Current.VerticalViewSize -eq 100
    }
    $refused = $false
    try { $scroll.SetScrollPercent(-1, 0) } catch [System.Management.Automation.MethodInvocationException] { $refused = $true }
    if (!$refused -or !(Visible $document).Contains('alternate-proof')) { throw 'Alternate screen accepted a scrollback request' }
    Send-To $ssh ($esc + '[?1049l')
    $null = Wait-Until 'restored primary scroll range' { $scroll.Current.VerticallyScrollable -and (Visible $document).Contains('wire-202') }
    if ((Geometry) -cne $geometry) { throw 'The rail changed pane geometry, IDs or SSH generation' }
    Press-Key 0x77
    Wait-Focus $local
    Send-To $local ('"same-local-" + ($railPid -eq $PID)' + [char]13)
    $null = Control @('wait-for-text','--pane',"$local",'--regex','--timeout','15','same-local-True *\r?\nPS fixture>')

    Press-Key 0x75
    Wait-Focus $ssh
    Run-Palette 'SFTP files'
    $button = [System.Windows.Automation.ControlType]::Button
    $edit = [System.Windows.Automation.ControlType]::Edit
    $null = Wait-Until 'remote directory listing' { Find-Element 'knot.txt' $button }
    Invoke-Button (Wait-Until 'filter action' { Find-Element ('Filter filenames' + [char]0x2026) $button })
    $field = Wait-Until 'filename filter receives keyboard focus' {
        $item = Find-Element 'Filter filenames' $edit
        if ($item -and $item.Current.HasKeyboardFocus) { $item }
    }
    $value = $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    $null = Press-FocusedKey 0x4b
    $null = Wait-Until 'first typed character filters filenames' {
        $value.Current.Value -ieq 'k' -and (Find-Element 'knot.txt' $button) -and (Find-Element 'kept.txt' $button)
    }
    Gesture $local 'page-up'
    $null = Press-FocusedKey 0x45
    $null = Wait-Until 'filename filter keeps focus and receives the next character' {
        $field.Current.HasKeyboardFocus -and $value.Current.Value -ieq 'ke' -and
            (Find-Element 'kept.txt' $button) -and !(Find-Element 'knot.txt' $button)
    }
    Wait-Focus $ssh
    if ((Control @('get-text','--pane',"$local")).text -match 'PS fixture> *[kKeE]+ *(\r?\n|$)') {
        throw 'Filename-filter keystrokes appeared in PowerShell'
    }
    Invoke-Button (Wait-Until 'Done editing the filter' { Find-Element 'Done' $button })
    $null = Wait-Until 'filename editor closed' { !(Find-Element 'Filter filenames' $edit) }
    Press-Key 0x77
    Wait-Focus $local
    $localDoc = Reader 'Palette PowerShell'
    $localScroll = $localDoc.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
    if ($localScroll.Current.VerticalScrollPercent -ge 100) { throw 'Clicking the background rail did not scroll PowerShell' }
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
