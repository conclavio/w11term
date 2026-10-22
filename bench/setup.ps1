[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$tools = Join-Path $project '.local\tools'
New-Item -ItemType Directory -Path $tools -Force | Out-Null

function Install-Archive($Url, $Archive, $Checksum, $Destination, $ExpectedFile) {
    if (-not (Test-Path -LiteralPath $Archive)) { Invoke-WebRequest $Url -OutFile $Archive }
    if ((Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash -ne $Checksum) {
        throw "Checksum mismatch: $Archive"
    }
    if (-not (Test-Path -LiteralPath $ExpectedFile)) {
        Expand-Archive -LiteralPath $Archive -DestinationPath $Destination -Force
    }
}

# Typometer's checksum pins the original HTTPS release archive retrieved for this setup.
Install-Archive 'https://github.com/pavelfatin/typometer/releases/download/v1.0.1/typometer-1.0.1-bin.zip' `
    (Join-Path $tools 'typometer-1.0.1-bin.zip') `
    '8afc176ef7b5b94021640be3089c7f7adbe94f0bdc5b15b84308e765fcb68d35' `
    (Join-Path $tools 'typometer') `
    (Join-Path $tools 'typometer\typometer-1.0.1\typometer-1.0.1.jar')

# Java checksum published in the Adoptium API metadata. No PATH/registry changes.
Install-Archive 'https://github.com/adoptium/temurin8-binaries/releases/download/jdk8u504-b01/OpenJDK8U-jre_x64_windows_hotspot_8u504b01.zip' `
    (Join-Path $tools 'temurin8-jre.zip') `
    '82e2cdc6693737c5998445b31f69668fa0da77c7705121053f6508ac84961123' `
    (Join-Path $tools 'java') `
    (Join-Path $tools 'java\jdk8u504-b01-jre\bin\javaw.exe')

Push-Location $project
try {
    cargo build --release --bin w11term --example echo_server --example ssh_probe
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
} finally { Pop-Location }
& (Join-Path $PSScriptRoot 'record-environment.ps1')
Write-Host 'Benchmark tools ready. Run .\bench\start-server.ps1, then .\bench\launch.ps1 typometer'
