[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/cursor-cell.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_TEST_CARET_BLINK_MS = '200'
$profiles = Join-Path $State 'profiles.json'

function Sample-Cell {
    [CursorCellProbe]::Sample($window, $bounds.X, $bounds.Y, $bounds.Width, $bounds.Height)
}
function Test-CursorInk {
    $pixels = [CursorCellProbe]::ReadPixels($window, $window, $bounds.X, $bounds.Y,
        $bounds.Width, $bounds.Height)
    foreach ($pixel in $pixels) {
        if ((($pixel -shr 16) -band 255) -gt 180 -and (($pixel -shr 8) -band 255) -lt 80 -and
            ($pixel -band 255) -gt 180) { return $true }
    }
    return $false
}
function Read-Generation {
    (Control @('get-text','--pane',"$pane")).generation
}
function Read-Appearance {
    # Settings are replaced atomically. Allow that replacement while reading the prior file.
    try {
        $stream = [IO.File]::Open($env:W11TERM_SETTINGS_PATH, [IO.FileMode]::Open,
            [IO.FileAccess]::Read, ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
    } catch [IO.FileNotFoundException] {
        return $null
    }
    $reader = [IO.StreamReader]::new($stream)
    try { $reader.ReadToEnd() | ConvertFrom-Json } finally { $reader.Dispose() }
}
function Send-Sequence([string]$Suffix) {
    $before = Read-Generation
    $bytes = [Text.Encoding]::ASCII.GetBytes(([string][char]27) + $Suffix)
    $null = Control @('send-text','--pane',"$pane",'--no-paste','--base64',[Convert]::ToBase64String($bytes))
    Wait-Until "SSH peer to echo $Suffix" { $g = Read-Generation; if ($g -gt $before) { $g } }
}

function Set-CursorPreference([string]$Name, [string]$Choice, [string]$Property, [string]$Value) {
    $combo = [System.Windows.Automation.ControlType]::ComboBox
    $listItem = [System.Windows.Automation.ControlType]::ListItem
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $selection = [System.Windows.Automation.SelectionPattern]::Pattern
    $selectItem = [System.Windows.Automation.SelectionItemPattern]::Pattern
    $field = Wait-Until "'$Name' dropdown" { Find-Element $Name $combo }
    $field.SetFocus()
    $null = Wait-Until "'$Name' selects '$Choice'" {
        try {
            $field = Find-Element $Name $combo -Visible
            if (!$field -or !$field.Current.IsEnabled) { return }
            $selected = $field.GetCurrentPattern($selection).Current.GetSelection()
            if ($selected.Count -eq 1 -and $selected[0].Current.Name -ceq $Choice) { return $true }

            $popup = $field.GetCurrentPattern($expand)
            if ($popup.Current.ExpandCollapseState -eq 'Collapsed') { $popup.Expand() }
            $item = Find-Element $Choice $listItem -Visible
            if ($item -and $item.Current.IsEnabled) { $item.GetCurrentPattern($selectItem).Select() }
        } catch [System.Windows.Automation.ElementNotAvailableException] {
            # WinUI recreates the popup elements while applying a selection.
        }
    }
    $null = Wait-Until "'$Name' persists '$Value'" { (Read-Appearance).$Property -ceq $Value }
    $popup = (Find-Element $Name $combo).GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
    $tab = Wait-Until 'SSH terminal tab' {
        Find-Element 'palette-ssh' ([System.Windows.Automation.ControlType]::TabItem)
    }
    $tab.GetCurrentPattern($selectItem).Select()
    Wait-Focus $pane
}

function Assert-Blinking {
    $last = $null
    $transitions = 0
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($transitions -lt 3 -and $timer.ElapsedMilliseconds -lt 6000) {
        $sample = Sample-Cell
        if ($sample -ne $shown -and $sample -ne $hidden) { throw 'Glyph or cell content changed during blink' }
        if ($null -ne $last -and $sample -ne $last) { $transitions++ }
        $last = $sample
        Start-Sleep -Milliseconds 35
    }
    if ($transitions -lt 3) { throw "Saw only $transitions blinking cursor transitions on an idle pane" }
}
function Assert-Steady {
    $null = Wait-Until 'steady bar restored' { (Sample-Cell) -eq $shown }
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.ElapsedMilliseconds -lt 850) {
        if ((Sample-Cell) -ne $shown) { throw 'Steady cursor blinked' }
        Start-Sleep -Milliseconds 35
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','60','--rows','20') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    Show-CursorWindow
    # Position the cursor after the restore, before measuring the live cell.
    $esc = [char]27
    $bell = [char]7
    $null = Send-Sequence "[2J$esc]12;#ff00ff$bell$esc[4;4HBlinkProbe$esc[4;4H$esc[6 q$esc[?25h"
    $bounds = Get-CursorCell 'palette-ssh' 'BlinkProbe'
    # Identify the actual cursor frame by its color. The marker can appear in UI
    # Automation before the canvas has displayed it or the bar has been drawn.
    $shown = Wait-Until 'steady bar visible in the native canvas' {
        if (Test-CursorInk) { Sample-Cell }
    }
    $null = Send-Sequence '[?25l'
    $hidden = Wait-Until 'the native cell with its bar hidden' {
        if (!(Test-CursorInk)) { Sample-Cell }
    }
    $null = Send-Sequence '[?25h'
    $null = Wait-Until 'the original bar pixels restored' { (Sample-Cell) -eq $shown }

    $generation = Send-Sequence '[5 q'
    $beforePng = Join-Path $State 'blink-before.png'
    $afterPng = Join-Path $State 'blink-after.png'
    $null = Control @('screenshot','--pane',"$pane",$beforePng)
    Assert-Blinking
    $null = Control @('screenshot','--pane',"$pane",$afterPng)
    $samePng = [Collections.StructuralComparisons]::StructuralEqualityComparer.Equals(
        [IO.File]::ReadAllBytes($beforePng), [IO.File]::ReadAllBytes($afterPng))
    if ((Read-Generation) -ne $generation -or !$samePng) {
        throw 'Blinking changed the terminal generation or deterministic PNG capture'
    }

    $null = Send-Sequence '[6 q'
    Assert-Steady
    $null = Send-Sequence '[?12h'
    Assert-Blinking
    $null = Send-Sequence '[?12l'
    Assert-Steady

    $null = Send-Sequence '[2 q'
    $block = Wait-Until 'application block cursor' {
        $sample = Sample-Cell
        if ($sample -ne $hidden -and $sample -ne $shown) { $sample }
    }
    Run-Palette 'Settings'
    Set-CursorPreference 'Default cursor style' 'Steady bar' 'default_cursor_style' 'steady_bar'
    $null = Wait-Until 'application cursor still overrides the preference' { (Sample-Cell) -eq $block }
    $null = Send-Sequence '[0 q'
    Assert-Steady

    $null = Send-Sequence "[4;20HGuideProbe$esc[4;4H"
    $guideBounds = Get-CursorCell 'palette-ssh' 'GuideProbe'
    function Sample-Guide {
        [CursorCellProbe]::Sample($window, $guideBounds.X, $guideBounds.Y, $guideBounds.Width, $guideBounds.Height)
    }
    $noGuide = Sample-Guide
    $null = Send-Sequence (']1337;HighlightCursorLine=yes' + [char]7)
    if ((Sample-Guide) -ne $noGuide) { throw 'Cursor guide is visible with the setting off' }
    Run-Palette 'Settings'
    Set-CursorPreference 'Cursor guide' 'Follow applications' 'cursor_guide' 'follow_applications'
    $null = Wait-Until 'application guide visible on the original row' { (Sample-Guide) -ne $noGuide }
    $null = Send-Sequence '[?25l'
    $null = Wait-Until 'guide hidden with the caret' { (Sample-Guide) -eq $noGuide }
    $null = Send-Sequence '[?25h'
    $null = Wait-Until 'guide restored with the caret' { (Sample-Guide) -ne $noGuide }
    $null = Send-Sequence (']1337;HighlightCursorLine=no' + [char]7)
    $null = Wait-Until 'application hides the guide' { (Sample-Guide) -eq $noGuide }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
