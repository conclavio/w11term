#Requires -Version 7.0
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string] $BinaryDirectory,
    [Parameter(Mandatory)][string] $ArchivePath
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$binaries = (Resolve-Path -LiteralPath $BinaryDirectory).Path
$outputFile = $PSCmdlet.GetUnresolvedProviderPathFromPSPath($ArchivePath)
$outputDir = [IO.Path]::GetDirectoryName($outputFile)
[void][IO.Directory]::CreateDirectory($outputDir)
if (Test-Path -LiteralPath $outputFile) { throw "Archive already exists: $outputFile" }

$metadata = & cargo metadata --locked --filter-platform x86_64-pc-windows-msvc --format-version 1 --manifest-path (Join-Path $repo 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'Unable to read the locked Cargo package metadata.' }
$packages = ($metadata | ConvertFrom-Json).packages
$reactor = $packages | Where-Object name -EQ windows-reactor-setup
# These are the two SDK versions staged by build.rs through this exact setup crate.
if ($reactor.version -ne '0.100.0') { throw 'Review the runtime package versions for the new windows-reactor-setup.' }
$sdkCache = Join-Path $env:LOCALAPPDATA 'windows-reactor-setup/temp'
$appSdk = 'Microsoft.WindowsAppSDK.Runtime.2.4.0'
$webView = 'Microsoft.Web.WebView2.1.0.4078.44'
$msix = Join-Path $sdkCache 'Microsoft.WindowsAppSDK.Runtime-2.4.0/MSIX/win10-x64/Microsoft.WindowsAppRuntime.2.msix'
$runtimeRoots = @(Get-Content -LiteralPath (Join-Path (Split-Path $reactor.manifest_path) 'assets/runtime.txt') | Where-Object { $_.Trim() })

# A partial archive never acquires the public filename. Archive entries come from
# manifests and named inputs, so a stale build or local profiles cannot add files.
$partial = Join-Path $outputDir ([guid]::NewGuid().ToString() + '.tmp')
function Add-File([string] $path, [string] $name) {
    [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $path, $name.Replace('\', '/'))
}
try {
    $archive = [IO.Compression.ZipFile]::Open($partial, 'Create')
    try {
        foreach ($name in 'w11term.exe', 'w11term-cli.exe', 'Microsoft.Web.WebView2.Core.dll') {
            Add-File (Join-Path $binaries $name) $name
        }
        $runtime = [IO.Compression.ZipFile]::OpenRead($msix)
        try {
            $seen = @{}
            foreach ($entry in $runtime.Entries) {
                $root = $entry.FullName.Split('/')[0]
                if ($entry.Name -and $root -in $runtimeRoots) {
                    Add-File (Join-Path $binaries $entry.FullName) $entry.FullName
                    $seen[$root] = $true
                }
            }
            foreach ($root in $runtimeRoots) {
                if (-not $seen.ContainsKey($root)) { throw "Runtime manifest entry missing from SDK: $root" }
            }
        } finally { $runtime.Dispose() }

        foreach ($name in 'LICENSE', 'NOTICE', 'README.md', 'CONTROL.md') {
            Add-File (Join-Path $repo $name) $name
        }
        foreach ($name in 'wezterm-term', 'wezterm-escape-parser', 'vtparse') {
            $license = "vendor/$name/LICENSE.md"
            Add-File (Join-Path $repo $license) $license
        }
        foreach ($id in $appSdk, $webView) {
            $package = [IO.Compression.ZipFile]::OpenRead((Join-Path $sdkCache "$id.nupkg"))
            try {
                foreach ($name in 'LICENSE.txt', 'NOTICE.txt') {
                    $entry = $package.Entries | Where-Object FullName -EQ $name
                    if (-not $entry) { throw "$id lacks $name" }
                    $inputStream = $entry.Open()
                    try {
                        $outputStream = $archive.CreateEntry("licenses/$id/$name").Open()
                        try { $inputStream.CopyTo($outputStream) } finally { $outputStream.Dispose() }
                    } finally { $inputStream.Dispose() }
                }
            } finally { $package.Dispose() }
        }

        # Preserve notices from the resolved crates, including bundled native code.
        # The inventory retains the declared SPDX terms where a crate omits a text.
        $dependencyList = $archive.CreateEntry('licenses/cargo-packages.json')
        $writer = [IO.StreamWriter]::new($dependencyList.Open())
        try {
            $writer.WriteLine(($packages | Select-Object name, version, license, authors, repository | ConvertTo-Json -Depth 4))
        } finally { $writer.Dispose() }
        foreach ($pkg in $packages | Where-Object name -NE w11term) {
            $dir = Split-Path $pkg.manifest_path
            $files = Get-ChildItem -LiteralPath $dir -File -Recurse | Where-Object {
                $_.Name -match '^(LICEN[CS]E|COPYING|UNLICENSE|NOTICE|COPYRIGHT|THIRD[-_ ]PARTY[-_ ]NOTICES?)([-._ ]|$)' -or
                $_.Directory.Name -in 'licenses', 'licences'
            }
            foreach ($file in $files) {
                $relative = [IO.Path]::GetRelativePath($dir, $file.FullName)
                Add-File $file.FullName "licenses/cargo/$($pkg.name)-$($pkg.version)/$relative"
            }
        }
    } finally { $archive.Dispose() }
    [IO.File]::Move($partial, $outputFile)
} finally {
    if ([IO.File]::Exists($partial)) { [IO.File]::Delete($partial) }
}
Write-Output $outputFile
