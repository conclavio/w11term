# Build with cargo build --bins --features ui-smoke before running this script.
[CmdletBinding()]
param(
    [string]$GuiPath = (Join-Path $PSScriptRoot '..\target\debug\w11term.exe'),
    [string]$CliPath
)
$ErrorActionPreference = 'Stop'
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
function Assert-FailedLaunch([string]$Name, [string]$Expected) {
    $log = "$Name.err"
    $child = Start-WorkspaceGui @('--workspace', $Name) $log
    try {
        if (-not $child.WaitForExit(15000) -or $child.ExitCode -ne 1 -or
            -not (Select-String -LiteralPath (Join-Path $state $log) -SimpleMatch $Expected)) {
            throw "Expected startup failure '$Expected'. See $state\$log"
        }
    } finally { Stop-WorkspaceGui $child }
}

$terminal = $null
try {
    $env:W11TERM_DATA_DIR = $data
    $env:W11TERM_SETTINGS_PATH = Join-Path $state 'settings.json'
    [Environment]::SetEnvironmentVariable('W11TERM_UI_SMOKE', $null, 'Process')
    [Environment]::SetEnvironmentVariable('W11TERM_WORKSPACE_SMOKE', $null, 'Process')
    [IO.File]::WriteAllText($profiles, '{"profiles":{}}', $encoding)
    $fixture = @'
{
  "version": 1,
  "workspaces": {
    "two-tabs": {
      "active_tab": 0,
      "tabs": [
        {"focused": 22, "layout": {"type": "split", "axis": "columns", "ratio": 0.3333333333333333,
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
    Stop-WorkspaceGui $terminal
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
    Stop-WorkspaceGui $terminal
    foreach ($name in $variables) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
}
