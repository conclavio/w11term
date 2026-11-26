[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'

function Measure-Grid([int]$Pane) { Control @('inspect','--pane',"$Pane") }
function Same-Grid($First, $Second) {
    $First.cols -eq $Second.cols -and $First.rows -eq $Second.rows -and
        $First.pixel_width -eq $Second.pixel_width -and $First.pixel_height -eq $Second.pixel_height
}
function Resize-Grid([int]$Pane, [int]$Cols, [int]$Rows) {
    $null = Control @('resize','--pane',"$Pane",'--cols',"$Cols",'--rows',"$Rows")
    $measured = Measure-Grid $Pane
    if ($measured.cols -ne $Cols -or $measured.rows -ne $Rows) {
        throw "Resize to ${Cols}x${Rows} acknowledged live grid $($measured.cols)x$($measured.rows)"
    }
    return $measured
}
function Check-Png([int]$Pane, [string]$Stage, $Grid) {
    $path = Join-Path $State "$Stage.png"
    $png = Control @('screenshot','--pane',"$Pane",$path)
    if ($png.width -ne $Grid.pixel_width -or $png.height -ne $Grid.pixel_height) {
        throw "Screenshot '$Stage' was $($png.width)x$($png.height); the live text area was $($Grid.pixel_width)x$($Grid.pixel_height)"
    }
}

$launch = @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh', '--cols', '125', '--rows', '32')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control manifest' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    $original = Wait-Until 'the measured split grids' {
        $ssh = Measure-Grid $remote
        $powershell = Measure-Grid $local
        if ($ssh.pixel_width -gt 0 -and $powershell.pixel_width -gt 0 -and
            $ssh.cols -gt 1 -and $powershell.cols -gt 1 -and
            [Math]::Abs($ssh.pixel_width - $powershell.pixel_width) -lt 30) {
            @{ ssh = $ssh; local = $powershell }
        }
    }
    Check-Png $remote 'before' $original.ssh
    Invoke-Button (Get-NativeMenuAction 'Pane layout' 'Increase pane text size' -Submenu 'Pane text size')
    Run-Palette 'Increase pane text size'
    $larger = Wait-Until 'the SSH split has larger cells on the same window' {
        $grid = Measure-Grid $remote
        if ($grid.cols -lt $original.ssh.cols -and
            ($grid.pixel_width / $grid.cols) -gt ($original.ssh.pixel_width / $original.ssh.cols)) { $grid }
    }
    if (!(Same-Grid (Measure-Grid $local) $original.local)) { throw 'Pane text size resized the PowerShell sibling' }
    Check-Png $remote 'larger-ssh' $larger
    Check-Png $local 'unchanged-powershell' $original.local
    Wait-Focus $remote

    Run-Palette 'Decrease font size    Ctrl+-'
    $smallerWindowDefault = Wait-Until 'the window font changes only the inherited split grid' {
        $grid = Measure-Grid $local
        if ($grid.cols -gt $original.local.cols) { $grid }
    }
    if (!(Same-Grid (Measure-Grid $remote) $larger)) { throw 'The window default changed the explicit SSH text size' }
    Run-Palette 'Reset pane text size'
    $null = Wait-Until 'reset adopts the current window font' {
        $ssh = Measure-Grid $remote
        ($ssh.pixel_width / $ssh.cols) -eq ($smallerWindowDefault.pixel_width / $smallerWindowDefault.cols)
    }
    Run-Palette 'Reset font size    Ctrl+0'
    $null = Wait-Until 'both original split grids return' {
        (Same-Grid (Measure-Grid $remote) $original.ssh) -and
            (Same-Grid (Measure-Grid $local) $original.local)
    }
    Wait-Focus $remote

    $null = Resize-Grid $remote ($original.ssh.cols - 2) $original.ssh.rows
    Run-Palette 'Increase pane text size'
    $beforeMove = Wait-Until 'larger SSH cells before moving the pane' {
        $grid = Measure-Grid $remote
        if ($grid.pixel_height / $grid.rows -gt $original.ssh.pixel_height / $original.ssh.rows) { $grid }
    }
    Run-Palette 'Move pane to new tab'
    $moved = Wait-Until 'the existing SSH pane moved to its own tab at the same text size' {
        $sessions = (Control @('list')).sessions
        $sshPane = $sessions | Where-Object id -eq $remote
        $localPane = $sessions | Where-Object id -eq $local
        $grid = Measure-Grid $remote
        if ($sshPane.active -and $localPane -and $sshPane.tab_id -ne $localPane.tab_id -and
            $grid.cols -gt $beforeMove.cols -and
            $grid.pixel_height / $grid.rows -eq $beforeMove.pixel_height / $beforeMove.rows) { $grid }
    }
    Check-Png $remote 'moved-ssh' $moved
    $resized = Resize-Grid $remote 72 20
    if ($resized.pixel_height / $resized.rows -ne $moved.pixel_height / $moved.rows) {
        throw 'Resizing the window changed the pane-specific font'
    }
    Check-Png $remote 'resized-ssh' $resized

    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $local
    $localVisible = Resize-Grid $local 80 22
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    $null = Resize-Grid $local ($localVisible.cols - 3) $localVisible.rows
    Wait-Focus $remote
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $local
    try {
        $null = Wait-Until 'reactivated PowerShell adopts its unchanged viewport after a background resize' {
            Same-Grid (Measure-Grid $local) $localVisible
        }
    } catch {
        $actual = Measure-Grid $local
        throw "$_. Expected $($localVisible.cols)x$($localVisible.rows) cells, measured $($actual.cols)x$($actual.rows)"
    }
    if (@((Control @('list')).sessions).Count -ne 2) { throw 'Adjusting text size replaced a live pane' }
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    Close-FixtureApp $app
}
