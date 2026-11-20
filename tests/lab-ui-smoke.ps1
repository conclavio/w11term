# Requires PowerShell 7 and cargo build --features ui-smoke --bins.
[CmdletBinding()]
param([string]$GuiPath = (Join-Path $PSScriptRoot '..\target\debug\w11term.exe'))

$ErrorActionPreference = 'Stop'
$gui = (Resolve-Path -LiteralPath $GuiPath).Path
$outputDir = Join-Path $PSScriptRoot ('..\.local\lab-ui-smoke\' + [guid]::NewGuid().ToString('N'))

function Test-LabWindow([string]$Name, [string[]]$Flags, [string]$Marker) {
    $state = (New-Item -ItemType Directory -Path (Join-Path $outputDir $Name) -Force).FullName
    $profiles = Join-Path $state 'profiles.json'
    $log = Join-Path $state 'stderr.log'
    [IO.File]::WriteAllText($profiles, '{"profiles":{}}')
    $start = [Diagnostics.ProcessStartInfo]::new($gui)
    $start.UseShellExecute = $false
    $start.WindowStyle = 'Hidden'
    $start.RedirectStandardError = $true
    foreach ($argument in @('--demo', '--profiles', $profiles)) { $start.ArgumentList.Add($argument) }
    $start.Environment['W11TERM_DATA_DIR'] = $state
    $start.Environment['W11TERM_SETTINGS_PATH'] = Join-Path $state 'appearance.json'
    $start.Environment['W11TERM_UI_SMOKE'] = '1'
    foreach ($flag in $Flags) { $start.Environment[$flag] = '1' }

    $process = [Diagnostics.Process]::Start($start)
    try {
        $report = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(60000)) {
            $process.Kill($true)
            $process.WaitForExit()
        }
        $stderr = $report.GetAwaiter().GetResult()
        [IO.File]::WriteAllText($log, $stderr)
        if ($process.ExitCode -ne 0 -or -not $stderr.Contains($Marker)) {
            throw "$Name native smoke failed (exit $($process.ExitCode)). See $log`n$stderr"
        }
        Write-Output "$Name native smoke passed. Artifacts in $state"
    } finally {
        $process.Dispose()
    }
}

Test-LabWindow 'tabs' @('W11TERM_INTERACTION_SMOKE', 'W11TERM_FONT_SMOKE') 'Tabbed UI smoke passed'
Test-LabWindow 'search' @('W11TERM_SEARCH_SMOKE') 'Search UI smoke passed'
Test-LabWindow 'player' @('W11TERM_PLAYER_SMOKE') 'Player UI smoke passed'
