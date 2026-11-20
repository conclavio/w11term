[CmdletBinding()]
param([string]$GuiPath = (Join-Path $PSScriptRoot '..\target\debug\w11term.exe'))
$ErrorActionPreference = 'Stop'
$gui = (Resolve-Path -LiteralPath $GuiPath).Path
$state = Join-Path $PSScriptRoot ('..\.local\split-ui-smoke\' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $state -Force | Out-Null
$state = (Resolve-Path -LiteralPath $state).Path
$variables = @('W11TERM_UI_SMOKE', 'W11TERM_SPLIT_SMOKE', 'W11TERM_DATA_DIR', 'W11TERM_SETTINGS_PATH')
$previous = @{}
foreach ($name in $variables) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
try {
    $env:W11TERM_UI_SMOKE = '1'
    $env:W11TERM_SPLIT_SMOKE = '1'
    $env:W11TERM_DATA_DIR = Join-Path $state 'data'
    $env:W11TERM_SETTINGS_PATH = Join-Path $state 'settings.json'
    $config = Join-Path $state 'profiles.json'
    '{"profiles":{}}' | Set-Content -LiteralPath $config -Encoding utf8
    $log = Join-Path $state 'stderr.log'
    $process = Start-Process -FilePath $gui -ArgumentList @('--demo', '--profiles', ('"' + $config + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError $log
    if (-not $process.WaitForExit(60000)) {
        Stop-Process -Id $process.Id
        throw "Split smoke timed out. See $log"
    }
    if ($process.ExitCode -ne 0 -or -not (Select-String -LiteralPath $log -SimpleMatch 'Split pane smoke passed')) {
        throw "Split smoke failed. See $log"
    }
    Write-Output "Split pane smoke passed. Artifacts: $state"
} finally {
    foreach ($name in $variables) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
}
