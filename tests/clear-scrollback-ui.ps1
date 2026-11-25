param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"clear_scrollback":"F6","command_palette":"F7"}}')
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$esc = [string][char]27
$bell = [string][char]7
$archive = 'ssh-archived-order'

function Send-Bytes([int]$Pane, [string]$Text) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Text))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$encoded)
}
function Export-Text([int]$Pane, [string]$Name) {
    $path = Join-Path $State $Name
    $null = Control @('save-text','--pane',"$Pane",$path)
    return [IO.File]::ReadAllText($path)
}
function Expect-Find([int]$Pane, [string]$Term, [int]$Count) {
    $rows = (Control @('find-text','--pane',"$Pane",$Term)).panes
    if ($rows.Count -ne 1 -or $rows[0].pane -ne $Pane -or $rows[0].count -ne $Count -or $rows[0].truncated) {
        throw "Unexpected Find results for '$Term' in pane $Pane. $($rows | ConvertTo-Json -Compress)"
    }
}
function Expect-Action([bool]$Enabled) {
    $field = Open-Palette 'Clear scrollback'
    $field.GetCurrentPattern($value).SetValue('Clear scrollback')
    $null = Wait-Until "Clear scrollback command enabled=$Enabled" {
        $row = Find-Element 'Clear scrollback    F6' $button
        $row -and $row.Current.IsEnabled -eq $Enabled
    }
    Press-Key 27
    $null = Wait-Until 'command palette dismissed' { !(Find-Element 'Search commands' $edit) }
}
function Wait-Viewport([string]$Witness) {
    $null = Wait-Until "SSH viewport showing '$Witness'" {
        $doc = Find-Element ('Terminal ' + [char]0xb7 + ' palette-ssh')
        if ($doc) { $doc.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1).Contains($Witness) }
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','120','--rows','24') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $sshPane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$sshPane",'--timeout','15','ssh-history-ready')
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'neighboring PowerShell' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','PS ')
    # Split output markers so the echoed command cannot satisfy the searches.
    $seed = 'function global:prompt { "PS fixture> " }; $global:clearProof=$PID; "neighbor-archive-" + "42"; 1..90 | % { "neighbor row $_" }; "neighbor-ready-" + "42"'
    Send-Bytes $localPane ($seed + [char]13)
    $null = Control @('wait-for-text','--pane',"$localPane",'--regex','--timeout','15','neighbor-ready-42 *\r?\nPS fixture>')
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $sshPane
    $panes = (Control @('list')).sessions
    $sshSession = $panes | Where-Object id -eq $sshPane
    if ($panes.Count -ne 2 -or $sshSession.tab_id -ne ($panes | Where-Object id -eq $localPane).tab_id) { throw 'Expected one mixed split tab' }

    Send-Bytes $sshPane ($esc + '[?1049h' + $esc + '[Halt-pilot')
    $null = Control @('wait-for-text','--pane',"$sshPane",'--timeout','15','alt-pilot')
    if (!(Control @('get-text','--pane',"$sshPane")).alternate_screen) { throw 'Expected alternate screen' }
    Expect-Action $false
    Send-Bytes $sshPane ($esc + '[?1049l' + [Environment]::NewLine + 'live-heading' + [Environment]::NewLine + $esc + ']133;A' + $bell + 'pilot> ' + $esc + ']133;B' + $bell)
    $null = Control @('wait-for-text','--pane',"$sshPane",'--timeout','15','pilot>')
    Expect-Action $true
    Expect-Find $sshPane $archive 1
    $sshBefore = Export-Text $sshPane 'ssh-before.txt'
    $localBefore = Export-Text $localPane 'local-before.txt'
    $localLive = (Control @('get-text','--pane',"$localPane")).text
    if (!$sshBefore.Contains($archive) -or !$localBefore.Contains('neighbor-archive-42') -or $localLive.Contains('neighbor-archive-42')) {
        throw 'Expected both panes to have offscreen history'
    }

    Run-Palette 'Find in scrollback    Ctrl+Shift+F'
    $field = Wait-Until 'local Find' { Find-Element 'Find in scrollback' $edit }
    $field.GetCurrentPattern($value).SetValue($archive)
    $statusId = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'find-status')
    $null = Wait-Until 'one local SSH Find match' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $status = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $statusId)
        $status -and $status.Current.Name -ceq '1 / 1'
    }
    Press-Key 27
    $null = Wait-Until 'local Find dismissed' { !(Find-Element 'Find in scrollback' $edit) }
    Wait-Viewport $archive
    $live = (Control @('get-text','--pane',"$sshPane")).text
    if ($live.Contains($archive) -or !$live.Contains('live-heading') -or !$live.Contains('pilot>')) { throw 'Expected the live prompt below the scrolled viewport' }

    Press-Key 0x75 # F6 invokes Clear scrollback from the scrolled SSH pane.
    Wait-Viewport 'pilot>'
    if ((Control @('get-text','--pane',"$sshPane")).text -cne $live) { throw 'Clearing scrollback changed the live SSH grid' }
    Expect-Find $sshPane $archive 0
    Expect-Find $sshPane 'live-heading' 1
    $sshAfter = Export-Text $sshPane 'ssh-after.txt'
    # All fixture rows are shorter than this split. Normalize the export's CRLF
    # and its final prompt space to compare every retained row with the live grid.
    if ($sshAfter.Replace([Environment]::NewLine, [string][char]10).TrimEnd() -cne $live.TrimEnd()) {
        throw 'Cleared SSH export differs from the live grid'
    }
    Expect-Action $false
    Expect-Find $localPane 'neighbor-archive-42' 1
    if ((Export-Text $localPane 'local-after.txt') -cne $localBefore -or (Control @('get-text','--pane',"$localPane")).text -cne $localLive) {
        throw 'Clearing the SSH pane changed the neighboring PowerShell pane'
    }
    Wait-Focus $sshPane
    $current = (Control @('list')).sessions | Where-Object id -eq $sshPane
    if ($current.connection_generation -ne $sshSession.connection_generation -or $current.tab_id -ne $sshSession.tab_id) { throw 'SSH connection or tab was replaced' }
    foreach ($c in 'status'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$sshPane",'--timeout','15','pilot> status')
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $localPane
    foreach ($c in 'if ($clearProof -eq $PID) { "local-still-" + "alive" }'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','local-still-alive')
    Run-Palette 'Clear scrollback    F6'
    $null = Wait-Until 'palette clears the selected PowerShell history' {
        $result = (Control @('find-text','--pane',"$localPane",'neighbor-archive-42')).panes
        $result.Count -eq 1 -and $result[0].count -eq 0 -and $null -eq $result[0].error
    }
    Expect-Find $sshPane 'pilot> status' 1
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
