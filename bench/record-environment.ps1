$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$results = Join-Path $project '.local\benchmark\results'
New-Item -ItemType Directory -Path $results -Force | Out-Null
$wezterm = Get-Command wezterm.exe -ErrorAction SilentlyContinue
$inventory = [ordered]@{
    recordedUtc = [DateTime]::UtcNow.ToString('o')
    os = Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber
    cpu = @(Get-CimInstance Win32_Processor | Select-Object Name)
    gpu = @(Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion)
    wezterm = if ($wezterm) { & $wezterm.Source --version } else { $null }
    windowsTerminal = @(Get-AppxPackage *WindowsTerminal* | Select-Object Name, Version)
    typometer = '1.0.1'
    java = 'Temurin 8u504-b01 x64 JRE'
    binaries = @(Get-Item (Join-Path $project 'target\release\w11term.exe'),
        (Join-Path $project 'target\release\examples\echo_server.exe'),
        (Join-Path $project 'Cargo.lock') | Get-FileHash -Algorithm SHA256 | Select-Object Path, Hash)
    monitorRefreshHz = $null
    monitorScalingPercent = $null
    note = 'Record monitor/refresh/scaling, actual terminal grid sizes, power mode, and Typometer parameters with each measurement. Re-record after rebuilding.'
}
$path = Join-Path $results ('environment-' + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss') + '.json')
$inventory | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $path -Encoding UTF8
Write-Host "Environment recorded: $path"
