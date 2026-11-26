param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_UI_SMOKE = '1'
$env:W11TERM_WORD_SELECTION_SMOKE = $State
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$download = 'Download' + [char]0x2026
# The existing loopback-only pointer harness dispatches two clicks to a known
# terminal cell. Disable copy-on-select in this fixture's isolated settings.
$settings = [IO.File]::ReadAllText($env:W11TERM_SETTINGS_PATH) | ConvertFrom-Json
$settings | Add-Member NoteProperty interaction ([pscustomobject]@{copyOnSelect=$false})
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, ($settings | ConvertTo-Json -Depth 8))
$trace = [IO.File]::ReadAllText((Join-Path $State 'drawer-key-trace'))

function Focus-Row([string]$Name) {
    (Wait-Until "file row '$Name'" { Find-Element $Name $button }).SetFocus()
    $null = Wait-Until "keyboard focus on '$Name'" { (Find-Element $Name $button).Current.HasKeyboardFocus }
}
function Select-Row([string]$Name, [bool]$Directory, [string]$ExpectedWire) {
    Focus-Row $Name
    $null = Press-FocusedKey 0x20
    $null = Wait-Until "Space selected '$Name'" {
        if ([Text.Encoding]::ASCII.GetString((Read-PeerTrace $traceReader)) -cne $ExpectedWire) {
            throw "A native drawer keystroke reached the SSH channel while '$Name' had focus"
        }
        (Find-Element 'Open folder' $button).Current.IsEnabled -eq $Directory -and (Find-Element $download $button).Current.IsEnabled -ne $Directory
    }
}
function Focus-TerminalCanvas {
    $request = Join-Path $State 'double-click'
    $pending = Join-Path $State 'double-click.tmp'
    [IO.File]::WriteAllText($pending,'ready')
    [IO.File]::Move($pending,$request)
    $null = Wait-Until 'click on the terminal canvas' { !(Test-Path -LiteralPath $request) }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
$scenarioFailure = $null
try {
    $traceReader = Open-PeerTraceReader $trace
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $root.GetCurrentPattern([System.Windows.Automation.TransformPattern]::Pattern).Resize(1150,900)
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    Run-Palette 'SFTP files'
    Select-Row 'projects/' $true ''
    Select-Row 'note.txt' $false ''

    Focus-Row 'note.txt'
    $null = Open-Palette 'command search from a focused file row'
    $null = Press-FocusedKey 0x1b
    $null = Wait-Until 'command palette closed' { !(Find-Element 'Search commands') }
    Focus-Row 'note.txt'
    Focus-TerminalCanvas
    foreach ($ch in 'canvas-typed'.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','10','canvas-typed')
    Select-Row 'projects/' $true 'canvas-typed'
    Select-Row 'note.txt' $false 'canvas-typed'

    Invoke-Button (Wait-Until 'Find files button' { Find-Element ('Find files' + [char]0x2026) $button })
    $null = Wait-SftpSearchFocus 'focused file search query'
    $null = Press-FocusedKey 0x51
    $null = Wait-Until 'the first key goes to the new search field' {
        $query = Find-Element 'Filename contains' ([System.Windows.Automation.ControlType]::Edit)
        if ([Text.Encoding]::ASCII.GetString((Read-PeerTrace $traceReader)) -cne 'canvas-typed') {
            throw 'The search keystroke reached SSH'
        }
        $query -and $query.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq 'q'
    }
    Focus-TerminalCanvas
    foreach ($ch in 'search-canvas-typed'.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','10','search-canvas-typed')
    $query = Find-Element 'Filename contains' ([System.Windows.Automation.ControlType]::Edit)
    if (!$query -or $query.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -cne 'q') {
        throw 'Returning to the terminal changed or closed the search'
    }
} catch {
    $scenarioFailure = $_
    throw
} finally {
    if ($traceReader) { $traceReader.Dispose() }
    try { Close-FixtureApp $app } catch {
        if ($scenarioFailure) {
            throw "$scenarioFailure at $($scenarioFailure.ScriptStackTrace)`nGUI cleanup also failed: $_"
        }
        throw
    }
}
