[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$peer = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$gate = Join-Path $peer 'hold-shell-output'
$button = [System.Windows.Automation.ControlType]::Button
$combo = [System.Windows.Automation.ControlType]::ComboBox
$listItem = [System.Windows.Automation.ControlType]::ListItem
$select = [System.Windows.Automation.SelectionPattern]::Pattern
$selectItem = [System.Windows.Automation.SelectionItemPattern]::Pattern
$expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','timing-started')
    Start-Sleep -Milliseconds 300
    [IO.File]::Delete($gate)
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','timing-ready')
    $history = Control @('list-commands','--pane',"$pane")
    $quick, $slow, $unknown = $history.commands
    if ($history.commands.Count -ne 3 -or $quick.command -cne 'timing quick' -or
        $slow.command -cne 'timing slow' -or $slow.duration_ms -lt 200 -or
        $quick.duration_ms -ge $slow.duration_ms -or
        $null -eq $quick.started_unix_ms -or $null -eq $slow.started_unix_ms -or
        $null -ne $unknown.started_unix_ms -or $null -ne $unknown.duration_ms -or
        $unknown.finished_unix_ms -le 0) {
        throw "Unexpected capture timing: $($history.commands | ConvertTo-Json -Compress)"
    }
    $record = Control @('get-command','--pane',"$pane",'--id',"$($slow.id)",'--connection-generation',"$($history.connection_generation)")
    if ($record.duration_ms -ne $slow.duration_ms -or $record.started_unix_ms -ne $slow.started_unix_ms -or
        $record.finished_unix_ms -ne $slow.finished_unix_ms) {
        throw 'Detailed capture timing differed from the list'
    }

    Run-Palette 'Command history'
    $quickLabel = "Selected, timing quick${dot}exit 0"
    $slowLabel = "Selected, timing slow${dot}exit 2"
    $null = Wait-Until 'the newest command is selected by default' { Find-Element $quickLabel $button }
    $order = Wait-Until 'command order combo' { Find-Element 'Command order' $combo }
    $order.GetCurrentPattern($expand).Expand()
    $choice = Wait-Until 'slowest-first choice' { Find-Element 'Slowest first' $listItem -Visible }
    $choice.GetCurrentPattern($selectItem).Select()
    $null = Wait-Until 'the order retains the current selection' {
        $order = Find-Element 'Command order' $combo
        $current = @($order.GetCurrentPattern($select).Current.GetSelection())
        $current.Count -eq 1 -and $current[0].Current.Name -ceq 'Slowest first' -and
            (Find-Element $quickLabel $button)
    }
    $order = Find-Element 'Command order' $combo
    $popup = $order.GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
    Invoke-Button (Wait-Until 'refresh history' { Find-Element 'Refresh history' $button })
    $null = Wait-Until 'refresh selects the longest command in the current order' { Find-Element $slowLabel $button }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $timingId = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'command-timing')
    $invariant = [Globalization.CultureInfo]::InvariantCulture
    $start = [DateTimeOffset]::FromUnixTimeMilliseconds($slow.started_unix_ms).UtcDateTime.ToString('yyyy-MM-dd HH:mm:ss', $invariant)
    $finish = [DateTimeOffset]::FromUnixTimeMilliseconds($slow.finished_unix_ms).UtcDateTime.ToString('yyyy-MM-dd HH:mm:ss', $invariant)
    $ms = [uint64]$slow.duration_ms
    $elapsed = if ($ms -lt 1000) { "$ms ms" } elseif ($ms -lt 60000) {
        '{0}.{1:000} s' -f [int][Math]::Floor($ms / 1000), ($ms % 1000)
    } else {
        '{0}m {1:00}s' -f [int][Math]::Floor($ms / 60000), [int][Math]::Floor(($ms % 60000) / 1000)
    }
    $expected = "Started $start UTC${dot}Finished $finish UTC${dot}Elapsed $elapsed"
    $null = Wait-Until 'the selected command shows its timestamps and elapsed time' {
        $detail = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $timingId)
        $detail -and $detail.Current.Name -ceq $expected
    }

    Run-Palette ('Find command history' + [char]0x2026)
    Wait-PickerEditor 'Search commands across panes' 'window command history'
    $ssh = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles.'palette-ssh'
    $source = "palette-ssh${dot}$($ssh.user)@$($ssh.host):$($ssh.port)${dot}Pane $pane"
    $quickRow = "${source}${dot}exit 0${dot}timing quick"
    $slowRow = "${source}${dot}exit 2${dot}timing slow"
    $unknownRow = "${source}${dot}exit 9${dot}Command $($unknown.id)"
    $null = Wait-Until 'window history defaults to newest first' {
        $first = Find-Element $quickRow $button -Visible
        $second = Find-Element $slowRow $button -Visible
        $first -and $second -and $first.Current.BoundingRectangle.Top -lt $second.Current.BoundingRectangle.Top
    }
    Press-Key 0x77 # F8 changes order without leaving the search box
    $slowWithDuration = "$slowRow    $elapsed"
    $quickMs = [uint64]$quick.duration_ms
    $quickDuration = if ($quickMs -eq 0) { '<1 ms' } else { "$quickMs ms" }
    $null = Wait-Until 'slowest first places known durations ahead of unknown durations' {
        $first = Find-Element $slowWithDuration $button -Visible
        $second = Find-Element "$quickRow    $quickDuration" $button -Visible
        $last = Find-Element "$unknownRow    Duration unavailable" $button -Visible
        $first -and $second -and $last -and
            $first.Current.BoundingRectangle.Top -lt $second.Current.BoundingRectangle.Top -and
            $second.Current.BoundingRectangle.Top -lt $last.Current.BoundingRectangle.Top
    }
    Press-Key 0x76 # F7 cycles to Failed
    $null = Wait-Until 'failed history retains both failures and hides the success' {
        (Find-Element $slowWithDuration $button -Visible) -and
            (Find-Element "$unknownRow    Duration unavailable" $button -Visible) -and
            !(Find-Element "$quickRow    $quickDuration" $button)
    }
    Press-Key 0x0d
    $null = Wait-Until 'Enter opens the filtered and sorted capture' { Find-Element $slowLabel $button }
} finally {
    [IO.File]::Delete($gate)
    Close-FixtureApp $app
}
