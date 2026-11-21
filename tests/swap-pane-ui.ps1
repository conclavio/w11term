[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$workspaces = Join-Path $env:W11TERM_DATA_DIR 'workspaces.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","swap_pane_up":"F8","focus_right":"F9","focus_down":"F10","focus_up":"F4","focus_left":"F6"}}')

function Read-Layout {
    $panes = @((Control @('list')).sessions)
    if ($panes.Count -ne 3 -or @($panes.tab_id | Sort-Object -Unique).Count -ne 1) { return }
    $byLabel = @{}
    foreach ($pane in $panes) {
        if (!$pane.visible -or $pane.connection_generation -le 0) { return }
        $byLabel[$pane.label] = $pane
    }
    if ($byLabel.ContainsKey('palette-ssh') -and $byLabel.ContainsKey('Upper PowerShell') -and
        $byLabel.ContainsKey('Lower PowerShell')) { return $byLabel }
}
function Focused-Rectangle([int]$Pane) {
    Wait-Focus $Pane
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, "w11term-pane-$Pane")
    return Wait-Until "accessible terminal bounds for focused pane $Pane" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $view = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,$condition)
        if ($view) {
            $rect = $view.Current.BoundingRectangle
            if (!$rect.IsEmpty -and $rect.Width -gt 0 -and $rect.Height -gt 0) { $rect }
        }
    }
}
function Measure-Positions($Panes, [string]$AtLeft, [string]$AtTop, [string]$AtBottom) {
    # w11term exposes its focused terminal to UI Automation. Visit all three
    # native rectangles and restore the original SSH focus after measuring.
    Wait-Focus $Panes['palette-ssh'].id
    if ($AtLeft -ne 'palette-ssh') { Press-Key 0x75 } # F6 left
    $left = Focused-Rectangle $Panes[$AtLeft].id
    Press-Key 0x78 # F9 right selects the longest shared edge
    $bottom = Focused-Rectangle $Panes[$AtBottom].id
    Press-Key 0x73 # F4 up
    $top = Focused-Rectangle $Panes[$AtTop].id
    if ($AtLeft -eq 'palette-ssh') { Press-Key 0x75 }
    if ($AtBottom -eq 'palette-ssh') { Press-Key 0x79 } # F10 down
    Wait-Focus $Panes['palette-ssh'].id
    if (!($left.Right -le $top.Left -and $top.Bottom -le $bottom.Top -and
            $top.Left -eq $bottom.Left -and $bottom.Height -gt $top.Height)) {
        throw "Unexpected native pane rectangles: $left / $top / $bottom"
    }
    return @($left,$top,$bottom)
}
function Same-Grid($Actual, $Original) {
    return $Actual.cols -eq $Original.cols -and $Actual.rows -eq $Original.rows
}
function Wait-Swap([string]$AtLeft, [string]$AtTop, [string]$AtBottom) {
    $result = Wait-Until "the same three panes at $AtLeft / $AtTop / $AtBottom" {
        $now = Read-Layout
        if (!$now -or !$now['palette-ssh'].active) { return }
        foreach ($name in $original.Keys) {
            $a = $original[$name]
            $b = $now[$name]
            if ($a.id -ne $b.id -or $a.tab_id -ne $b.tab_id -or $a.window_id -ne $b.window_id -or
                $a.connection_generation -ne $b.connection_generation) { return }
        }
        if ((Same-Grid $now[$AtLeft] $original['palette-ssh']) -and
            (Same-Grid $now[$AtTop] $original['Upper PowerShell']) -and
            (Same-Grid $now[$AtBottom] $original['Lower PowerShell'])) { $now }
    }
    $positions = Measure-Positions $result $AtLeft $AtTop $AtBottom
    for ($i = 0; $i -lt 3; $i++) {
        if (!$positions[$i].Equals($baseline[$i])) { throw "Slot $i changed from $($baseline[$i]) to $($positions[$i])" }
    }
}
function Send-FocusedSsh([int]$Pane, [string]$Text) {
    Wait-Focus $Pane
    foreach ($char in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$char) }
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15',$Text)
}
function Shell-Identity([int]$Pane, [string]$Marker, [string]$Value, [switch]$Initialize) {
    $command = if ($Initialize) { '$global:SwapValue="' + $Value + '"; ' } else { '' }
    $command += "'" + $Marker + "=' + " + '$PID + "/" + $global:SwapValue' + [char]13
    $bytes = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($command))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$bytes)
    $pattern = "$Marker=([0-9]+)/$Value"
    $text = (Control @('wait-for-text','--pane',"$Pane",'--timeout','15','--regex',$pattern)).text
    return [regex]::Match($text,$pattern).Groups[1].Value
}
function Assert-NoLeftNeighbor {
    $label = 'Swap pane left'
    $field = Open-Palette $label
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($label)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $buttons = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button)
    $null = Wait-Until 'disabled left swap at the edge of the tab' {
        foreach ($row in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$buttons)) {
            if ($row.Current.Name.StartsWith($label) -and !$row.Current.IsEnabled) { return $true }
        }
    }
    Press-Key 27
    $null = Wait-Until 'command palette dismissed' { !(Find-Element 'Search commands') }
}

foreach ($recipe in @('original','swapped')) {
    $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--workspace',$recipe,'--cols','154','--rows','38') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
    try {
        $window = Wait-InputWindow
        $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
        $layout = Wait-Until "three visible native terminals in $recipe" {
            $panes = Read-Layout
            if (!$panes -or !$panes['palette-ssh'].active) { return }
            $left = if ($recipe -eq 'original') { $panes['palette-ssh'] } else { $panes['Lower PowerShell'] }
            $top = if ($recipe -eq 'original') { $panes['Upper PowerShell'] } else { $panes['palette-ssh'] }
            $bottom = if ($recipe -eq 'original') { $panes['Lower PowerShell'] } else { $panes['Upper PowerShell'] }
            if ($left.cols -lt $top.cols -and $left.rows -gt $bottom.rows -and $bottom.rows -gt $top.rows) { $panes }
        }
        $ssh = $layout['palette-ssh'].id
        $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','ready')
        if ($recipe -eq 'swapped') {
            $null = Measure-Positions $layout 'Lower PowerShell' 'palette-ssh' 'Upper PowerShell'
            Send-FocusedSsh $ssh 'reopened/'
            continue
        }
        $original = $layout
        $baseline = Measure-Positions $layout 'palette-ssh' 'Upper PowerShell' 'Lower PowerShell'
        $upper = $layout['Upper PowerShell'].id
        $lower = $layout['Lower PowerShell'].id
        foreach ($shell in @($upper,$lower)) {
            $null = Control @('wait-for-text','--pane',"$shell",'--timeout','15','PS ')
        }
        $upperPid = Shell-Identity $upper 'InitA' 'upper' -Initialize
        $lowerPid = Shell-Identity $lower 'InitB' 'lower' -Initialize
        if ($upperPid -eq $lowerPid) { throw 'Expected separate PowerShell processes' }
        Send-FocusedSsh $ssh 'before/'
        Assert-NoLeftNeighbor
        # Both right-hand cells touch SSH. The lower pane has the larger shared edge.
        Run-Palette 'Swap pane right'
        Wait-Swap 'Lower PowerShell' 'Upper PowerShell' 'palette-ssh'
        Send-FocusedSsh $ssh 'right/'
        Press-Key 0x77 # remapped F8 swaps the now-focused bottom-right SSH pane up
        Wait-Swap 'Lower PowerShell' 'palette-ssh' 'Upper PowerShell'
        Send-FocusedSsh $ssh 'up/'
        if ((Shell-Identity $upper 'AfterA' 'upper') -ne $upperPid -or
            (Shell-Identity $lower 'AfterB' 'lower') -ne $lowerPid) {
            throw 'Swapping lost a PowerShell process or its live variables'
        }

        Run-Palette ('Workspaces' + [char]0x2026)
        $field = Wait-Until 'workspace name field' { Find-Element 'Workspace name' ([System.Windows.Automation.ControlType]::Edit) }
        $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('swapped')
        Invoke-Button (Wait-Until 'enabled Save workspace' {
            $save = Find-Element 'Save workspace' ([System.Windows.Automation.ControlType]::Button)
            if ($save -and $save.Current.IsEnabled) { $save }
        })
        $null = Wait-Until 'workspace saved with the swapped split tree' { Find-Element 'Saved swapped' }
        $saved = (Get-Content $workspaces -Raw | ConvertFrom-Json).workspaces.swapped.tabs[0]
        if ($saved.layout.ratio -ne 0.375 -or $saved.layout.second.ratio -ne 0.3125 -or
            $saved.focused -ne $saved.layout.second.first.slot) { throw 'Saved divider ratios or focused slot changed' }
    } finally {
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
}
