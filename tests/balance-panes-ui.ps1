[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","zoom_pane":"F6"}}')
$profiles = Join-Path $State 'profiles.json'
$balance = 'Balance panes'
$button = [System.Windows.Automation.ControlType]::Button

function Disabled-Balance {
    $field = Open-Palette $balance
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($balance)
    $null = Wait-Until 'disabled Balance panes command' {
        $item = Find-Element $balance $button
        $item -and !$item.Current.IsEnabled
    }
    Press-Key 27
    $null = Wait-Until 'command palette dismissed' { !(Find-Element 'Search commands') }
}
function Send-Bytes([int]$Pane, [string]$Text) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Text))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$encoded)
}
function Shell-Proof([int]$Pane, [string]$Tag, [switch]$Init) {
    $cmd = if ($Init) { '$global:balancePid = $PID; ' } else { '' }
    $cmd += "'" + $Tag + "=' + " + '$PID + "/" + $global:balancePid' + [char]13
    Send-Bytes $Pane $cmd
    $pattern = "$Tag=(\d+)/(\d+)"
    $frame = Control @('wait-for-text','--pane',"$Pane",'--timeout','15','--regex',$pattern)
    $m = [regex]::Match($frame.text, $pattern)
    if (!$m.Success -or $m.Groups[1].Value -cne $m.Groups[2].Value) { throw "PowerShell state lost for $Tag" }
    return $m.Groups[1].Value
}
function Three-Panes {
    $panes = @((Control @('list')).sessions | Sort-Object id)
    if ($panes.Count -eq 3 -and @($panes | Where-Object { !$_.visible -or $_.connection_generation -lt 1 }).Count -eq 0 -and
        @($panes.tab_id | Select-Object -Unique).Count -eq 1 -and $panes[2].active) { $panes }
}
function New-ShellSplit([int[]]$Existing) {
    Run-Palette 'Split right with Palette PowerShell'
    $pane = Wait-Until 'new active local shell in the original split tab' {
        $p = (Control @('list')).sessions | Where-Object { $_.id -notin $Existing -and $_.active -and $_.label -ceq 'Palette PowerShell' }
        if ($p) { $p.id }
    }
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','PS ')
    return $pane
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','177','--rows','35') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $ssh = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','ready')
    Disabled-Balance
    $mid = New-ShellSplit @($ssh)
    $right = New-ShellSplit @($ssh,$mid)
    $before = @(Wait-Until 'three uneven columns with stable PTY measurements' {
        $p = @(Three-Panes)
        if ($p.Count -eq 3 -and @($p.rows | Select-Object -Unique).Count -eq 1 -and
            $p[0].cols -gt $p[1].cols + 10 -and [Math]::Abs($p[1].cols - $p[2].cols) -le 1) { $p }
    })
    $middlePid = Shell-Proof $mid midBefore -Init
    $rightPid = Shell-Proof $right rightBefore -Init
    if ($middlePid -eq $rightPid) { throw 'Expected distinct PowerShell processes' }

    Press-Key 0x75 # F6 toggles zoom in this fixture.
    $null = Wait-Until 'only the right-hand PowerShell is visible while zoomed' {
        $p = @((Control @('list')).sessions | Where-Object visible)
        $p.Count -eq 1 -and $p[0].id -eq $right
    }
    Disabled-Balance
    Press-Key 0x75
    $null = Wait-Until 'uneven columns restored after zoom' {
        $p = @(Three-Panes)
        $p.Count -eq 3 -and $p[0].cols -eq $before[0].cols -and $p[1].cols -eq $before[1].cols -and $p[2].cols -eq $before[2].cols
    }

    Run-Palette $balance
    $null = Wait-Until 'three live terminal grids balanced within one column' {
        $p = @(Three-Panes)
        if ($p.Count -ne 3) { return $false }
        for ($i = 0; $i -lt 3; $i++) {
            $old = $before[$i]
            $new = $p[$i]
            if ($new.id -ne $old.id -or $new.tab_id -ne $old.tab_id -or $new.window_id -ne $old.window_id -or
                $new.active -ne $old.active -or $new.connection_generation -ne $old.connection_generation -or $new.rows -ne $old.rows) { return $false }
        }
        $widths = $p.cols | Measure-Object -Minimum -Maximum -Sum
        $oldTotal = ($before.cols | Measure-Object -Sum).Sum
        $widths.Maximum - $widths.Minimum -le 1 -and [Math]::Abs($widths.Sum - $oldTotal) -le 2
    }
    if ((Shell-Proof $mid midAfter) -cne $middlePid -or (Shell-Proof $right rightAfter) -cne $rightPid) {
        throw 'Balancing restarted a PowerShell process'
    }
    Send-Bytes $ssh 'balance-ssh-live'
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','balance-ssh-live')
    Wait-Focus $right
} catch {
    $observed = try { (Control @('list')).sessions | Select-Object id,active,visible,cols,rows,tab_id,connection_generation | ConvertTo-Json -Compress } catch {}
    throw "$_ at $($_.ScriptStackTrace). Pane grids: $observed"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
