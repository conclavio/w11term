[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","select_tab_1":"F8","focus_right":"F6"}}')
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$text = [System.Windows.Automation.ControlType]::Text
$valuePattern = [System.Windows.Automation.ValuePattern]::Pattern
$directory = '/projects/' + [char]0x96ea + '-orbit'

function Search-Field { Find-Element 'Search remote sessions' $edit }
function Click([string]$Label) {
    Invoke-Button (Wait-Until "enabled $Label" {
        $item = Find-Element $Label $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Search-Rows {
    $elements = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $button)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $elements) |
        ForEach-Object { $_.Current.Name } | Where-Object { $_.StartsWith('Attach to ') }
}
function Wait-Rows([string]$Count, [int]$Visible, [string]$Contains = '') {
    $actual = @{ rows = @() }
    try {
        $null = Wait-Until "$Count with $Visible listed rows matching '$Contains'" {
            if (!(Find-Element $Count $text)) { return $false }
            $actual.rows = @(Search-Rows)
            $actual.rows.Count -eq $Visible -and (!$Contains -or
                @($actual.rows | Where-Object { $_.Contains($Contains) }).Count -gt 0)
        }
    } catch { throw "$($_.Exception.Message). Listed rows: $($actual.rows -join '; ')" }
}
function Set-Query([string]$Query) {
    $field = Wait-Until 'mounted remote session search' { Search-Field }
    $field.GetCurrentPattern($valuePattern).SetValue($Query)
}
function Search-Key([uint32]$Key) {
    $field = Wait-Until 'mounted remote session search' { Search-Field }
    $field.SetFocus()
    $null = Wait-Until 'remote session search focus' { (Search-Field).Current.HasKeyboardFocus }
    $null = Press-FocusedKey $Key
}
function Shell-Pid([string]$Tag, [int]$Pane = $localPane) {
    $line = "'$Tag=' + " + '$PID'
    foreach ($c in $line.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $pattern = "$Tag=([0-9]+)"
    $frame = Control @('wait-for-text','--pane',"$Pane",'--regex','--timeout','15',$pattern)
    return [int]([regex]::Match($frame.text,$pattern).Groups[1].Value)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $localPane = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','20','PS ')
    $pidBefore = Shell-Pid 'browse-before'
    Run-Palette 'Find sessions on palette-ssh'
    $browser = Wait-Until 'remote browser pane' { ((Control @('list')).sessions | Where-Object id -ne $localPane).id }
    Wait-Focus $browser
    Wait-Rows '38 sessions' 32
    Run-Palette 'Split right with Palette PowerShell'
    $neighbor = Wait-Until 'new PowerShell split' {
        ((Control @('list')).sessions | Where-Object { $_.id -ne $localPane -and $_.id -ne $browser }).id
    }
    Wait-Focus $neighbor
    $null = Control @('wait-for-text','--pane',"$neighbor",'--timeout','15','PS ')
    $neighborPid = Shell-Pid 'neighbor-before' $neighbor

    # Invoke the native button used for clicks on the inactive browser. Check
    # its actual hit area includes the search box without moving the OS pointer.
    $activation = Wait-Until 'inactive browser activation surface over its search box' {
        $field = Search-Field
        $item = Find-Element 'Activate remote session browser' $button
        if ($field -and !$field.Current.IsEnabled -and $item -and $item.Current.IsEnabled) {
            $bounds = $field.Current.BoundingRectangle
            if ($bounds.Width -gt 0 -and $bounds.Height -gt 0 -and
                $item.Current.BoundingRectangle.Contains($bounds)) { $item }
        }
    }
    Invoke-Button $activation
    Wait-Focus $browser
    $null = Wait-Until 'search field focused by the click' { (Search-Field).Current.HasKeyboardFocus }
    $null = Press-FocusedKey 0x51
    $null = Wait-Until 'typed q routed to the newly active search field' {
        (Search-Field).GetCurrentPattern($valuePattern).Current.Value -ceq 'q'
    }
    Wait-Rows 'No matching sessions' 0
    Set-Query ''
    Wait-Rows '38 sessions' 32
    Search-Key 13 # Empty query leaves all sessions available.
    Wait-Rows '38 sessions' 32
    Click 'Next sessions'
    Wait-Rows '38 sessions' 6 $directory
    Click 'Previous sessions'
    Wait-Rows '38 sessions' 32
    if (@(Search-Rows | Where-Object { $_.Contains($directory) }).Count) { throw 'Page one unexpectedly includes the directory target' }

    Set-Query $directory
    Wait-Rows '1 match' 1 'Survey-Recorder'
    Set-Query 'SURVEY-RECORDER'
    Wait-Rows '1 match' 1 $directory
    Set-Query '$8'
    Wait-Rows '1 match' 1 'Night shift ($8)'
    Set-Query 'no-such-hosted-session'
    Wait-Rows 'No matching sessions' 0
    Search-Key 13
    Wait-Rows 'No matching sessions' 0

    Set-Query 'night shif'
    Search-Key 0x23 # End, including when UI Automation left the caret at the beginning.
    Search-Key 0x54 # Type the last t into the browser's native edit.
    $null = Wait-Until 'actual typed character in the session query' { (Search-Field).GetCurrentPattern($valuePattern).Current.Value -ceq 'night shift' }
    Wait-Rows '2 matches' 2 'Night shift ($7)'
    Search-Key 13
    Wait-Rows '2 matches' 2 'Night shift ($8)'
    Click 'Refresh'
    Wait-Rows '1 match' 1 'Night shift ($7)'
    if ((Search-Field).GetCurrentPattern($valuePattern).Current.Value -cne 'night shift') { throw 'Refresh discarded the active query' }
    Search-Key 13
    $null = Control @('wait-for-text','--pane',"$browser",'--timeout','15','restore-screen')
    if (Search-Field) { throw 'Browser search stayed open after attaching' }
    Wait-Focus $browser
    foreach ($c in 'attached-search-pane'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$browser",'--timeout','15','attached-search-pane')
    Press-Key 0x75 # F6 focuses the PowerShell split on the right.
    Wait-Focus $neighbor
    if ((Shell-Pid 'neighbor-after' $neighbor) -ne $neighborPid) { throw 'Activating search changed the neighboring PowerShell process or sent it input' }
    Press-Key 0x77 # F8 selects the original local shell tab in this fixture.
    Wait-Focus $localPane
    if ((Shell-Pid 'browse-after') -ne $pidBefore) { throw 'Browsing replaced the neighboring PowerShell process' }
} catch {
    throw "$($_.Exception.Message) at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
}
