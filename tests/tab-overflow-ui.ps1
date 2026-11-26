param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$config = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
$config.profiles | Add-Member -NotePropertyName 'Last shell' -NotePropertyValue $config.profiles.'Palette PowerShell'
[IO.File]::WriteAllText($profiles, ($config | ConvertTo-Json -Depth 8))
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"find_tab":"F10","select_tab_1":"F8"}}')
$tab = [System.Windows.Automation.ControlType]::TabItem
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$selection = [System.Windows.Automation.SelectionItemPattern]::Pattern

function Wait-Header([string]$Name, [string]$Stage) {
    $null = Wait-Until "visible and selected header '$Name' $Stage" {
        $header = Find-Element $Name $tab -Visible
        $plus = Find-Element 'New tab' $button -Visible
        if (!$header -or !$plus) { return }
        $rect = $header.Current.BoundingRectangle
        $rect.Width -gt 0 -and $rect.Left -ge $root.Current.BoundingRectangle.Left -and
            $rect.Right -le $plus.Current.BoundingRectangle.Left -and
            $header.GetCurrentPattern($selection).Current.IsSelected
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile',
    'palette-ssh', '--cols', '72', '--rows', '24') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $ssh = (Wait-Until 'SSH session' { (Control @('list')).sessions | Where-Object label -eq 'palette-ssh' }).id
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','ready')
    foreach ($index in 1..8) { $null = Control @('open','--profile','Palette PowerShell') }
    $null = Control @('open','--profile','Last shell')
    Press-Key 0x77 # F8 selects the first tab.
    Wait-Focus $ssh
    Wait-Header 'palette-ssh' 'after opening background tabs'
    $null = Wait-Until 'the far end of the tab strip is offscreen' { !(Find-Element 'Last shell' $tab -Visible) }

    Press-Key 0x79 # F10 opens Find tab.
    $field = Wait-PickerEditor 'Search tabs' 'F10 for the last tab'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Last shell')
    $null = Wait-Until 'the last tab matches the search' { Find-Element 'Last shell    Tab 10' $button }
    Press-Key 13
    $last = ((Control @('list')).sessions | Where-Object label -eq 'Last shell').id
    Wait-Focus $last
    Wait-Header 'Last shell' 'after selecting it from Find tab'
    $null = Wait-Until 'the first header scrolled out of view' { !(Find-Element 'palette-ssh' $tab -Visible) }

    $transform = $root.GetCurrentPattern([System.Windows.Automation.TransformPattern]::Pattern)
    $initial = $root.Current.BoundingRectangle
    $scale = [FixtureMessages]::GetDpiForWindow($window) / 96.0
    $narrow = [Math]::Round(480 * $scale)
    $transform.Resize($narrow, $initial.Height)
    $null = Wait-Until 'the window narrows around the last tab' {
        $root.Current.BoundingRectangle.Width -lt $initial.Width - 40 * $scale
    }
    try { Wait-Header 'Last shell' 'after narrowing the window' } catch {
        $header = Find-Element 'Last shell' $tab
        throw "$_ after resizing from $($initial.Width) to $($root.Current.BoundingRectangle.Width) pixels. Header bounds: $($header.Current.BoundingRectangle)"
    }

    Press-Key 0x77
    Wait-Focus $ssh
    Wait-Header 'palette-ssh' 'after switching back with F8'
    foreach ($ch in 'still-focused'.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','12','still-focused')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
