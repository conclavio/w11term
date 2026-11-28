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
    Click 'Back to settings'
    $null = Wait-Until 'the regular Settings page is restored' {
        Find-Element 'Filter profiles' $edit
    }
    if ([IO.File]::ReadAllText($profiles) -cne $catalogBefore) { throw 'The audit refresh edited the catalog' }
} finally {
    if ($app) { Close-FixtureApp $app }
}
