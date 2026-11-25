[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$picker = 'Search scrollback bookmarks'
$find = 'Find scrollback bookmark' + [char]0x2026

function Document {
    Find-Element ('Terminal ' + [char]0xb7 + ' palette-ssh') ([System.Windows.Automation.ControlType]::Document)
}

function Visible {
    try {
        $doc = Document
        if ($doc) {
            $pattern = $doc.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
            $pattern.GetVisibleRanges()[0].GetText(-1)
        }
    } catch {
        if ($_.Exception.GetBaseException() -isnot [System.Windows.Automation.ElementNotAvailableException]) { throw }
    }
}

function Scroll-Viewport([string]$Description, [scriptblock]$Change) {
    $null = Wait-Until $Description {
        try {
            $doc = Document
            if ($doc) {
                $scroll = $doc.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
                if ($scroll.Current.VerticallyScrollable) {
                    & $Change $scroll
                    return $true
                }
            }
        } catch {
            if ($_.Exception.GetBaseException() -isnot [System.Windows.Automation.ElementNotAvailableException]) { throw }
        }
    }
}

function Buttons {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $type = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button)
    foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $type)) {
        if ($item.Current.IsEnabled) { $item.Current.Name }
    }
}

function Bookmark-Button([string]$Prefix) {
    Buttons | Where-Object { $_.StartsWith($Prefix) } |
        Select-Object -First 1
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','deploy-stream-ready')
    Scroll-Viewport 'scroll to oldest row' { param($scroll) $scroll.SetScrollPercent(-1, 0) }
    $null = Wait-Until 'oldest row' { (Visible) -like 'ready*' }
    Scroll-Viewport 'scroll to Alpha anchor' {
        param($scroll)
        $scroll.Scroll([System.Windows.Automation.ScrollAmount]::NoAmount, [System.Windows.Automation.ScrollAmount]::SmallIncrement)
    }
    $null = Wait-Until 'Alpha anchor' { (Visible) -like 'Deploy Alpha checkpoint*' }
    Run-Palette 'Toggle scrollback bookmark'

    Scroll-Viewport 'scroll to Omega anchor' {
        param($scroll)
        $scroll.Scroll([System.Windows.Automation.ScrollAmount]::NoAmount, [System.Windows.Automation.ScrollAmount]::SmallIncrement)
    }
    $null = Wait-Until 'Omega anchor' { (Visible) -like 'Deploy Omega checkpoint*' }
    Run-Palette 'Toggle scrollback bookmark'

    Scroll-Viewport 'return to live output' { param($scroll) $scroll.SetScrollPercent(-1, 100) }
    $null = Wait-Until 'live tail' { (Visible) -like '*deploy-stream-ready*' }
    $before = Wait-Until 'live SSH viewport' { Visible }
    Run-Palette $find
    $field = Wait-PickerEditor $picker 'Find scrollback bookmark'
    try {
        $null = Wait-Until 'both bookmark previews' {
            (Bookmark-Button 'Deploy Alpha checkpoint') -and (Bookmark-Button 'Deploy Omega checkpoint')
        }
    } catch {
        throw "$_ Visible buttons: $((Buttons) -join ', ')"
    }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('missing-marker')
    $null = Wait-Until 'empty bookmark search' { Find-Element 'No matching scrollback bookmarks' }
    Press-Key 27
    $null = Wait-Until 'bookmark search closes without scrolling' {
        !(Find-Element $picker) -and (Visible) -ceq $before
    }

    Run-Palette $find
    $field = Wait-PickerEditor $picker 'Find scrollback bookmark'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('ALPHA checkpoint')
    $null = Wait-Until 'filtered Alpha bookmark' {
        (Bookmark-Button 'Deploy Alpha checkpoint') -and !(Bookmark-Button 'Deploy Omega checkpoint')
    }
    Press-Key 13
    $null = Wait-Until 'selected bookmark at the top of the SSH viewport' {
        !(Find-Element $picker) -and (Visible) -like 'Deploy Alpha checkpoint*'
    }
    Wait-Focus $pane
    foreach ($c in 'after-bookmark'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','10','after-bookmark')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
