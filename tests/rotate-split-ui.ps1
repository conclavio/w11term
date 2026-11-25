[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","rotate_split":"F8","zoom_pane":"F6"}}')
$profiles = Join-Path $State 'profiles.json'
$recipes = Join-Path $env:W11TERM_DATA_DIR 'workspaces.json'
$rotate = 'Rotate split    F8'
$button = [System.Windows.Automation.ControlType]::Button

function Three-Panes {
    $sessions = @((Control @('list')).sessions)
    if ($sessions.Count -ne 3 -or @($sessions.tab_id | Select-Object -Unique).Count -ne 1) { return }
    $panes = @{}
    foreach ($s in $sessions) {
        if (!$s.visible -or $s.connection_generation -lt 1) { return }
        $panes[$s.label] = $s
    }
    if ($panes['palette-ssh'].active -and $panes['Inner PowerShell'] -and $panes['Outer PowerShell']) { $panes }
}
function Same-Grid($A, $B) { $A.rows -eq $B.rows -and $A.cols -eq $B.cols }
function Wait-Geometry([bool]$Rotated) {
    $null = Wait-Until "stable nested grid, rotated=$Rotated" {
        $now = Three-Panes
        if (!$now) { return }
        foreach ($name in $baseline.Keys) {
            $old = $baseline[$name]
            $p = $now[$name]
            if ($old.id -ne $p.id -or $old.connection_generation -ne $p.connection_generation -or
                $old.window_id -ne $p.window_id -or $old.tab_id -ne $p.tab_id) { return }
        }
        $a = $now['palette-ssh']
        $b = $now['Inner PowerShell']
        $outer = $now['Outer PowerShell']
        if (!(Same-Grid $outer $baseline['Outer PowerShell'])) { return }
        if (!$Rotated) { return (Same-Grid $a $baseline['palette-ssh']) -and (Same-Grid $b $baseline['Inner PowerShell']) }
        # Only the right-hand branch changes from stacked rows into columns.
        # The SSH child is the smaller, first side of the asymmetric divider.
        $fraction = $a.cols / ($a.cols + $b.cols)
        $a.rows -eq $outer.rows -and $b.rows -eq $outer.rows -and
            $a.cols -lt $b.cols -and [Math]::Abs($fraction - 0.3125) -lt 0.07
    }
}
function Stamp([int]$Pane, [string]$Marker, [switch]$Init) {
    $cmd = if ($Init) { '$global:rotatePid=$PID; ' } else { '' }
    $cmd += "'" + $Marker + "=' + " + '$PID + "/" + $rotatePid' + [char]13
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($cmd)))
    $expression = "$Marker=(\d+)/(\d+)"
    $reply = Control @('wait-for-text','--pane',"$Pane",'--timeout','15','--regex',$expression)
    $m = [regex]::Match($reply.text, $expression)
    if (!$m.Success -or $m.Groups[1].Value -ne $m.Groups[2].Value) { throw "PowerShell session state changed in pane $Pane" }
    $m.Groups[1].Value
}
function Type-Ssh([string]$Marker) {
    Wait-Focus $ssh
    foreach ($c in $Marker.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15',$Marker)
}
function Expect-Disabled {
    $field = Open-Palette 'Rotate split'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Rotate split')
    $null = Wait-Until 'rotation disabled for this pane' {
        $entry = Find-Element $rotate $button
        $entry -and !$entry.Current.IsEnabled
    }
    Press-Key 27
    $null = Wait-Until 'palette dismissed' { !(Find-Element 'Search commands') }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--workspace','original','--cols','154','--rows','38') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $baseline = Wait-Until 'initial asymmetric layout' {
        $p = Three-Panes
        if ($p -and $p['palette-ssh'].cols -eq $p['Inner PowerShell'].cols -and
            $p['palette-ssh'].rows -lt $p['Inner PowerShell'].rows -and $p['Outer PowerShell'].rows -gt $p['Inner PowerShell'].rows) { $p }
    }
    $ssh = $baseline['palette-ssh'].id
    $inner = $baseline['Inner PowerShell'].id
    $outer = $baseline['Outer PowerShell'].id
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','ready')
    foreach ($id in @($inner,$outer)) { $null = Control @('wait-for-text','--pane',"$id",'--timeout','15','PS ') }
    $innerPid = Stamp $inner initialInner -Init
    $outerPid = Stamp $outer initialOuter -Init
    if ($innerPid -eq $outerPid) { throw 'Expected two independent PowerShell processes' }

    Type-Ssh 'before/'
    Press-Key 0x75
    $null = Wait-Until 'only focused SSH visible when zoomed' { @((Control @('list')).sessions | Where-Object visible).Count -eq 1 }
    Expect-Disabled
    Press-Key 0x75
    Wait-Geometry $false
    Run-Palette $rotate
    Wait-Geometry $true
    Type-Ssh 'rotated/'
    Run-Palette ('Workspaces' + [char]0x2026)
    $nameField = Wait-Until 'workspace name' { Find-Element 'Workspace name' ([System.Windows.Automation.ControlType]::Edit) }
    $nameField.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('rotated')
    Invoke-Button (Wait-Until 'Save workspace' {
        $save = Find-Element 'Save workspace' $button
        if ($save -and $save.Current.IsEnabled) { $save }
    })
    $null = Wait-Until 'saved rotated workspace' { Find-Element 'Saved rotated' }
    $tab = ([IO.File]::ReadAllText($recipes) | ConvertFrom-Json).workspaces.rotated.tabs[0]
    $root = $tab.layout
    $branch = $root.second
    if ($root.axis -cne 'columns' -or $root.ratio -ne 0.375 -or $branch.axis -cne 'columns' -or $branch.ratio -ne 0.3125 -or
        $root.first.source.value.name -cne 'Outer PowerShell' -or $branch.first.source.value -cne 'palette-ssh' -or
        $branch.second.source.value.name -cne 'Inner PowerShell' -or $tab.focused -ne $branch.first.slot) {
        throw "Unexpected persisted tree: $($tab | ConvertTo-Json -Depth 12 -Compress)"
    }
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $ssh
    Press-Key 0x77
    Wait-Geometry $false
    Type-Ssh 'restored/'
    if ((Stamp $inner finalInner) -ne $innerPid -or (Stamp $outer finalOuter) -ne $outerPid) { throw 'Rotation restarted PowerShell' }
    $solo = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $solo
    Expect-Disabled
    $menu = [System.Windows.Automation.ControlType]::MenuItem
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' $button })
    $panesMenu = Wait-Until 'Panes submenu' { Find-Element 'Panes' $menu }
    $panesMenu.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $null = Wait-Until 'solo pane has disabled native Rotate split menu item and assigned chord' {
        $item = Find-Element $rotate $menu
        $item -and !$item.Current.IsEnabled
    }
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
