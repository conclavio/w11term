param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$catalogBefore = [IO.File]::ReadAllText($profiles)
$menu = [System.Windows.Automation.ControlType]::MenuItem
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$on = [System.Windows.Automation.ToggleState]::On
$off = [System.Windows.Automation.ToggleState]::Off
$westManifest = Join-Path $State 'west-tools.json'
$westExecutable = Join-Path $State 'rg'
$dot = ' ' + [char]0xb7 + ' '
$fixtureProfiles = ($catalogBefore | ConvertFrom-Json).profiles
$expectedPin = $fixtureProfiles.West.tool_bundle.manifest_sha256
$currentPin = $fixtureProfiles.East.tool_bundle.manifest_sha256

function Click([string]$Name, $Type = $button) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $Type
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}
function Open-Settings {
    Click 'Tab menu'
    Click 'Settings' $menu
}
function Text-Field([string]$Name) {
    (Wait-Until "field '$Name'" { Find-Element $Name $edit }).GetCurrentPattern($value)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    Open-Settings
    Click 'Edit profile West'
    Set-NativeExpansion 'SSH tools' $true
    (Text-Field 'Tool bundle SFTP home').SetValue('/home/unsaved')
    $null = Wait-Until 'the tool draft contains the unsaved home' {
        (Text-Field 'Tool bundle SFTP home').Current.Value -ceq '/home/unsaved'
    }
    Set-NativeExpansion 'SSH tools' $false

    Open-Settings
    Click 'Audit SSH tool profiles'
    $null = Wait-Until 'the audit distinguishes matching, changed, and missing manifests' {
        (Find-Element 'Showing 4 of 4 SSH profiles.') -and
            (Find-Element 'Manifest matches its pin') -and
            (Find-Element 'Manifest differs from its pin') -and
            (Find-Element 'Manifest could not be loaded')
    }
    if (Find-Element 'Prepare pinned downloads for East' $button) { throw 'The audit offered a download before selecting the architecture scope' }
    (Wait-CheckBox 'Show only tool sync issues' $off).Toggle()
    $null = Wait-Until 'issues include invalid PATH settings on a following profile' {
        (Find-Element 'Showing 3 of 4 SSH profiles.') -and
            !(Find-Element 'Edit SSH tools for East' $button) -and
            (Find-Element 'Edit SSH tools for North' $button)
    }
    (Text-Field 'Filter audited SSH profiles').SetValue('ops@west.example.test')
    $null = Wait-Until 'the SSH address filter finds only West' {
        (Find-Element 'Showing 1 of 4 SSH profiles.') -and
            (Find-Element 'Edit SSH tools for West' $button) -and
            !(Find-Element 'Edit SSH tools for Missing' $button)
    }
    Set-NativeExpansion 'Manifest details for West' $true
    $null = Wait-Until 'the review shows both manifest hashes and its path' {
        Get-NativeTextLabels | Where-Object {
            $_.Contains($westManifest) -and $_.Contains("Pinned SHA-256 $expectedPin") -and
                $_.Contains("Current SHA-256 $currentPin")
        }
    }

    Click 'Edit SSH tools for West'
    $null = Wait-Until 'the existing draft opens at SSH tools' {
        $homeField = Find-Element 'Tool bundle SFTP home' $edit
        $tools = Get-NativeExpander 'SSH tools'
        if ($homeField -and $tools.Current.ExpandCollapseState -eq 'Expanded') {
            $homeField.GetCurrentPattern($value).Current.Value -ceq '/home/unsaved'
        }
    }
    if ([IO.File]::ReadAllText($profiles) -cne $catalogBefore) { throw 'The audit or draft navigation edited the catalog' }

    Open-Settings
    $null = Wait-Until 'returning to Settings preserves the audit filters' {
        (Find-Element 'Showing 1 of 4 SSH profiles.') -and
            (Text-Field 'Filter audited SSH profiles').Current.Value -ceq 'ops@west.example.test'
    }
    [IO.File]::WriteAllBytes($westManifest, [IO.File]::ReadAllBytes((Join-Path $State 'previous-tools.json')))
    Click 'Refresh audit'
    $null = Wait-Until 'refresh clears the repaired manifest from the issue filter' {
        Find-Element 'No SSH profiles match these filters.'
    }
    (Wait-CheckBox 'Show only tool sync issues' $on).Toggle()
    $null = Wait-Until 'the repaired profile is now reported as matching' {
        (Find-Element 'Showing 1 of 4 SSH profiles.') -and
            (Find-Element 'Manifest matches its pin')
    }

    Select-NativeComboItem 'SSH tool audit scope' 'Verify ARM64 builds'
    $null = Wait-Until 'ARM64 verification shows the unavailable command' {
        Find-Element "ARM64${dot}0 of 1 commands ready locally"
    }
    if (Find-Element 'Prepare pinned downloads for West' $button) { throw 'The audit offered x86-64 downloads in ARM64 scope' }
    Set-NativeExpansion 'ARM64 builds for West' $true
    $null = Wait-Until 'the ARM64 details explain that no build was listed' {
        Find-Element "rg${dot}No build in manifest"
    }
    (Wait-CheckBox 'Show only tool sync issues' $off).Toggle()
    $null = Wait-Until 'the architecture gap keeps West in the issue filter' {
        (Find-Element 'Showing 1 of 4 SSH profiles.') -and
            (Find-Element "ARM64${dot}0 of 1 commands ready locally")
    }
    Select-NativeComboItem 'SSH tool audit scope' 'Verify x86-64 builds'
    $null = Wait-Until 'a verified x86-64 command clears West from the issue filter' {
        Find-Element 'No SSH profiles match these filters.'
    }
    (Wait-CheckBox 'Show only tool sync issues' $on).Toggle()
    $null = Wait-Until 'the local x86-64 build is ready' {
        Find-Element "x86-64${dot}1 of 1 commands ready locally"
    }
    Set-NativeExpansion 'x86-64 builds for West' $true
    $approvedBuild = ([IO.File]::ReadAllText($westManifest) | ConvertFrom-Json).tools[0].builds[0]
    $null = Wait-Until 'build details show the verified file and SHA-256' {
        Get-NativeTextLabels | Where-Object {
            $_.Contains('Verified local file') -and $_.Contains($westExecutable) -and
                $_.Contains("Executable SHA-256 $($approvedBuild.sha256)")
        }
    }
    [IO.File]::Move($westExecutable, (Join-Path $State 'moved-rg'))
    Click 'Refresh audit'
    $null = Wait-Until 'refresh reruns the same x86-64 executable audit' {
        Find-Element "x86-64${dot}0 of 1 commands ready locally"
    }
    Set-NativeExpansion 'x86-64 builds for West' $true
    $null = Wait-Until 'build details identify the missing file' {
        Get-NativeTextLabels | Where-Object {
            $_.Contains('Missing local file') -and $_.Contains($westExecutable)
        }
    }
    $reviewedBytes = [IO.File]::ReadAllBytes($westManifest)
    [IO.File]::WriteAllBytes($westManifest, [byte[]]($reviewedBytes + 10))
    Click 'Prepare pinned downloads for West'
    $null = Wait-Until 'preparation rejects a manifest edited since the audit' {
        Find-Element 'Could not prepare downloads for West. The tool manifest or its pin changed. Refresh the audit before preparing downloads.'
    }
    [IO.File]::WriteAllBytes($westManifest, $reviewedBytes)
    Click 'Refresh audit'
    $null = Wait-Until 'the refreshed profile can prepare its x86-64 downloads' {
        (Find-Element 'Prepare pinned downloads for West' $button) -and
            (Find-Element "x86-64${dot}0 of 1 commands ready locally")
    }
    # Seed the exact bytes after the audit so the explicit action verifies the cached download
    # while the local build stays missing. The fixture needs no network service.
    $cache = Join-Path $env:W11TERM_DATA_DIR 'tools/cache/v1'
    $null = [IO.Directory]::CreateDirectory($cache)
    $cached = Join-Path $cache $approvedBuild.sha256
    [IO.File]::Copy((Join-Path $State 'moved-rg'), $cached)
    Click 'Prepare pinned downloads for West'
    $null = Wait-Until 'preparation verifies the download and refreshes the executable audit' {
        (Find-Element 'Prepared 1 pinned build for West.') -and
            (Find-Element "x86-64${dot}1 of 1 commands ready locally")
    }
    if ([IO.File]::Exists($westExecutable) -or
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($westManifest)) -cne [Convert]::ToBase64String($reviewedBytes)) {
        throw 'Preparing a pinned download changed a local build or manifest'
    }
    Click 'Back to settings'
    $null = Wait-Until 'the regular Settings page is restored' {
        Find-Element 'Filter profiles' $edit
    }
    if ([IO.File]::ReadAllText($profiles) -cne $catalogBefore) { throw 'The audit refresh edited the catalog' }
} finally {
    if ($app) { Close-FixtureApp $app }
}
