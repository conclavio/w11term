[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_TEST_COPY_FILE = Join-Path $State 'environment-report.txt'
$profiles = Join-Path $State 'profiles.json'
$config = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
$dot = ' ' + [char]0xb7 + ' '
$review = 'Export group variables'
$entry = $review + [char]0x2026
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Field([string]$Name) {
    Wait-Until "editable '$Name'" {
        $item = Find-Element $Name $edit
        $provider = $null
        if ($item -and $item.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider)) { $provider }
    }
}
function Select-Fish {
    $popup = Wait-Until 'shell syntax selector' {
        $item = Find-Element 'Shell syntax' ([System.Windows.Automation.ControlType]::ComboBox)
        $provider = $null
        if ($item -and $item.TryGetCurrentPattern(
                [System.Windows.Automation.ExpandCollapsePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $popup.Expand()
    $choice = Wait-Until 'Fish option' {
        $item = Find-Element 'Fish' ([System.Windows.Automation.ControlType]::ListItem)
        $provider = $null
        if ($item -and !$item.Current.IsOffscreen -and $item.TryGetCurrentPattern(
                [System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$provider)) { $provider }
    }
    $choice.Select()
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}
function Wait-SendResult {
    $null = Wait-Until 'export submission' { Find-Element "Submitted$dot$review" }
    foreach ($name in @('Fleet East', 'Fleet West')) {
        $null = Wait-Until "successful export to $name" { Find-Element "$($targets[$name])$($dot)Exit 0" }
    }
    if (Find-Element 'Load command' $button) { throw 'Per-host assignments cannot load as a shared draft' }
    if (Find-Element $targets['Witness']) { throw 'The hidden host appeared in the export' }
}

$launch = @('--profiles',('"' + $profiles + '"'),'--profile','"Fleet East"','--tab','"Fleet West"',
    '--tab','Witness','--cols','95','--rows','34')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $sessions = @((Control @('list')).sessions)
    if ($sessions.Count -ne 3) { throw 'Expected three SSH panes' }
    $targets = @{}
    foreach ($session in $sessions) {
        $name = $session.label
        $port = $config.$name.port
        $targets[$name] = "$name$($dot)test@127.0.0.1:$port$($dot)Pane $($session.id)"
        $null = Control @('wait-for-text','--pane',"$($session.id)",'--timeout','15','fleet')
    }
    $west = ($sessions | Where-Object label -eq 'Fleet West').id
    Run-Palette 'SSH host group'
    Click 'Select shown'
    foreach ($target in $targets.Values) { $null = Wait-CheckBox $target On }
    (Field 'Host filter').SetValue('Fleet')
    (Field 'Group command').SetValue('working draft')
    $null = Wait-Until 'the checked witness is hidden' { Find-Element '1 selected host is hidden by the filter.' }
    Click $entry
    $eastPosix = "export POLYSH_NR_SHELLS=2 POLYSH_RANK=0 POLYSH_NAME='127.0.0.1' POLYSH_DISPLAY_NAME='Fleet East'"
    $westPosix = "export POLYSH_NR_SHELLS=2 POLYSH_RANK=1 POLYSH_NAME='127.0.0.1' POLYSH_DISPLAY_NAME='Fleet West'"
    $null = Wait-Until 'ordered assignments for the two shown hosts' {
        (Find-Element $eastPosix) -and (Find-Element $westPosix) -and !(Find-Element $targets['Witness'])
    }
    $null = Control @('send-text','--pane',"$west",'--no-paste','alt')
    $null = Control @('wait-for-text','--pane',"$west",'--timeout','15','alternate-proof')
    $null = Wait-Until 'stale review disables the entire export' {
        $submit = Find-Element 'Export to 2 hosts' $button
        $submit -and !$submit.Current.IsEnabled -and
            (Find-Element 'The hosts or their prompts changed. Return to the roster and review again.')
    }
    Click 'Back to host group'
    $null = Control @('send-text','--pane',"$west",'--no-paste','prompt')
    Click $entry
    Click 'Export to 2 hosts'
    Wait-SendResult
    if ((Field 'Group command').Current.Value -cne 'working draft') { throw 'The group draft changed' }
    Click 'Copy results'
    $null = Wait-Until 'export report includes each reviewed assignment' {
        $report = Read-ClipboardFixture
        $report -and $report.Contains($eastPosix) -and $report.Contains($westPosix)
    }
    Click $entry
    Select-Fish
    $westFish = "set -gx POLYSH_NR_SHELLS 2 && set -gx POLYSH_RANK 1 && set -gx POLYSH_NAME '127.0.0.1' && set -gx POLYSH_DISPLAY_NAME 'Fleet West'"
    $null = Wait-Until 'review shows the selected Fish syntax' { Find-Element $westFish }
    Click 'Export to 2 hosts'
    Wait-SendResult
    $null = Wait-Until 'Fish assignment appears in the send report' { Find-Element $westFish }
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
