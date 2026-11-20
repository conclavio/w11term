[CmdletBinding()]
param([string]$GuiPath = (Join-Path $PSScriptRoot '..\target\debug\w11term.exe'))
$ErrorActionPreference='Stop'
$gui=(Resolve-Path -LiteralPath $GuiPath).Path
$state=(New-Item -ItemType Directory -Force -Path (Join-Path $PSScriptRoot ('..\.local\ime-smoke\'+[guid]::NewGuid().ToString('N')))).FullName
$profiles=Join-Path $state 'profiles.json'
[IO.File]::WriteAllText($profiles,'{"profiles":{}}')
$variables=@('W11TERM_DATA_DIR','W11TERM_SETTINGS_PATH','W11TERM_UI_SMOKE','W11TERM_IME_SMOKE')
$previous=@{}
foreach($name in $variables){$previous[$name]=[Environment]::GetEnvironmentVariable($name,'Process')}
$process=$null
try {
    $env:W11TERM_DATA_DIR=$state
    $env:W11TERM_SETTINGS_PATH=Join-Path $state 'settings.json'
    $env:W11TERM_IME_SMOKE='1'
    $env:W11TERM_UI_SMOKE='1'
    $log=Join-Path $state 'stderr.log'
    $process=Start-Process -FilePath $gui -ArgumentList @('--demo','--profiles',('"'+$profiles+'"')) -PassThru -WindowStyle Hidden -RedirectStandardError $log
    if (-not $process.WaitForExit(30000) -or $process.ExitCode -ne 0 -or -not (Select-String -LiteralPath $log -SimpleMatch 'IME native smoke passed')) {throw "IME smoke failed. See $log"}
    Write-Output "IME native smoke passed. Artifacts in $state"
} finally {
    if ($process -and -not $process.HasExited){$process.Kill();$process.WaitForExit()}
    foreach($name in $variables){[Environment]::SetEnvironmentVariable($name,$previous[$name],'Process')}
}
