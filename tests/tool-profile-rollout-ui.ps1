param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$tabItem = [System.Windows.Automation.ControlType]::TabItem
$selection = [System.Windows.Automation.SelectionItemPattern]::Pattern
$edit = [System.Windows.Automation.ControlType]::Edit
$checkbox = [System.Windows.Automation.ControlType]::CheckBox
$value = [System.Windows.Automation.ValuePattern]::Pattern
$on = [System.Windows.Automation.ToggleState]::On
$off = [System.Windows.Automation.ToggleState]::Off
$dot = ' ' + [char]0xb7 + ' '
$candidate = Join-Path $State 'Fleet revision.json'
$candidateBefore = [IO.File]::ReadAllText($candidate)
$previous = Join-Path $State 'previous.json'
$previousBefore = [IO.File]::ReadAllText($previous)
$receipt = Join-Path $State 'Tool rollout receipt.json'
$more = [char]0x2026
$catalogBefore = [IO.File]::ReadAllText($profiles)
$digest = [IO.File]::ReadAllText((Join-Path $State 'candidate-pin'))
$previousPin = ($catalogBefore | ConvertFrom-Json).profiles.East.tool_bundle.manifest_sha256

function Click([string]$Name, $Type = $button) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $Type
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}
function Open-Settings {
    if (!(Find-Element 'Settings' $tabItem)) {
        Invoke-Button (Get-NativeMenuAction 'Tab menu' 'Settings' -VisibleItem)
    }
    $null = Wait-Until 'the Settings tab is selected' {
        $tab = Find-Element 'Settings' $tabItem
        if ($tab) {
            $selected = $tab.GetCurrentPattern($selection)
            if ($selected.Current.IsSelected) { return $true }
            $selected.Select()
        }
    }
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
function Open-ToolReceipt([string]$Path) {
    Click "Open tool rollout receipt$more"
    $dialog = Wait-OwnedDialog 'Open SSH tool rollout receipt'
    if ($Path) {
        $null = Wait-Until 'enter the receipt path' { [FixtureDialog]::SetFilename($dialog, $Path, '') }
        $command = 1
    } else { $command = 2 }
    $null = Wait-Until 'submit the receipt picker' { [FixtureDialog]::ClickButton($dialog, $command) }
}
function Wait-RollbackReview {
    $null = Wait-Until 'the rollback preview shows both previous pin policies and current digests' {
        (Find-Element "Restore pin SHA-256 $previousPin") -and
            (Find-Element "Current manifest SHA-256 $digest") -and
            (Find-Element 'Previous manifest matches its pin') -and
            (Find-Element 'Follows the previous manifest file')
    }
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
    $prepare = 'Prepare candidate x86-64 downloads'
    $null = Wait-Until 'the rollout checks both declared candidate architectures' {
        (Find-Element "Candidate x86-64${dot}0 of 2 commands ready locally") -and
            (Find-Element "Candidate ARM64${dot}0 of 2 commands ready locally") -and
            (Find-Element $prepare $button)
    }
    if (Find-Element 'Prepare candidate ARM64 downloads' $button) {
        throw 'A local-only candidate build offered a download'
    }
    [IO.File]::WriteAllText($candidate, "$candidateBefore`n")
    Click $prepare
    $null = Wait-Until 'preparation rejects a changed candidate' {
        Find-Element 'The candidate manifest changed. Review the rollout again.'
    }
    if ([IO.File]::ReadAllText($profiles) -cne $catalogBefore) { throw 'Candidate preparation edited profiles' }
    [IO.File]::WriteAllText($candidate, $candidateBefore)
    Click 'Check candidate builds'
    $null = Wait-Until 'the restored review again offers preparation' { Find-Element $prepare $button }
    $cache = Join-Path $env:W11TERM_DATA_DIR 'tools/cache/v1'
    $null = [IO.Directory]::CreateDirectory($cache)
    $executablePin = ($candidateBefore | ConvertFrom-Json).tools[0].builds[0].sha256
    [IO.File]::Copy((Join-Path $State 'candidate-executable'), (Join-Path $cache $executablePin))
    Click $prepare
    $null = Wait-Until 'preparation refreshes readiness before any rollout' {
        (Find-Element "Candidate x86-64${dot}1 of 2 commands ready locally") -and
            (Find-Element "Candidate ARM64${dot}0 of 2 commands ready locally") -and
            !(Find-Element $prepare $button)
    }
    if ([IO.File]::ReadAllText($profiles) -cne $catalogBefore -or
        [IO.File]::ReadAllText($candidate) -cne $candidateBefore) {
        throw 'Checking the candidate or preparing a build changed the rollout inputs'
    }
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
    if (![FixtureDialog]::SameFile($candidate, $saved.East.tool_bundle.bundle) -or $saved.East.tool_bundle.manifest_sha256 -cne $digest -or
        ![FixtureDialog]::SameFile($candidate, $saved.West.tool_bundle.bundle) -or $saved.West.tool_bundle.manifest_sha256 -or
        $saved.Other.tool_bundle.bundle -cne 'previous.json' -or
        $saved.Different.tool_bundle.bin_directory -cne '.local/other' -or
        $saved.East.tool_bundle.add_to_path.shell -cne 'bash') {
        throw 'The batch changed the wrong bindings or approval policies'
    }
    $rolledOut = [IO.File]::ReadAllText($profiles)
    Click 'Review tool rollback'
    Wait-RollbackReview
    [IO.File]::WriteAllText($previous, "$previousBefore`n")
    Click 'Apply reviewed tool rollback'
    $null = Wait-Until 'the rollback rejects a changed previous manifest' {
        Find-Element 'The rollback manifests or approvals changed. Preview the rollback again.'
    }
    if ([IO.File]::ReadAllText($profiles) -cne $rolledOut) { throw 'A stale rollback changed the catalog' }
    [IO.File]::WriteAllText($previous, $previousBefore)
    Click 'Back to rollout result'
    Click "Save tool rollout receipt$more"
    $dialog = Wait-OwnedDialog 'Save SSH tool rollout receipt'
    $null = Wait-Until 'enter an export filename' { [FixtureDialog]::SetCommonDialogFilename($dialog, $receipt) }
    $null = Wait-Until 'save the rollback receipt' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the exported receipt and destination are available' {
        if ([IO.File]::Exists($receipt)) {
            $prefix = "Saved rollout receipt$dot"
            Get-NativeTextLabels | Where-Object {
                $_.StartsWith($prefix) -and [FixtureDialog]::SameFile($receipt, $_.Substring($prefix.Length))
            }
        }
    }
    $preview = & $Cli rollback-tool-profiles $receipt | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or !$preview.ok -or $preview.result.applied -or
        (($preview.result.profiles.profile) -join ',') -cne 'East,West' -or
        (($preview.result.profiles.restore_status) -join ',') -cne 'matches-pin,follows-file') {
        throw 'The exported receipt did not produce the expected CLI rollback review'
    }
    if ([IO.File]::ReadAllText($profiles) -cne $rolledOut) { throw 'Exporting or previewing a receipt changed profiles' }

    Click 'Close tool rollout'
    Click 'Edit SSH tools for East'
    $null = Wait-Until 'the existing draft retains its edit and receives the new manifest' {
        $field = Find-Element 'Tool bundle manifest' $edit
        $scrollback = Find-Element 'Scrollback lines' $edit
        $field -and $scrollback -and
            [FixtureDialog]::SameFile($candidate, $field.GetCurrentPattern($value).Current.Value) -and
            $scrollback.GetCurrentPattern($value).Current.Value -ceq '8765'
    }
    Open-Settings
    Open-ToolReceipt
    $null = Wait-Until 'canceling the receipt picker returns to the audit' { Find-Element 'Filter audited SSH profiles' $edit }
    $otherCatalog = Join-Path $State 'other-profiles.json'
    $otherReceipt = Join-Path $State 'Other rollout receipt.json'
    [IO.File]::WriteAllText($otherCatalog, $rolledOut)
    $otherReply = [IO.File]::ReadAllText($receipt) | ConvertFrom-Json
    $otherReply.result.profiles_path = $otherCatalog
    [IO.File]::WriteAllText($otherReceipt, ($otherReply | ConvertTo-Json -Depth 25))
    Open-ToolReceipt $otherReceipt
    $null = Wait-Until 'the audit rejects a receipt for a different profile catalog' {
        Find-Element 'This rollout receipt is for a different profile catalog'
    }
    if ([IO.File]::ReadAllText($profiles) -cne $rolledOut -or [IO.File]::ReadAllText($otherCatalog) -cne $rolledOut) {
        throw 'Opening a receipt for another catalog changed a catalog'
    }
    Click 'Close tool rollback'
    Open-ToolReceipt $receipt
    Wait-RollbackReview
    if ([IO.File]::ReadAllText($profiles) -cne $rolledOut) { throw 'Reopening a receipt changed profiles' }
    Click 'Apply reviewed tool rollback'
    $null = Wait-Until 'the previous approvals were restored' {
        Find-Element 'Restored tool manifests in 2 saved SSH profiles.'
    }
    $restored = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles
    if (![FixtureDialog]::SameFile($previous, $restored.East.tool_bundle.bundle) -or
        $restored.East.tool_bundle.manifest_sha256 -cne $previousPin -or
        ![FixtureDialog]::SameFile($previous, $restored.West.tool_bundle.bundle) -or
        $restored.West.tool_bundle.manifest_sha256 -or
        $restored.Other.tool_bundle.bundle -cne 'previous.json' -or
        $restored.Different.tool_bundle.bin_directory -cne '.local/other' -or
        $restored.East.tool_bundle.add_to_path.shell -cne 'bash') {
        throw 'Rollback restored the wrong bindings or approval policies'
    }
    Click 'Close tool rollback'
    Click 'Edit SSH tools for East'
    $null = Wait-Until 'the pending profile draft receives the restored approval and keeps the unrelated edit' {
        $field = Find-Element 'Tool bundle manifest' $edit
        $scrollback = Find-Element 'Scrollback lines' $edit
        $field -and $scrollback -and
            [FixtureDialog]::SameFile($previous, $field.GetCurrentPattern($value).Current.Value) -and
            $scrollback.GetCurrentPattern($value).Current.Value -ceq '8765'
    }
    Click 'Save profile changes'
    $null = Wait-Until 'the open draft saves against the restored profile' {
        $savedEast = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.East
        $savedEast.scrollback_lines -eq 8765 -and $savedEast.tool_bundle.manifest_sha256 -ceq $previousPin
    }
} finally {
    if ($app) { Close-FixtureApp $app }
}
