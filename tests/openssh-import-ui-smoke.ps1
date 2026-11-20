[CmdletBinding()]
param([string]$GuiPath = (Join-Path $PSScriptRoot '..\target\debug\w11term.exe'))
$ErrorActionPreference = 'Stop'
$gui = (Resolve-Path -LiteralPath $GuiPath).Path
$state = (New-Item -ItemType Directory -Force -Path (Join-Path $PSScriptRoot ('..\.local\openssh-import-smoke\' + [guid]::NewGuid().ToString('N')))).FullName
$config = Join-Path $state 'config'
$profiles = Join-Path $state 'profiles.json'
$data = (New-Item -ItemType Directory -Path (Join-Path $state 'data')).FullName
$variables = @('W11TERM_DATA_DIR','W11TERM_SETTINGS_PATH','W11TERM_UI_SMOKE','W11TERM_IMPORT_SMOKE')
$previous = @{}
foreach ($name in $variables) { $previous[$name] = [Environment]::GetEnvironmentVariable($name,'Process') }
$terminal = $null
try {
    $fixture = @'
Host via
    HostName target.test
    User pilot
    Port 2222
    ProxyJump gateway
    ServerAliveInterval 15
Host gateway
    HostName gateway.test
    User jump
Host blocked
    ProxyCommand example-command %h
'@
    $utf8 = [Text.UTF8Encoding]::new($false)
    [IO.File]::WriteAllText($config,$fixture,$utf8)
    [IO.File]::WriteAllText($profiles,'{"profiles":{}}',$utf8)
    $env:W11TERM_DATA_DIR = $data
    $env:W11TERM_SETTINGS_PATH = Join-Path $state 'settings.json'
    $env:W11TERM_UI_SMOKE = '1'
    $env:W11TERM_IMPORT_SMOKE = $config
    $log = Join-Path $state 'stderr.log'
    $terminal = Start-Process -FilePath $gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError $log
    if (-not $terminal.WaitForExit(30000) -or $terminal.ExitCode -ne 0 -or -not (Select-String -LiteralPath $log -SimpleMatch 'OpenSSH import UI smoke passed')) {
        throw "OpenSSH import smoke failed. See $log"
    }
    $saved = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
    if ($saved.psobject.Properties.Name.Count -ne 1 -or $saved.via.proxy_jump[0].host -ne 'gateway.test') { throw "Unexpected saved profiles. See $profiles" }
    Write-Output "OpenSSH import native smoke passed. Artifacts in $state"
} finally {
    if ($terminal -and -not $terminal.HasExited) { $terminal.Kill(); $terminal.WaitForExit() }
    foreach ($name in $variables) { [Environment]::SetEnvironmentVariable($name,$previous[$name],'Process') }
}
