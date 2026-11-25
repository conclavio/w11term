[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$fixture = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$log = Join-Path $fixture 'shell directory/engine.log'
$trace = Join-Path $fixture 'transfer-trace'
$gate = Join-Path $fixture 'hold-transfer-read'
$button = [System.Windows.Automation.ControlType]::Button
$utf8 = [Text.UTF8Encoding]::new($false)

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Wait-Tail([string]$End) {
    $null = Wait-Until "selectable tail ends with '$End'" {
        $field = Find-Element 'File tail'
        if ($field -and $field.Current.AutomationId -eq 'sftp-file-tail') {
            $text = $field.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
            $text.DocumentRange.GetText(-1).TrimEnd().EndsWith($End)
        }
    }
}
function Tail-Scroll {
    $area = Find-Element 'File tail scroll area'
    if ($area) {
        $pattern = $null
        if ($area.TryGetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern, [ref]$pattern)) {
            $pattern
        }
    }
}
function Count-Reads {
    ([regex]::Matches([IO.File]::ReadAllText($trace), '(?m)^OPEN /shell directory/engine[.]log')).Count
}
function Assert-NoNewReads {
    $before = Count-Reads
    $deadline = [DateTime]::UtcNow.AddMilliseconds(1400)
    do {
        [Threading.Thread]::Sleep(80)
        if ((Count-Reads) -ne $before) { throw 'The tail issued a new read while paused or closed' }
    } while ([DateTime]::UtcNow -lt $deadline)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = (Control @('list')).sessions[0]
    $null = Control @('wait-for-text','--pane',"$($pane.id)",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Click 'engine.log'
    Invoke-SftpEntryAction ('Tail text file' + [char]0x2026)
    Wait-Tail 'seed-080'
    [IO.File]::AppendAllText($log, "live line$([char]10)", $utf8)
    Assert-NoNewReads
    Wait-Tail 'seed-080'

    Click 'Follow tail'
    Wait-Tail 'live line'
    $null = Wait-Until 'automatic scrolling reaches the bottom' {
        $scroll = Tail-Scroll
        $scroll -and $scroll.Current.VerticallyScrollable -and $scroll.Current.VerticalScrollPercent -gt 98
    }
    $null = Wait-Until 'tail scrolled back to earlier lines' {
        $scroll = Tail-Scroll
        if ($scroll) {
            if ($scroll.Current.VerticalScrollPercent -lt 2) { return $true }
            $scroll.Scroll([System.Windows.Automation.ScrollAmount]::NoAmount,
                           [System.Windows.Automation.ScrollAmount]::LargeDecrement)
        }
    }
    $before = Count-Reads
    $null = Wait-Until 'unchanged snapshots were polled without jumping to the bottom' {
        (Count-Reads) -ge ($before + 2) -and (Tail-Scroll).Current.VerticalScrollPercent -lt 2
    }

    [IO.File]::WriteAllBytes($gate, [byte[]]@())
    $null = Wait-Until 'read in flight on the loopback SFTP channel' {
        [IO.File]::ReadAllText($trace).TrimEnd() -match 'READ [1-9][0-9]*$'
    }
    [IO.File]::AppendAllText($log, "held line$([char]10)", $utf8)
    Click 'Pause follow'
    $null = Wait-Until 'following is paused' { Find-Element 'Follow tail' $button }
    [IO.File]::Delete($gate)
    $null = Wait-Until 'cancelled read closed its SFTP handle' {
        [IO.File]::ReadAllText($trace).TrimEnd().EndsWith('CLOSE')
    }
    Assert-NoNewReads
    Wait-Tail 'live line'

    Click 'Follow tail'
    Wait-Tail 'held line'
    [IO.File]::Move($log, "$log.old")
    $null = Wait-Until 'follow reports the temporary missing file' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-preview-status')
        $message = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        $message -and $message.Current.Name.StartsWith('Could not refresh the file tail.')
    }
    Wait-Tail 'held line'
    [IO.File]::WriteAllText($log, "rotated live$([char]10)", $utf8)
    Wait-Tail 'rotated live'
    Click 'Close tail'
    $null = Wait-Until 'tail dialog closed' { !(Find-Element 'Close tail' $button) }
    Assert-NoNewReads
    if ([IO.File]::ReadAllText($trace).Contains('WRITE')) { throw 'Tail sent an SFTP write' }
} catch {
    throw "$($_.Exception.Message)$([char]10)$($_.ScriptStackTrace)"
} finally {
    [IO.File]::Delete($gate)
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
