param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_RELEASE_FIXTURE_PORT = [IO.File]::ReadAllText((Join-Path $State 'release-port'))
$expected = [IO.File]::ReadAllText((Join-Path $State 'release-expected.json')) | ConvertFrom-Json
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$check = 'Check latest Windows release'
$save = 'Save verified ZIP'
$saveDialog = 'Save verified Windows ZIP'

function Start-ReleaseCheck([int]$Number) {
    $control = Wait-Until 'the release check is available' {
        $candidate = Find-Element $check $button
        if ($candidate -and $candidate.Current.IsEnabled) { $candidate }
    }
    [IO.File]::WriteAllText((Join-Path $State "release-check-$Number"), 'requested')
    Invoke-Button $control
}

function Start-ZipSave([string]$Path) {
    $control = Wait-Until 'Save verified ZIP to be available' {
        $candidate = Find-Element $save $button
        if ($candidate -and $candidate.Current.IsEnabled) { $candidate }
    }
    Invoke-Button $control
    $dialog = Wait-OwnedDialog $saveDialog
    if ($Path) {
        $null = Wait-Until 'enter the Windows ZIP destination' {
            [FixtureDialog]::SetCommonDialogFilename($dialog, $Path)
        }
        $command = 1
    } else { $command = 2 }
    $null = Wait-Until 'submit the Windows ZIP picker' { [FixtureDialog]::ClickButton($dialog, $command) }
    $null = Wait-Until 'the Windows ZIP picker closes' { !(Find-OwnedDialog $saveDialog) }
}

function Wait-ZipStatus([string]$Text) {
    $null = Wait-Until $Text {
        Get-NativeTextLabels | Where-Object { $_.Contains($Text) }
    }
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
    $null = Wait-Until 'the latest verified release information' {
        (Find-Element $expected.first) -and (Find-Element $expected.published) -and (Find-Element 'Open release page on GitHub')
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
    Start-ZipSave
    $downloads = Join-Path $State 'verified releases'
    $null = [IO.Directory]::CreateDirectory($downloads)
    $bad = Join-Path $downloads 'corrupt.zip'
    [IO.File]::WriteAllText((Join-Path $State 'release-download-bad'), 'requested')
    Start-ZipSave $bad
    Wait-ZipStatus 'The Windows ZIP SHA-256 does not match the published checksum'
    if ([IO.File]::Exists($bad)) { throw 'The corrupt package was published' }

    $good = Join-Path $downloads ('release ' + [char]0x96ea + '.zip')
    [IO.File]::WriteAllText((Join-Path $State 'release-download-good'), 'requested')
    Start-ZipSave $good
    Wait-ZipStatus 'Verified ZIP saved to'
    if (![IO.File]::Exists($good)) { throw 'The verified ZIP was not published' }
    $stamp = [IO.File]::GetLastWriteTimeUtc($good)
    Start-ZipSave $good
    Wait-ZipStatus 'The selected destination already exists'
    if ([IO.File]::GetLastWriteTimeUtc($good) -ne $stamp) {
        throw 'Saving to an existing ZIP changed the previous package'
    }

    $cancelled = Join-Path $downloads 'cancelled.zip'
    [IO.File]::WriteAllText((Join-Path $State 'release-download-cancel'), 'requested')
    Start-ZipSave $cancelled
    $null = Wait-Until 'part of the archive reaches the client' {
        [IO.File]::Exists((Join-Path $State 'release-download-cancel-ready'))
    }
    Invoke-Button (Wait-Until 'Cancel ZIP download to be enabled' {
        $control = Find-Element 'Cancel ZIP download' $button
        if ($control -and $control.Current.IsEnabled) { $control }
    })
    Wait-ZipStatus 'Cancelling the ZIP download'
    [IO.File]::WriteAllText((Join-Path $State 'release-download-cancelled'), 'proceed')
    Wait-ZipStatus 'ZIP download cancelled'
    $saved = @([IO.Directory]::GetFileSystemEntries($downloads))
    if ($saved.Count -ne 1 -or [IO.Path]::GetFileName($saved[0]) -cne [IO.Path]::GetFileName($good)) {
        throw 'A failed or cancelled download left a file in the destination'
    }
} finally {
    Close-FixtureApp $app
}
