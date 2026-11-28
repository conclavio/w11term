param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$menu = [System.Windows.Automation.ControlType]::MenuItem
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$on = [System.Windows.Automation.ToggleState]::On
$off = [System.Windows.Automation.ToggleState]::Off
$candidate = Join-Path $State 'revision.json'
$previous = Join-Path $State 'previous.json'
$receipt = Join-Path $State 'Approved rollout.json'
$oldCatalog = [IO.File]::ReadAllText($profiles)
$oldCandidate = [IO.File]::ReadAllText($candidate)
$oldBundle = [IO.File]::ReadAllText($previous)
$digest = ((& $Cli inspect-dotfiles-bundle $candidate --shell bash | ConvertFrom-Json).result.shells[0]).bundle_sha256
$oldPin = ((& $Cli inspect-dotfiles-bundle $previous --shell bash | ConvertFrom-Json).result.shells[0]).bundle_sha256

function Wait-Control([string]$Name, $Type = $button) {
    $search = @{ scroll = $null }
    Wait-Until "enabled visible '$Name'" {
        $control = Find-Element $Name $Type
        if ($control -and $control.Current.IsEnabled) {
            if (!$control.Current.IsOffscreen) { return $control }
            Move-NativeControlIntoView $control $search
        }
    }
}
function Click([string]$Name, $Type = $button) { Invoke-Button (Wait-Control $Name $Type) }

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Fixture local"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    Click 'Tab menu'
    Click 'Settings' $menu
    Click 'Edit profile East'
    Set-NativeExpansion 'SSH dotfiles' $true
    $bundle = Wait-Control 'Dotfiles bundle' $edit
    $bundle.GetCurrentPattern($value).SetValue($candidate)
    $null = Wait-Until 'the revision is selected in the editor' {
        (Find-Element 'Dotfiles bundle' $edit).GetCurrentPattern($value).Current.Value -ceq $candidate
    }

    Click 'Roll out to saved profiles'
    $null = Wait-CheckBox 'Rollout profile Other' $off
    (Wait-CheckBox 'Rollout profile East' $off).Toggle()
    $null = Wait-CheckBox 'Rollout profile East' $on
    (Wait-CheckBox 'Rollout profile West' $off).Toggle()
    $null = Wait-CheckBox 'Rollout profile West' $on
    Click 'Review selected rollout profiles'
    $null = Wait-Until 'the Bash digest appears in the review' { Find-Element "Proposed digest $digest" }
    $null = Wait-Until 'the review identifies the pin and the unavailable Fish baseline' {
        (Find-Element 'Compared with the approved pinned version.') -and
            (Find-Element 'The approved bundle could not be verified. Showing proposed fragments.')
    }
    Set-NativeExpansion 'East prompt changes' $true
    $null = Wait-Until 'the reviewed Bash lines show the old and new prompts' {
        Get-NativeTextLabels | Where-Object { $_.Contains('Removed line 1') -and $_.Contains('PS1=before') -and
            $_.Contains('Added line 1') -and $_.Contains('PS1=reviewed') }
    }
    Set-NativeExpansion 'West prompt changes' $true
    $null = Wait-Until 'a missing baseline shows only the proposed Fish prompt' {
        Get-NativeTextLabels | Where-Object { $_.Contains('Proposed line 1') -and $_.Contains('set -g fish_greeting reviewed') }
    }
    if ([IO.File]::ReadAllText($profiles) -cne $oldCatalog) { throw 'Preview changed the catalog' }

    [IO.File]::WriteAllText($candidate, $oldCandidate.Replace('PS1=reviewed', 'PS1=edited'))
    Click 'Apply reviewed rollout'
    $null = Wait-Until 'the rollout rejected the changed revision' {
        Find-Element 'The candidate, baseline, or profile approvals changed. Preview the rollout again.'
    }
    if ([IO.File]::ReadAllText($profiles) -cne $oldCatalog) { throw 'Stale rollout changed the catalog' }
    [IO.File]::WriteAllText($candidate, $oldCandidate)
    Click 'Review selected rollout profiles'
    Click 'Apply reviewed rollout'
    $null = Wait-Until 'the reviewed profiles were saved together' { Find-Element 'Updated dotfiles in 2 saved SSH profiles.' }
    $null = Wait-Until 'the reviewed catalog updates are on disk' {
        $saved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles
        $saved.East.dotfiles.bundle -ceq $candidate -and $saved.East.dotfiles.bundle_sha256 -ceq $digest -and
            $saved.West.dotfiles.bundle -ceq $candidate -and !$saved.West.dotfiles.bundle_sha256 -and
            $saved.Other.dotfiles.bundle -ceq 'previous.json'
    }
    $rolledOut = [IO.File]::ReadAllText($profiles)
    Click 'Save rollout receipt'
    $dialog = Wait-OwnedDialog 'Save dotfile rollout receipt'
    $null = Wait-Until 'cancel the receipt picker' { [FixtureDialog]::ClickButton($dialog, 2) }
    $null = Wait-Control 'Save rollout receipt'
    if ([IO.File]::Exists((Join-Path $State 'dotfile-rollout.json'))) { throw 'Canceled picker wrote a receipt' }

    Click 'Save rollout receipt'
    $dialog = Wait-OwnedDialog 'Save dotfile rollout receipt'
    $null = Wait-Until 'enter a draft receipt filename' { [FixtureDialog]::SetCommonDialogFilename($dialog, "$receipt.draft") }
    $null = Wait-Until 'choose the receipt filename' { [FixtureDialog]::SetCommonDialogFilename($dialog, $receipt) }
    $null = Wait-Until 'save the receipt' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'receipt is published and its destination is shown' {
        if ([IO.File]::Exists($receipt)) {
            $prefix = 'Saved rollout receipt ' + [char]0xb7 + ' '
            Get-NativeTextLabels | Where-Object {
                $_.StartsWith($prefix) -and [FixtureDialog]::SameFile($receipt, $_.Substring($prefix.Length))
            }
        }
    }
    $preview = & $Cli rollback-dotfiles-profiles $receipt | ConvertFrom-Json
    $east = $preview.result.profiles | Where-Object profile -eq 'East'
    $west = $preview.result.profiles | Where-Object profile -eq 'West'
    if (!$preview.ok -or $preview.result.applied -or $preview.result.changed_profiles -ne 2 -or
        $east.restore.bundle_sha256 -cne $oldPin -or $east.restore_status -cne 'matches-pin' -or
        $west.restore_status -cne 'bundle-unavailable') {
        throw 'The exported receipt did not produce the expected CLI rollback review'
    }
    if ([IO.File]::ReadAllText($profiles) -cne $rolledOut) { throw 'Receipt export or preview changed the catalog' }

    Click 'Review rollback'
    $null = Wait-Until 'the rollback preview shows the old pin and unavailable Fish bundle' {
        (Find-Element "Restore pin $oldPin") -and
            (Find-Element 'The previous bundle matches its pin.') -and
            (Find-Element 'The previous bundle is unavailable for this shell.')
    }
    if ([IO.File]::ReadAllText($profiles) -cne $rolledOut) { throw 'Rollback preview changed the catalog' }
    [IO.File]::WriteAllText($previous, $oldBundle.Replace('PS1=before', 'PS1=changed'))
    Click 'Apply reviewed rollback'
    $null = Wait-Until 'rollback rejects a changed previous bundle' {
        Find-Element 'The rollback files or approvals changed. Preview the rollback again.'
    }
    if ([IO.File]::ReadAllText($profiles) -cne $rolledOut) { throw 'Stale rollback changed the catalog' }
    [IO.File]::WriteAllText($previous, $oldBundle)
    Click 'Review rollback'
    Click 'Apply reviewed rollback'
    $null = Wait-Until 'the previous approvals were restored together' { Find-Element 'Restored dotfiles in 2 saved SSH profiles.' }
    $saved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles
    if ($saved.East.dotfiles.bundle -cne $previous -or $saved.East.dotfiles.bundle_sha256 -cne $oldPin -or
        $saved.West.dotfiles.bundle -cne $previous -or $saved.West.dotfiles.bundle_sha256 -or
        $saved.Other.dotfiles.bundle -cne 'previous.json') {
        throw 'The rollback did not restore the selected approvals while preserving the unchecked profile'
    }
    Click 'Close dotfile rollout'
    $null = Wait-Until 'the initiating draft shows its restored pin' { Find-Element "Pinned dotfile version $oldPin" }
    Invoke-Button (Wait-Until 'the profile save action is enabled' {
        $save = Find-Element 'Save profile changes' $button
        if ($save -and $save.Current.IsEnabled) { $save }
    })
    $null = Wait-Until 'the initiating draft can still be saved' { Find-Element 'Changes saved.' }
} finally {
    if ($app) { Close-FixtureApp $app }
}
