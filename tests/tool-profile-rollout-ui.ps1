param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$menu = [System.Windows.Automation.ControlType]::MenuItem
$edit = [System.Windows.Automation.ControlType]::Edit
$checkbox = [System.Windows.Automation.ControlType]::CheckBox
$value = [System.Windows.Automation.ValuePattern]::Pattern
$on = [System.Windows.Automation.ToggleState]::On
$off = [System.Windows.Automation.ToggleState]::Off
$dot = ' ' + [char]0xb7 + ' '
$candidate = Join-Path $State 'Fleet revision.json'
$candidateBefore = [IO.File]::ReadAllText($candidate)
$catalogBefore = [IO.File]::ReadAllText($profiles)
$digest = [IO.File]::ReadAllText((Join-Path $State 'candidate-pin'))

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
function Select-Manifest([string]$Path) {
    Click 'Roll out tool manifest'
    $picker = Wait-OwnedDialog 'Choose SSH tool bundle'
    if ($Path) {
        $null = Wait-Until 'enter the candidate filename' { [FixtureDialog]::SetFilename($picker, $Path, '') }
        $command = 1
    } else { $command = 2 }
    $null = Wait-Until 'submit the tool manifest picker' { [FixtureDialog]::ClickButton($picker, $command) }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    Open-Settings
    Click 'Edit profile East'
    (Text-Field 'Scrollback lines').SetValue('8765')
    $null = Wait-Until 'the open profile draft has an unsaved scrollback setting' {
        (Text-Field 'Scrollback lines').Current.Value -ceq '8765'
    }

    Open-Settings
    Click 'Audit SSH tool profiles'
    Select-Manifest
    $null = Wait-Until 'cancelling the picker returns to the audit' {
        Find-Element 'Filter audited SSH profiles' $edit
    }
    Select-Manifest $candidate
    $null = Wait-CheckBox 'Tool rollout profile Other' $off
    if (Find-Element 'Tool rollout profile Different' $checkbox) { throw 'The picker offered an incompatible destination' }
    $filter = Text-Field 'Filter tool rollout profiles'
    $filter.SetValue('east.example.test')
    $null = Wait-Until 'the filter hides West' { !(Find-Element 'Tool rollout profile West' $checkbox) }
    (Wait-CheckBox 'Tool rollout profile East' $off).Toggle()
    $null = Wait-CheckBox 'Tool rollout profile East' $on
    $filter.SetValue('')
    (Wait-CheckBox 'Tool rollout profile West' $off).Toggle()
    $null = Wait-CheckBox 'Tool rollout profile West' $on
    $null = Wait-CheckBox 'Tool rollout profile Other' $off

    Click 'Review selected tool rollout'
    $null = Wait-Until 'the preview shows the digest, pin policies, and command changes' {
        (Find-Element "Proposed manifest SHA-256 $digest") -and
            (Find-Element 'Keeps a manifest pin') -and (Find-Element 'Follows the manifest file') -and
            (Find-Element "Added commands${dot}jq") -and (Find-Element "Removed commands${dot}ncdu") -and
            (Find-Element "Changed builds${dot}rg")
    }
    if ([IO.File]::ReadAllText($profiles) -cne $catalogBefore) { throw 'The preview edited saved profiles' }
    [IO.File]::WriteAllText($candidate, "$candidateBefore`n")
    Click 'Apply reviewed tool rollout'
    $null = Wait-Until 'the apply rejects the stale candidate' {
        Find-Element 'The candidate, baseline, or profile approvals changed. Preview the rollout again.'
    }
    if ([IO.File]::ReadAllText($profiles) -cne $catalogBefore) { throw 'A stale rollout edited saved profiles' }
    [IO.File]::WriteAllText($candidate, $candidateBefore)
    Click 'Review selected tool rollout'
    Click 'Apply reviewed tool rollout'
    $null = Wait-Until 'the two selected bindings were updated' {
        Find-Element 'Updated tool manifests in 2 saved SSH profiles.'
    }
    $saved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles
    if ($saved.East.tool_bundle.bundle -cne $candidate -or $saved.East.tool_bundle.manifest_sha256 -cne $digest -or
        $saved.West.tool_bundle.bundle -cne $candidate -or $saved.West.tool_bundle.manifest_sha256 -or
        $saved.Other.tool_bundle.bundle -cne 'previous.json' -or
        $saved.Different.tool_bundle.bin_directory -cne '.local/other' -or
        $saved.East.tool_bundle.add_to_path.shell -cne 'bash') {
        throw 'The batch changed the wrong bindings or approval policies'
    }
    Click 'Close tool rollout'
    Click 'Edit SSH tools for East'
    $null = Wait-Until 'the existing draft retains its edit and receives the new manifest' {
        $field = Find-Element 'Tool bundle manifest' $edit
        $scrollback = Find-Element 'Scrollback lines' $edit
        $field -and $scrollback -and
            [FixtureDialog]::SameFile($candidate, $field.GetCurrentPattern($value).Current.Value) -and
            $scrollback.GetCurrentPattern($value).Current.Value -ceq '8765'
    }
    Click 'Save profile changes'
    $null = Wait-Until 'the open draft saves against the updated profile' {
        $savedEast = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.East
        $savedEast.scrollback_lines -eq 8765 -and $savedEast.tool_bundle.manifest_sha256 -ceq $digest
    }
} finally {
    if ($app) { Close-FixtureApp $app }
}
