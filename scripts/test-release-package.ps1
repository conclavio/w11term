#Requires -Version 7.0
[CmdletBinding()]
param([Parameter(Mandatory)][string] $ArchivePath)

$ErrorActionPreference = 'Stop'
# list can report that the process has not yet opened its control pipe.
$PSNativeCommandUseErrorActionPreference = $false
$zip = (Resolve-Path -LiteralPath $ArchivePath).Path
$tempRoot = [IO.Path]::TrimEndingDirectorySeparator([IO.Path]::GetFullPath([IO.Path]::GetTempPath()))
$scratch = Join-Path $tempRoot ('w11term-release-test-' + [guid]::NewGuid())
$app = Join-Path $scratch 'extracted'
$state = Join-Path $scratch 'state'
$diagnostics = Join-Path $scratch 'gui.stderr'
$savedLocal = $env:LOCALAPPDATA
$savedData = $env:W11TERM_DATA_DIR
$savedSettings = $env:W11TERM_SETTINGS_PATH
$gui = $null

try {
    [void][IO.Directory]::CreateDirectory($state)
    [IO.Compression.ZipFile]::ExtractToDirectory($zip, $app)
    $env:LOCALAPPDATA = $state
    $env:W11TERM_DATA_DIR = Join-Path $state 'app'
    $env:W11TERM_SETTINGS_PATH = Join-Path $state 'settings.json'
    Set-Content -LiteralPath (Join-Path $state 'profiles.json') -Value '{"profiles":{}}' -Encoding utf8
    $gui = Start-Process -FilePath (Join-Path $app 'w11term.exe') -WorkingDirectory $state -PassThru `
        -WindowStyle Hidden -RedirectStandardError $diagnostics `
        -ArgumentList '--demo', '--cols', '72', '--rows', '26', '--profiles', 'profiles.json', '--smoke-seconds', '15'
    $cli = Join-Path $app 'w11term-cli.exe'
    $startup = [Diagnostics.Stopwatch]::StartNew()
    do {
        if ($gui.HasExited) { throw "GUI exited before publishing its demo pane (exit $($gui.ExitCode))." }
        $json = & $cli --pid $gui.Id list
        if ($LASTEXITCODE -eq 0) {
            $listing = $json | ConvertFrom-Json
            $pane = $listing.result.sessions | Select-Object -First 1
            if ($listing.ok -and $pane.id) { break }
        }
        Start-Sleep -Milliseconds 150
    } while ($startup.Elapsed.TotalSeconds -lt 30)
    if (-not $pane.id) { throw "The packaged CLI could not find the packaged GUI's demo pane." }

    $reply = & $cli --pid $gui.Id wait-for-text --pane $pane.id --timeout 5 'lab>' | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or -not $reply.ok -or -not $reply.result.matched) {
        throw "The packaged terminal did not report its ready prompt: $($reply.error)"
    }
    $frame = Join-Path $state 'frame.png'
    $reply = & $cli --pid $gui.Id screenshot --pane $pane.id $frame | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or -not $reply.ok -or $reply.result.width -le 0 -or $reply.result.height -le 0) {
        throw "Could not render the packaged demo: $($reply.error)"
    }
    if ([Convert]::ToHexString([IO.File]::ReadAllBytes($frame)[0..7]) -ne '89504E470D0A1A0A') {
        throw 'The packaged renderer returned an invalid PNG signature.'
    }
    if (-not $gui.WaitForExit(35000) -or $gui.ExitCode -ne 0) {
        throw 'The packaged GUI did not exit cleanly after its smoke timer.'
    }
    Write-Output "Release ZIP verified: CLI read the demo prompt, rendered a PNG, and the GUI exited successfully."
} catch {
    foreach ($log in $diagnostics, (Join-Path $env:W11TERM_DATA_DIR 'startup.log' -ErrorAction SilentlyContinue)) {
        if ($log -and (Test-Path -LiteralPath $log)) { Get-Content -LiteralPath $log -Tail 12 | Write-Warning }
    }
    throw
} finally {
    $env:LOCALAPPDATA = $savedLocal
    $env:W11TERM_DATA_DIR = $savedData
    $env:W11TERM_SETTINGS_PATH = $savedSettings
    if ($gui) {
        if (-not $gui.HasExited) { $gui.Kill(); $gui.WaitForExit() }
        $gui.Dispose()
    }
    if ([IO.Directory]::Exists($scratch)) {
        $actual = (Resolve-Path -LiteralPath $scratch).Path
        if ([IO.Path]::GetDirectoryName($actual) -ne $tempRoot -or
            [IO.Path]::GetFileName($actual) -notmatch '^w11term-release-test-[0-9a-f-]{36}$') {
            throw "Unexpected smoke test directory: $actual"
        }
        Remove-Item -LiteralPath $actual -Recurse -Force
    }
}
