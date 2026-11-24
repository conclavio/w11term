# Build with cargo build --bins --features ui-smoke before running this script.
[CmdletBinding()]
param(
    [string]$GuiPath = (Join-Path $PSScriptRoot '..\target\debug\w11term.exe'),
    [string]$CliPath
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class WorkspaceFocus {
    [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr window, int command);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr window);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint pid);
}
'@
$gui = (Resolve-Path -LiteralPath $GuiPath).Path
if (-not $CliPath) { $CliPath = Join-Path (Split-Path -Parent $gui) 'w11term-cli.exe' }
$ctl = (Resolve-Path -LiteralPath $CliPath).Path
$state = Join-Path $PSScriptRoot ('..\.local\workspace-ui-smoke\' + [guid]::NewGuid().ToString('N'))
$data = (New-Item -ItemType Directory -Path (Join-Path $state 'data') -Force).FullName
$state = Split-Path -Parent $data
$profiles = Join-Path $state 'profiles.json'
$catalog = Join-Path $data 'workspaces.json'
$encoding = [Text.UTF8Encoding]::new($false)
$variables = @('W11TERM_DATA_DIR', 'W11TERM_SETTINGS_PATH', 'W11TERM_UI_SMOKE', 'W11TERM_WORKSPACE_SMOKE')
$previous = @{}
foreach ($name in $variables) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }

function Start-WorkspaceGui([string[]]$Arguments, [string]$LogName) {
    Start-Process -FilePath $gui -ArgumentList (@('--profiles', ('"' + $profiles + '"')) + $Arguments) -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $state $LogName)
}
function Stop-WorkspaceGui([Diagnostics.Process]$Process) {
    if ($Process -and -not $Process.HasExited) {
        $null = $Process.CloseMainWindow()
        if (-not $Process.WaitForExit(5000)) { $Process.Kill(); $Process.WaitForExit() }
    }
}
function Assert-FailedLaunch([string]$Name, [string]$Expected, [string]$Mode = '--workspace') {
    $log = "$Name-$Mode.err"
    $child = Start-WorkspaceGui @($Mode, $Name) $log
    try {
        if (-not $child.WaitForExit(15000) -or $child.ExitCode -ne 1 -or
            -not (Select-String -LiteralPath (Join-Path $state $log) -SimpleMatch $Expected)) {
            throw "Expected startup failure '$Expected'. See $state\$log"
        }
    } finally { Stop-WorkspaceGui $child }
}

function Open-WorkspacePicker {
    # A native flyout can dismiss when activation moves between the fixture's
    # windows. Address the Workspaces action to the intended HWND each time.
    Press-Key 0x77 # F8 in this fixture
    Wait-Until 'workspace search box' { Find-Element 'Find workspace' }
}

$terminal = $null
$picker = $null
$first = $null
$second = $null
$duplicate = $null
try {
    $env:W11TERM_DATA_DIR = $data
    $env:W11TERM_SETTINGS_PATH = Join-Path $state 'settings.json'
    [IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","workspaces":"F8"}}', $encoding)
    Remove-Item -LiteralPath Env:W11TERM_UI_SMOKE, Env:W11TERM_WORKSPACE_SMOKE -ErrorAction SilentlyContinue
    [IO.File]::WriteAllText($profiles, '{"profiles":{}}', $encoding)
    $fixture = @'
{
  "version": 1,
  "workspaces": {
    "two-tabs": {
      "active_tab": 0,
      "tabs": [
        {"title": "Build console", "focused": 22, "layout": {"type": "split", "axis": "columns", "ratio": 0.3333333333333333,
          "first":  {"type": "leaf", "slot": 11, "source": {"kind": "demo"}},
          "second": {"type": "leaf", "slot": 22, "source": {"kind": "demo"}}}},
        {"focused": 33, "layout": {"type": "leaf", "slot": 33, "source": {"kind": "demo"}}}
      ]
    },
    "missing-profile": {
      "active_tab": 0,
      "tabs": [{"focused": 44, "layout": {"type": "leaf", "slot": 44,
        "source": {"kind": "saved_profile", "value": "absent-profile"}}}]
    }
  }
}
'@
    [IO.File]::WriteAllText($catalog, $fixture, $encoding)
    Assert-FailedLaunch 'absent-workspace' 'Unknown workspace absent-workspace'
    Assert-FailedLaunch 'missing-profile' 'Unknown workspace profile absent-profile'
    $terminal = Start-WorkspaceGui @('--workspace', 'two-tabs') 'startup.err'
    $manifest = Join-Path $env:LOCALAPPDATA "w11term\instances\$($terminal.Id).json"
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    $ready = $false
    do {
        if ($terminal.HasExited) { throw "Workspace GUI exited early. See $state\startup.err" }
        if (Test-Path -LiteralPath $manifest) {
            $reply = & $ctl --pid $terminal.Id list | ConvertFrom-Json
            if ($LASTEXITCODE -eq 0 -and $reply.ok) {
                $sessions = @($reply.result.sessions)
                $focused = @($sessions | Where-Object active)
                $other = @($sessions | Where-Object { $_.visible -and -not $_.active })
                $hidden = @($sessions | Where-Object { -not $_.visible })
                if ($sessions.Count -eq 3 -and $focused.Count -eq 1 -and $other.Count -eq 1 -and $hidden.Count -eq 1) {
                    $right = $focused[0]
                    $left = $other[0]
                    $ready = $right.visible -and $right.tab_id -eq $left.tab_id -and $right.tab_id -ne $hidden[0].tab_id -and
                        $right.rows -eq $left.rows -and $right.rows -gt 0 -and $left.cols -gt 0 -and
                        [Math]::Abs($right.cols - (2 * $left.cols)) -le 5 -and
                        @($sessions.id | Sort-Object -Unique).Count -eq 3
                    if ($ready) { break }
                }
            }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $ready) { throw "Workspace layout, focus or grid mismatch. Last list: $($reply | ConvertTo-Json -Depth 5 -Compress). See $state" }
    $ids = @($sessions.id | Sort-Object) -join ','
    $picker = Start-WorkspaceGui @('--demo') 'picker.err'
    $app = $picker
    $window = Wait-Until 'picker window' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
    foreach ($attempt in 1..2) {
        $search = Open-WorkspacePicker
        if ($attempt -eq 1) {
            Invoke-Button (Wait-Until 'broken fixture workspace' { Find-Element 'Switch to workspace missing-profile' })
            $null = Wait-Until 'failed child error in the open picker' { Find-Element 'Unknown workspace profile absent-profile' }
            $terminal.Refresh()
            $targetWindow = $terminal.MainWindowHandle
            [void][WorkspaceFocus]::ShowWindowAsync($targetWindow, 6)
            $null = Wait-Until 'workspace minimized' { [WorkspaceFocus]::IsIconic($targetWindow) }
            $search = Wait-Until 'search field after failed launch' { Find-Element 'Find workspace' }
        }
        $search.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('TWO')
        $switch = Wait-Until 'filtered switch result' {
            if (-not (Find-Element 'Switch to workspace missing-profile')) { Find-Element 'Switch to workspace two-tabs' }
        }
        Invoke-Button $switch
        $null = Wait-Until "original workspace in foreground and picker closed (switch $attempt)" {
            [uint32]$foregroundPid = 0
            [void][WorkspaceFocus]::GetWindowThreadProcessId([WorkspaceFocus]::GetForegroundWindow(), [ref]$foregroundPid)
            $foregroundPid -eq $terminal.Id -and -not [WorkspaceFocus]::IsIconic($targetWindow) -and
                -not (Find-Element 'Find workspace')
        }
        $reply = & $ctl --pid $terminal.Id list | ConvertFrom-Json
        if (-not $reply.ok -or (@($reply.result.sessions.id | Sort-Object) -join ',') -ne $ids) {
            throw 'Workspace switch replaced the original pane IDs'
        }
    }
    $duplicate = Start-WorkspaceGui @('--workspace', 'two-tabs') 'duplicate.err'
    $app = $duplicate
    $window = Wait-Until 'duplicate window' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
    Assert-FailedLaunch 'two-tabs' 'Multiple windows have this workspace open' '--switch-workspace'
    $null = Open-WorkspacePicker
    Invoke-Button (Wait-Until 'same workspace from inside its second window' { Find-Element 'Switch to workspace two-tabs' })
    $null = Wait-Until 'second window kept its own sessions and closed the picker' {
        [uint32]$currentPid = 0
        [void][WorkspaceFocus]::GetWindowThreadProcessId([WorkspaceFocus]::GetForegroundWindow(), [ref]$currentPid)
        $currentPid -eq $duplicate.Id -and -not (Find-Element 'Find workspace')
    }
    Stop-WorkspaceGui $duplicate
    Stop-WorkspaceGui $picker
    & (Join-Path $PSScriptRoot 'tab-title-ui.ps1') -ProcessId $terminal.Id -Cli $ctl
    Stop-WorkspaceGui $terminal
    # Race two launches for a closed workspace, and verify that only the winner owns panes.
    $first = Start-WorkspaceGui @('--switch-workspace', 'two-tabs') 'switch-first.err'
    $second = Start-WorkspaceGui @('--switch-workspace', 'two-tabs') 'switch-second.err'
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while (-not $first.HasExited -and -not $second.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 100
    }
    $owners = @($first, $second | Where-Object { -not $_.HasExited })
    $followers = @($first, $second | Where-Object HasExited)
    if ($owners.Count -ne 1 -or $followers.Count -ne 1 -or $followers[0].ExitCode -ne 0) {
        throw "Concurrent switches did not elect exactly one owner. See $state"
    }
    $reply = & $ctl --pid $owners[0].Id list | ConvertFrom-Json
    if (-not $reply.ok -or @($reply.result.sessions).Count -ne 3) { throw 'Closed workspace did not restore three panes' }
    Stop-WorkspaceGui $owners[0]
    $env:W11TERM_UI_SMOKE = '1'
    $env:W11TERM_WORKSPACE_SMOKE = '1'
    $terminal = Start-WorkspaceGui @('--demo') 'save-restore.err'
    if (-not $terminal.WaitForExit(60000) -or $terminal.ExitCode -ne 0 -or
        -not (Select-String -LiteralPath (Join-Path $state 'save-restore.err') -SimpleMatch 'Workspace UI smoke passed')) {
        throw "Save/restore UI smoke failed. See $state\save-restore.err"
    }
    $saved = (Get-Content -LiteralPath $catalog -Raw | ConvertFrom-Json).workspaces
    if (-not $saved.'saved-by-smoke' -or -not $saved.'two-tabs' -or -not $saved.'missing-profile') {
        throw "Saving did not preserve all three named workspaces. See $catalog"
    }
    Write-Output "Workspace smoke passed. Artifacts: $state"
} finally {
    Stop-WorkspaceGui $duplicate
    Stop-WorkspaceGui $second
    Stop-WorkspaceGui $first
    Stop-WorkspaceGui $picker
    Stop-WorkspaceGui $terminal
    foreach ($name in $variables) {
        if ($null -eq $previous[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
    }
}
