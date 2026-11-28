param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_RELEASE_FIXTURE_PORT = [IO.File]::ReadAllText((Join-Path $State 'release-port'))
$expected = [IO.File]::ReadAllText((Join-Path $State 'release-expected.json')) | ConvertFrom-Json
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$check = 'Check latest Windows release'

function Start-ReleaseCheck([int]$Number) {
    $control = Wait-Until 'the release check is available' {
        $candidate = Find-Element $check $button
        if ($candidate -and $candidate.Current.IsEnabled) { $candidate }
    }
    [IO.File]::WriteAllText((Join-Path $State "release-check-$Number"), 'requested')
    Invoke-Button $control
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    Invoke-Button (Get-NativeMenuAction 'Tab menu' 'Settings')
    Set-NativeExpansion 'Windows release' $true
    $null = Wait-Until 'the running revision and offline release link' {
        (Find-Element $expected.running) -and (Find-Element 'Open Latest on GitHub')
    }
    Start-ReleaseCheck 0
    $published = 'Published 2026-11-27 ' + [char]0x00b7 + ' 43.0 MiB ZIP'
    $null = Wait-Until 'the latest verified release information' {
        (Find-Element $expected.first) -and (Find-Element $published) -and (Find-Element 'Open release page on GitHub')
    }
    Start-ReleaseCheck 1
    $rateLimit = 'Release check failed ' + [char]0x00b7 + ' GitHub refused or rate-limited the release check (HTTP 429)'
    $null = Wait-Until 'a rate limit leaves the Latest link available' {
        (Find-Element $rateLimit) -and (Find-Element 'Open Latest on GitHub')
    }
    Start-ReleaseCheck 2
    $null = Wait-Until 'a retry shows the other release' {
        (Find-Element $expected.retry) -and (Find-Element 'Open release page on GitHub')
    }
} finally {
    Close-FixtureApp $app
}
