[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$verifyClipboard = $env:GITHUB_ACTIONS -ceq 'true' -and $env:RUNNER_ENVIRONMENT -ceq 'github-hosted'
if ($verifyClipboard) {
    . (Join-Path $PSScriptRoot 'support/clipboard.ps1')
} else {
    $env:W11TERM_TEST_COPY_FILE = Join-Path $State 'copied-manifest-path.txt'
}

$edit = [System.Windows.Automation.ControlType]::Edit
$combo = [System.Windows.Automation.ControlType]::ComboBox
$selection = [System.Windows.Automation.SelectionPattern]::Pattern
$value = [System.Windows.Automation.ValuePattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '
$wire = [IO.File]::ReadAllText((Join-Path $State 'ssh-wire'))
$privatePages = Join-Path $env:W11TERM_DATA_DIR 'command-reference/pages/linux'
$null = [IO.Directory]::CreateDirectory($privatePages)
[IO.File]::WriteAllText((Join-Path $privatePages 'rg.md'), @'
# Team rg notes

> Search code in the team workspace.

- Search source:

`rg {{pattern}} src`
'@)

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $button
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}
function Set-Field([string]$Name, [string]$Text) {
    $field = Wait-Until "'$Name' field" { Find-Element $Name $edit }
    $field.SetFocus()
    $field = Wait-Until "'$Name' editor focused" {
        $current = Find-Element $Name $edit -Visible
        if ($current -and $current.Current.HasKeyboardFocus) { $current }
    }
    $field.GetCurrentPattern($value).SetValue($Text)
    $null = Wait-Until "'$Name' contains the entered value" {
        (Find-Element $Name $edit).GetCurrentPattern($value).Current.Value -ceq $Text
    }
}
function Search([string]$Command, [string]$Platform) {
    Set-Field 'Search command reference' $Command
    $null = Wait-Until "$Command page for $Platform" {
        (Find-Element $Command) -and (Find-Element "$Platform${dot}tldr-pages (CC BY 4.0)")
    }
}
function Wait-Platform([string]$Name) {
    $null = Wait-Until "'$Name' platform selection" {
        $selector = Find-Element 'Command reference platform' $combo
        if ($selector) {
            $choice = $selector.GetCurrentPattern($selection).Current.GetSelection()
            $choice.Count -eq 1 -and $choice[0].Current.Name -ceq $Name
        }
    }
}
function Select-InstalledTool([string]$Name) {
    Click 'Installed SSH tools'
    Select-NativeComboItem 'Installed SSH commands' "${Name}${dot}portable"
}
function Select-ToolVersion([string]$Id, [string]$Suffix = '') {
    $label = "portable${dot}$($Id.Substring(0,12))$Suffix"
    $expandPattern = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $selectPattern = [System.Windows.Automation.SelectionItemPattern]::Pattern
    $null = Wait-Until "select retained bundle '$label'" {
        if (Find-Element "SHA-256 $Id") { return $true }
        $history = Find-Element 'Retained tool bundle versions' $combo
        if ($history -and $history.Current.IsEnabled) {
            try {
                $popup = $history.GetCurrentPattern($expandPattern)
                if ($popup.Current.ExpandCollapseState -eq [System.Windows.Automation.ExpandCollapseState]::Collapsed) {
                    $history.SetFocus()
                    $popup.Expand()
                } else {
                    $choice = Find-Element $label ([System.Windows.Automation.ControlType]::ListItem)
                    if ($choice) { $choice.GetCurrentPattern($selectPattern).Select() }
                }
            } catch [System.Windows.Automation.ElementNotAvailableException] {
                # WinUI replaces the dropdown's automation elements when it opens.
            }
        }
    }
}
function Set-Manual([string]$Text) {
    $field = Wait-Until 'editable private Markdown box' {
        $control = Find-Element 'Private page Markdown' $edit
        if ($control -and $control.Current.IsEnabled) { $control }
    }
    $field.SetFocus()
    $markdownLf = $Text.Replace("`r`n", "`n").Replace("`r", "`n")
    $field.GetCurrentPattern($value).SetValue($markdownLf.Replace("`n", "`r"))
    $null = Wait-Until 'complete private Markdown in the editor' {
        $actual = (Find-Element 'Private page Markdown' $edit).GetCurrentPattern($value).Current.Value
        $actual.Replace("`r`n", "`n").Replace("`r", "`n") -ceq $markdownLf
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $localPane = ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id
    Wait-Text $localPane 'PS '
    Run-Palette 'Command reference'
    $null = Wait-Until 'offline TLDR catalogue' { Find-Element "Bundled TLDR pages${dot}v2.3" }
    $null = Wait-Until 'reference search has keyboard focus' {
        $field = Find-Element 'Search command reference' $edit
        $field -and $field.Current.HasKeyboardFocus
    }
    Wait-Platform 'Auto (Windows)'
    if (Find-Element 'Installed SSH tools' $button) { throw 'A PowerShell pane offered SSH tool lookup' }
    Search 'rg' 'Common'
    Set-Field 'Search command reference' 'comparative benchmark'
    $null = Wait-Until 'task search finds the offline hyperfine page' {
        (Find-Element 'hyperfine') -and (Find-Element "Common${dot}tldr-pages (CC BY 4.0)")
    }
    Set-Field 'Search command reference' 'mddog'
    $null = Wait-Until 'project name finds the bundled mdd manual offline' {
        (Find-Element 'mdd') -and (Find-Element "Linux${dot}w11term manual")
    }
    Search 'Get-ChildItem' 'Windows'
    Click 'Edit example 1'
    $null = Wait-Until 'PowerShell command draft' {
        $field = Find-Element 'Edit command example' $edit
        $field -and $field.GetCurrentPattern($value).Current.Value -ceq 'Get-ChildItem'
    }

    $remotePane = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $remotePane
    $null = Control @('wait-ssh-connected','--pane',"$remotePane",'--timeout','15')
    Wait-Platform 'Auto (Linux)'
    $loginSummary = "Last login sync${dot}portable installed"
    $null = Wait-Until 'last SSH tool sync shown in command reference' { Find-Element $loginSummary }
    $null = Wait-Until 'example from the other pane cannot be inserted' {
        $insert = Find-Element 'Insert into terminal' $button
        !$insert -or !$insert.Current.IsEnabled
    }
    Select-InstalledTool 'rg'
    $null = Wait-Until 'installed command opened its private Linux reference' {
        (Find-Element 'Team rg notes') -and (Find-Element "Linux${dot}Custom page") -and
            (Find-Element "Managed SSH launcher${dot}portable") -and (Find-Element '/.local/bin/rg')
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    Wait-Platform 'Auto (Windows)'
    $null = Wait-Until 'SSH launcher hidden on the PowerShell pane' {
        (Find-Element 'rg') -and !(Find-Element '/.local/bin/rg')
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    $null = Wait-Until 'launcher returns on the same SSH connection' {
        (Find-Element 'Team rg notes') -and (Find-Element '/.local/bin/rg')
    }
    Select-InstalledTool 'mdd'
    $null = Wait-Until 'installed mdd opened its bundled manual' {
        (Find-Element 'mdd') -and (Find-Element "Linux${dot}w11term manual") -and
            (Find-Element '/.local/bin/mdd') -and !(Find-Element '/.local/bin/rg')
    }
    Click 'Install SSH tools'
    $null = Wait-Until 'configured profile manifest offered' { Find-Element (Join-Path $State 'fleet.json') }
    $null = Wait-Until 'tool installation and blocked PATH are reported separately' {
        (Find-Element $loginSummary) -and (Find-Element 'Managed PATH skipped') -and
            (Find-Element 'Resolve the managed PATH conflict at /.bashrc')
    }
    Click 'Review PATH repair'
    $null = Wait-Until 'reviewed repair identifies the edited startup block' {
        (Find-Element "Replace managed block in${dot}/.bashrc") -and
            (Find-Element 'The repaired PATH applies to new shells.')
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'another pane cannot apply the PATH repair' {
        !(Find-Element 'Apply reviewed PATH repair' $button)
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click 'Apply reviewed PATH repair'
    $null = Wait-Until 'PATH repair completed without reconnecting' {
        Find-Element 'Managed PATH repaired for new shells'
    }
    Click 'Browse for tool bundle'
    $manifestDialog = Wait-OwnedDialog 'Choose SSH tool bundle'
    $manifestPath = Join-Path $State 'Tool review.json'
    $null = Wait-Until 'enter the full update manifest path' { [FixtureDialog]::SetFilename($manifestDialog, $manifestPath, '') }
    $null = Wait-Until 'select the update manifest' { [FixtureDialog]::ClickButton($manifestDialog, 1) }
    $null = Wait-Until 'close the update manifest picker' { !(Find-OwnedDialog 'Choose SSH tool bundle') }
    $manifestId = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'ssh-tool-install-manifest')
    $null = Wait-Until 'chosen update manifest shown' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $selected = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $manifestId)
        if ($selected) { [FixtureDialog]::SameFile($manifestPath, $selected.Current.Name) }
    }
    Click 'Review bundle'
    $null = Wait-Until 'existing launcher blocks installation' {
        $install = Find-Element 'Install reviewed bundle' $button
        (Find-Element "netwatch${dot}Existing entry at /.local/bin/netwatch") -and !$install.Current.IsEnabled
    }
    $conflict = [IO.File]::ReadAllText((Join-Path $State 'tool-launcher-conflict'))
    if ([IO.File]::ReadAllText($conflict) -cne 'existing fixture command') { throw 'Review changed an existing command' }
    $remoteLocal = Split-Path -Parent (Split-Path -Parent $conflict)
    $selectorPath = Join-Path $remoteLocal 'share/w11term/tools/portable/current'
    $oldId = ([IO.File]::ReadAllText($selectorPath).Trim() -split '/')[-1]
    [IO.File]::Delete($conflict)
    Click 'Review bundle'
    $null = Wait-Until 'installation plan shows the new launcher' { Find-Element "netwatch${dot}Create /.local/bin/netwatch" }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'the other pane cannot apply the original review' {
        $install = Find-Element 'Install reviewed bundle' $button
        $install -and !$install.Current.IsEnabled -and !(Find-Element $loginSummary)
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click 'Install reviewed bundle'
    $null = Wait-Until 'installation completed' { Find-Element 'Installed tool bundle portable' }
    Click 'Back to command reference'
    Select-InstalledTool 'netwatch'
    $null = Wait-Until 'newly installed tool is visible on the same pane' {
        (Find-Element 'netwatch') -and (Find-Element "Linux${dot}w11term manual") -and
            (Find-Element '/.local/bin/netwatch')
    }
    Click 'Install SSH tools'
    Click 'Bundle versions'
    Select-ToolVersion $oldId
    $beforeRollbackReview = [IO.File]::ReadAllText($selectorPath)
    Click 'Review rollback'
    $priorManifest = [IO.File]::ReadAllText((Join-Path $State 'fleet.json')) | ConvertFrom-Json
    $activeManifest = [IO.File]::ReadAllText($manifestPath) | ConvertFrom-Json
    $oldPin = ($priorManifest.tools | Where-Object name -ceq 'rg').builds[0].sha256
    $activePin = ($activeManifest.tools | Where-Object name -ceq 'rg').builds[0].sha256
    $null = Wait-Until 'rollback shows the prior contents and update policy' {
        (Find-Element 'Changes from the active release') -and
        (Find-Element 'Keep mdd') -and
        (Find-Element 'Replace pinned build for rg') -and
        (Find-Element "Active SHA-256 $activePin${dot}256 bytes") -and
        (Find-Element "Restored SHA-256 $oldPin${dot}256 bytes") -and
        (Find-Element 'Deactivate netwatch') -and
        (Find-Element 'Restoring this version pauses automatic bundle updates.') -and
        (Find-Element 'Restore reviewed version' $button -Visible)
    }
    if ([IO.File]::ReadAllText($selectorPath) -cne $beforeRollbackReview) {
        throw 'Review switched the active release before approval'
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'the other pane cannot restore the retained bundle' {
        $restore = Find-Element 'Restore reviewed version' $button
        $restore -and !$restore.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click 'Restore reviewed version'
    $null = Wait-Until 'rollback restored and pinned the original version' {
        (Find-Element 'Restored tool bundle portable') -and
        [IO.File]::ReadAllText($selectorPath).Contains("./versions/$oldId")
    }
    $held = [IO.File]::ReadAllText($selectorPath)
    $discarded = ($beforeRollbackReview.TrimEnd() -split '/')[-1]
    $discardedDir = Join-Path (Split-Path -Parent $selectorPath) "versions/$discarded"
    $files = @(Get-ChildItem -LiteralPath $discardedDir -File -Force)
    $bytes = [long]0
    foreach ($file in $files) { $bytes += $file.Length }
    if (Find-Element 'Review release deletion' $button) { throw 'An active release offered deletion' }
    Select-ToolVersion $discarded
    Click 'Review release deletion'
    $null = Wait-Until 'deletion review lists the files and their total size' {
        (Find-Element "Files to remove $($files.Count)${dot}$bytes bytes") -and
            (Find-Element 'Deleting this release permanently removes it from rollback history.') -and
            (Find-Element 'Delete reviewed release' $button -Visible)
    }
    if (!(Test-Path -LiteralPath $discardedDir) -or [IO.File]::ReadAllText($selectorPath) -cne $held) {
        throw 'The deletion review changed the installed bundle'
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'another pane cannot confirm release deletion' {
        $delete = Find-Element 'Delete reviewed release' $button
        $delete -and !$delete.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click 'Delete reviewed release'
    $null = Wait-Until 'the inactive release is deleted and the active selector stays pinned' {
        (Find-Element "Deleted retained release${dot}$bytes bytes") -and
            !(Test-Path -LiteralPath $discardedDir) -and [IO.File]::ReadAllText($selectorPath) -ceq $held
    }
    Click 'Back to tool installer'
    $null = Wait-Until 'original pinned manifest offered' { Find-Element (Join-Path $State 'fleet.json') }
    $installedRg = Join-Path (Split-Path -Parent $selectorPath) "versions/$oldId/rg"
    $originalRg = [IO.File]::ReadAllBytes($installedRg)
    $damagedRg = [byte[]]$originalRg.Clone()
    $damagedRg[-1] = $damagedRg[-1] -bxor 255
    [IO.File]::WriteAllBytes($installedRg, $damagedRg)
    Click 'Review bundle'
    $null = Wait-Until 'repair review offers the exact pinned build' {
        (Get-NativeTextLabels | Where-Object {
            $_.StartsWith("Replace changed executable${dot}rg") -and $_.Contains("Pinned SHA-256 $oldPin")
        }) -and (Find-Element 'Automatic sync stays paused for this bundle.')
    }
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($installedRg)) -cne [Convert]::ToBase64String($damagedRg)) {
        throw 'Review replaced the executable before approval'
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'another pane cannot confirm executable repair' {
        $repair = Find-Element 'Repair from reviewed bundle' $button
        $repair -and !$repair.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click 'Repair from reviewed bundle'
    $null = Wait-Until 'repair restored the exact bytes and kept the active selector paused' {
        (Get-NativeTextLabels | Where-Object { $_.StartsWith("Repaired tool bundle portable${dot}rg") }) -and
            [Convert]::ToBase64String([IO.File]::ReadAllBytes($installedRg)) -ceq [Convert]::ToBase64String($originalRg) -and
            [IO.File]::ReadAllText($selectorPath) -ceq $held
    }
    Click 'Bundle versions'
    Select-ToolVersion $oldId "${dot}Active${dot}Updates paused"
    $beforeSave = [IO.File]::ReadAllText($profiles)
    $snapshotRoot = Join-Path $State 'tool-bundle-snapshots'
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'another pane cannot save the active bundle' {
        $save = Find-Element 'Save pinned bundle on Windows' $button
        $save -and !$save.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click 'Save pinned bundle on Windows'
    $observed = @{ ManifestCount = 0; CopyEnabled = $false }
    try {
        $manifest = Wait-Until 'saved snapshot and its enabled copy action' {
            $copy = Find-Element 'Copy saved manifest path' $button
            $observed.CopyEnabled = $copy -and $copy.Current.IsEnabled
            if ([IO.Directory]::Exists($snapshotRoot)) {
                $paths = @(Get-ChildItem -LiteralPath $snapshotRoot -Filter 'bundle.json' -File -Recurse)
                $observed.ManifestCount = $paths.Count
                if ($paths.Count -eq 1 -and $observed.CopyEnabled) { $paths[0].FullName }
            }
        }
    } catch {
        $labels = (Get-NativeTextLabels) -join ' | '
        throw "$_ Snapshot root [$snapshotRoot], manifests $($observed.ManifestCount), copy enabled $($observed.CopyEnabled). Visible text: $labels"
    }
    if ([IO.File]::ReadAllText($profiles) -cne $beforeSave -or [IO.File]::ReadAllText($selectorPath) -cne $held) {
        throw 'Saving the bundle changed the profile or resumed automatic updates'
    }
    if ($verifyClipboard) {
        $clipBefore = [FixtureClipboard]::GetClipboardSequenceNumber()
    }
    Click 'Copy saved manifest path'
    $null = Wait-Until 'manifest path copied' {
        $copied = if ($verifyClipboard) {
            [FixtureClipboard]::ReadNewText($clipBefore)
        } else {
            Read-ClipboardFixture
        }
        $copied -and [FixtureDialog]::SameFile($manifest, $copied)
    }
    Click 'Review update resumption'
    $null = Wait-Until 'review explains when automatic updates run' {
        Find-Element 'Approved SSH profiles can sync this bundle on their next connection.'
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'another pane cannot resume updates' {
        $resume = Find-Element 'Resume automatic updates' $button
        $reviewProfile = Find-Element 'Review in this SSH profile' $button
        $resume -and !$resume.Current.IsEnabled -and $reviewProfile -and !$reviewProfile.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click 'Resume automatic updates'
    $null = Wait-Until 'automatic updates resumed for the same active version' {
        (Find-Element 'Resumed automatic updates for portable') -and
        [IO.File]::ReadAllText($selectorPath).TrimEnd().EndsWith("`nversions/$oldId")
    }
    $unpaused = [IO.File]::ReadAllText($selectorPath)
    Click 'Review update pause'
    $null = Wait-Until 'pause review explains that the current release stays installed' {
        Find-Element 'Keep this release installed and skip this bundle on future SSH logins.'
    }
    if ([IO.File]::ReadAllText($selectorPath) -cne $unpaused) { throw 'Review paused the bundle before approval' }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'another pane cannot pause automatic updates' {
        $pause = Find-Element 'Pause automatic updates' $button
        $pause -and !$pause.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Click 'Pause automatic updates'
    $null = Wait-Until 'automatic updates paused for the same active version' {
        (Find-Element 'Paused automatic updates for portable') -and
        [IO.File]::ReadAllText($selectorPath).TrimEnd().EndsWith("`n./versions/$oldId")
    }
    Click 'Review update resumption'
    Click 'Resume automatic updates'
    $null = Wait-Until 'resuming returns the same release to automatic updates' {
        (Find-Element 'Resumed automatic updates for portable') -and
        [IO.File]::ReadAllText($selectorPath) -ceq $unpaused
    }
    $originalProfile = ($beforeSave | ConvertFrom-Json).profiles.'palette-ssh'
    Click 'Review in this SSH profile'
    $null = Wait-Until 'profile reviews the saved manifest for the authenticated SSH account' {
        $bundleField = Find-Element 'Tool bundle manifest' $edit
        $homeField = Find-Element 'Tool bundle SFTP home' $edit
        $fingerprintField = Find-Element 'Tool bundle host-key fingerprint' $edit
        $approve = Find-Element 'Approve tool sync' $button
        $bundleField -and $homeField -and $fingerprintField -and $approve -and $approve.Current.IsEnabled -and
            [FixtureDialog]::SameFile($manifest, $bundleField.GetCurrentPattern($value).Current.Value) -and
            $homeField.GetCurrentPattern($value).Current.Value -ceq '/' -and
            $fingerprintField.GetCurrentPattern($value).Current.Value -ceq $originalProfile.host_key
    }
    if ([IO.File]::ReadAllText($profiles) -cne $beforeSave) { throw 'Opening review saved the draft automatically' }
    Click 'Approve tool sync'
    $null = Wait-Until 'snapshot approved in the editor' { Find-Element 'Approved tool bundle: portable' }
    if ([IO.File]::ReadAllText($profiles) -cne $beforeSave) { throw 'Approving the draft persisted unsaved changes' }
    Click 'Save profile changes'
    $null = Wait-Until 'snapshot becomes the saved profile bundle and retains the PATH preference' {
        $scope = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.'palette-ssh'.tool_bundle
        [FixtureDialog]::SameFile($manifest, $scope.bundle) -and $scope.name -ceq 'portable' -and $scope.home -ceq '/' -and
            $scope.fingerprint -ceq $originalProfile.host_key -and $scope.add_to_path.shell -ceq 'bash'
    }
    $null = Control @('focus-pane','--pane',"$remotePane")
    Wait-Focus $remotePane
    Run-Palette 'Command reference'
    $null = Wait-Until 'rollback invalidates the previously seen launcher' {
        (Find-Element 'netwatch') -and !(Find-Element '/.local/bin/netwatch')
    }
    Set-Field 'Search command reference' 'crew report'
    Click 'New private page'
    $manual = @'
# Crew runbook

> Check the crew rotation.

- Show report:

`crew-report --summary`
'@
    Set-Manual $manual
    Click 'Save private page'
    $null = Wait-Until 'saved manual shown immediately' {
        (Find-Element 'Crew runbook') -and (Find-Element 'crew-report --summary')
    }
    $manualFile = Join-Path $privatePages 'crew-report.md'
    if (![IO.File]::Exists($manualFile)) { throw 'Private manual was saved in the wrong platform directory' }
    Click 'Edit private page'
    $null = Wait-Until 'existing multiline page seeded without losing the example' {
        $field = Find-Element 'Private page Markdown' $edit
        $field -and $field.Current.IsEnabled -and $field.GetCurrentPattern($value).Current.Value.Contains('crew-report --summary')
    }
    Set-Manual ($manual.Replace('Crew runbook', 'Crew runbook updated').Replace('--summary','--details'))
    Click 'Save private page'
    $null = Wait-Until 'edited manual shown' {
        (Find-Element 'Crew runbook updated') -and (Find-Element 'crew-report --details')
    }
    $saved = [IO.File]::ReadAllText($manualFile)
    Click 'Edit private page'
    Set-Manual ($manual.Replace('Crew runbook','Discarded draft'))
    Click 'Cancel private page edit'
    $null = Wait-Until 'cancel returns to the published page' { Find-Element 'Crew runbook updated' }
    if ([IO.File]::ReadAllText($manualFile) -cne $saved) { throw 'Cancel changed the saved file' }
    Click 'Close command reference'
    $null = Wait-Until 'closed command reference before reload' { !(Find-Element 'Search command reference' $edit) }
    Run-Palette 'Command reference'
    $null = Wait-Until 'the offline catalogue displays the edited page after reopening' { Find-Element 'Crew runbook updated' }
    Search 'ip' 'Linux'
    Set-Field 'Search command reference' 'crew rotation'
    $null = Wait-Until 'task search finds the private Linux manual by description' {
        (Find-Element 'Crew runbook updated') -and (Find-Element "Linux${dot}Custom page")
    }
    if ([IO.File]::ReadAllText($wire)) { throw 'Browsing or editing a manual wrote to the SSH shell' }
    Search 'ip' 'Linux'
    Click 'Edit example 1'
    Set-Field 'Edit command example' 'ip address'
    Click 'Insert into terminal'
    $null = Wait-Until 'edited example reaches SSH' {
        try { [IO.File]::ReadAllText($wire) -ceq 'ip address' } catch [IO.IOException] { $false }
    }

    $selector = Find-Element 'Command reference platform' $combo
    $selector.SetFocus()
    $popup = $selector.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
    $popup.Expand()
    $choice = Wait-Until 'visible Windows override' {
        Find-Element 'Windows' ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
    Wait-Platform 'Windows'
    Search 'Get-ChildItem' 'Windows'
    Click 'Close command reference'
    $null = Wait-Until 'close command reference' { !(Find-Element 'Search command reference' $edit) }
    Wait-Focus $remotePane
    $reconnected = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Text $reconnected 'tools-at-shell:'
    $sync = (Control @('events','--pane',"$reconnected",'--kind','tool-bundle-sync')).journal.events
    if ($sync.Count -ne 1 -or $sync[0].data.status -cne 'current' -or $sync[0].data.path -cne 'current' -or
        ![IO.File]::ReadAllText($selectorPath).TrimEnd().EndsWith("`nversions/$oldId")) {
        throw "Unexpected pinned bundle sync on a new login: $($sync | ConvertTo-Json -Depth 5 -Compress)"
    }
    Wait-Focus $reconnected
    Run-Palette 'Command reference'
    Select-InstalledTool 'mdd'
    Click 'Review removing mdd from portable'
    $beforeRemoval = [IO.File]::ReadAllText($selectorPath)
    $null = Wait-Until 'removal identifies the launcher and retained rg pin' {
        $labels = Get-NativeTextLabels
        $apply = Find-Element 'Remove reviewed SSH tool' $button
        (Find-Element 'Remove mdd from portable') -and
            ($labels | Where-Object { $_.StartsWith("Managed launcher becomes inactive`n/.local/bin/mdd") }) -and
            ($labels | Where-Object { $_.StartsWith("Keep rg`nSHA-256 ") }) -and $apply -and $apply.Current.IsEnabled
    }
    if ([IO.File]::ReadAllText($selectorPath) -cne $beforeRemoval) { throw 'Review changed the active release' }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'another pane cannot remove the reviewed tool' {
        $apply = Find-Element 'Remove reviewed SSH tool' $button
        $apply -and !$apply.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$reconnected")
    Wait-Focus $reconnected
    Click 'Remove reviewed SSH tool'
    $null = Wait-Until 'mdd is removed from the active release and bundle updates are paused' {
        (Find-Element 'Removed mdd from portable') -and
            [IO.File]::ReadAllText($selectorPath).TrimEnd().Split("`n")[-1].StartsWith('./versions/')
    }
    Click 'Back to command reference'
    Select-InstalledTool 'rg'
    $null = Wait-Until 'rg remains available after the removal' { Find-Element '/.local/bin/rg' }
    Click 'Close command reference'
    $afterRemoval = [IO.File]::ReadAllText($selectorPath)
    $nextLogin = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Text $nextLogin 'tools-at-shell:'
    $sync = (Control @('events','--pane',"$nextLogin",'--kind','tool-bundle-sync')).journal.events
    if ($sync.Count -ne 1 -or $sync[0].data.status -cne 'held' -or
        [IO.File]::ReadAllText($selectorPath) -cne $afterRemoval) {
        throw "Login must preserve the removal even when the saved profile still includes mdd: $($sync | ConvertTo-Json -Depth 5 -Compress)"
    }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
