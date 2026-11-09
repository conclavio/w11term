# Keep the executable beside the runtime files staged by build.rs.
$binary = Join-Path $PSScriptRoot 'target\release\w11term.exe'
if (-not (Test-Path -LiteralPath $binary)) {
    throw 'Build w11term first with cargo build --release, then run .\launch.ps1.'
}
& $binary @args | Out-Host
exit $LASTEXITCODE
