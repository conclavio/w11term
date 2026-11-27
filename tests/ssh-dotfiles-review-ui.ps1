[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$dot = ' ' + [char]0xb7 + ' '
$bundle = Join-Path $State 'Review shell fragments.json'
$remote = [IO.File]::ReadAllText((Join-Path $State 'dotfiles-remote-folder'))
$remoteStore = Join-Path $remote '.config/w11term/dotfiles'
$account = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.'palette-ssh'
$target = "$($account.user)@$($account.host):$($account.port)"

function Write-Bundle([string]$Prompt) {
    $data = @{ version = 1; environment = 'export EDITOR=vi'; bash = "PS1='$Prompt> '" }
    [IO.File]::WriteAllText($bundle, ($data | ConvertTo-Json))
}
function Click([string]$Name) {
    $search = @{ scroll = $null }
    Invoke-Button (Wait-Until "visible and enabled '$Name'" {
        $control = Find-Element $Name $button
        if ($control -and $control.Current.IsEnabled) {
            if (!$control.Current.IsOffscreen) { return $control }
            Move-NativeControlIntoView $control $search
        }
    })
}
function Save-RetainedDotfiles([string]$Path) {
    $prefix = "Saved dotfile bundle${dot}"
    Click 'Save selected dotfile bundle'
    $dialog = Wait-OwnedDialog 'Save new dotfiles bundle'
    $null = Wait-Until 'enter a draft retained-bundle filename' { [FixtureDialog]::SetCommonDialogFilename($dialog, "$Path.draft") }
    $null = Wait-Until 'choose the retained-bundle filename' { [FixtureDialog]::SetCommonDialogFilename($dialog, $Path) }
    $null = Wait-Until 'save the retained bundle' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'saved bundle is available at the selected Windows destination' {
        if ([IO.File]::Exists($Path)) {
            Get-NativeTextLabels | Where-Object {
                $_.StartsWith($prefix) -and [FixtureDialog]::SameFile($Path, $_.Substring($prefix.Length))
            }
        }
    }
}
function Save-ProfileEdit {
    $saveProfile = Wait-Until 'enabled profile save action' {
        $save = Find-Element 'Save profile changes' $button
        if ($save -and $save.Current.IsEnabled) { $save }
    }
    if ($saveProfile.Current.IsOffscreen) {
        $profileScroll = Find-NativeVerticalScroll $saveProfile
        if ($profileScroll) { $profileScroll.SetScrollPercent(-1, 100) }
    }
    Click 'Save profile changes'
}
function Wait-Review {
    $null = Wait-Until 'dotfiles review is ready on the named account' {
        $apply = Find-Element 'Apply reviewed dotfiles' $button
        (Find-Element $target) -and (Find-Element 'SFTP home /') -and $apply -and $apply.Current.IsEnabled
    }
}
function Expand-ReviewDetail([string]$Name) {
    $search = @{ scroll = $null }
    try {
        $null = Wait-Until "visible '$Name' details" {
            $element = Find-Element $Name $button
            if ($element -and !$element.Current.IsOffscreen) { return $element }
            Move-NativeControlIntoView $element $search
        }
        Set-NativeExpansion $Name $true
    } catch {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $controls = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition) | Where-Object {
                $_.Current.Name -like '*hook*' -or $_.Current.Name -like '*fragment*'
            } | ForEach-Object {
                "$($_.Current.ControlType.ProgrammaticName) [$($_.Current.Name)] offscreen=$($_.Current.IsOffscreen) expand=$($_.GetSupportedPatterns().ProgrammaticName -contains 'ExpandCollapsePatternIdentifiers.Pattern')"
            }
        throw "$_ Review controls $($controls -join ' | ')"
    }
}
function Select-RetainedDotfile([int]$Pane, [string]$Id) {
    $report = (Control @('sftp','--pane',"$Pane",'list-dotfile-versions','--shell','bash')).sftp
    $version = $report.versions | Where-Object bundle_sha256 -ceq $Id
    if (!$version) { throw "Missing retained dotfile version $Id" }
    $label = $Id.Substring(0,12)
    if ($report.active_bundle_sha256 -ceq $Id) { $label += "${dot}Active" }
    if ($null -ne $version.modified) {
        $time = [DateTimeOffset]::FromUnixTimeSeconds($version.modified).UtcDateTime
        $label += $dot + $time.ToString("yyyy-MM-dd HH:mm 'UTC'", [Globalization.CultureInfo]::InvariantCulture)
    }
    $comboType = [System.Windows.Automation.ControlType]::ComboBox
    $itemType = [System.Windows.Automation.ControlType]::ListItem
    $expandPattern = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $selectPattern = [System.Windows.Automation.SelectionItemPattern]::Pattern
    $search = @{ scroll = $null }
    $null = Wait-Until "select retained dotfile '$label'" {
        if (Find-Element "Selected version $Id") { return $true }
        $selector = Find-Element 'Retained dotfile versions' $comboType
        if (!$selector -or !$selector.Current.IsEnabled) { return }
        if ($selector.Current.IsOffscreen) {
            Move-NativeControlIntoView $selector $search
            return
        }
        try {
            $popup = $selector.GetCurrentPattern($expandPattern)
            if ($popup.Current.ExpandCollapseState -eq 'Collapsed') { $popup.Expand() }
            $row = Find-Element $label $itemType -Visible
            if ($row) { $row.GetCurrentPattern($selectPattern).Select() }
        } catch [System.Windows.Automation.ElementNotAvailableException] {
            # Wait for the replacement automation elements after the popup opens.
        }
    }
}

Write-Bundle 'reviewed'
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $localPane = ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id
    Wait-Text $localPane 'PS '
    Run-Palette 'Command reference'
    $null = Wait-Until 'the command reference is open' { Find-Element 'Search command reference' }
    if (Find-Element 'Apply SSH dotfiles' $button) { throw 'The local shell offered remote dotfiles' }

    $pane = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $pane
    $null = Control @('wait-ssh-connected','--pane',"$pane",'--timeout','15')
    Click 'Apply SSH dotfiles'
    Click 'Browse for dotfiles bundle'
    $dialog = Wait-OwnedDialog 'Choose dotfiles bundle'
    $null = Wait-Until 'enter the local bundle' { [FixtureDialog]::SetFilename($dialog, $bundle, '') }
    $null = Wait-Until 'select the local bundle' { [FixtureDialog]::ClickButton($dialog, 1) }
    Click 'Review dotfiles changes'
    Wait-Review
    if (!(Find-Element "Add startup hook${dot}/.bashrc") -or
        !(Find-Element "Create${dot}/.profile") -or [IO.Directory]::Exists($remoteStore)) {
        throw 'The read-only review did not describe the actual startup changes'
    }
    Expand-ReviewDetail "Startup hook${dot}/.bashrc"
    Expand-ReviewDetail 'Prompt fragment changes'
    $null = Wait-Until 'first review shows the new hook and added prompt line' {
        $labels = Get-NativeTextLabels
        ($labels | Where-Object { $_.Contains('# >>> w11term dotfiles >>>') -and $_.Contains('bash-current') }) -and
            ($labels | Where-Object { $_.Contains("Added line 1${dot}PS1='reviewed> '") })
    }
    if (Get-NativeTextLabels | Where-Object { $_.Contains('# keep my local shell settings') }) {
        throw 'The dotfiles review exposed unrelated startup-file contents'
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'the other pane cannot apply this review' {
        $apply = Find-Element 'Apply reviewed dotfiles' $button
        $apply -and !$apply.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$pane")
    Wait-Focus $pane

    Write-Bundle 'verified'
    Click 'Apply reviewed dotfiles'
    $null = Wait-Until 'an edited local bundle requires a fresh review' {
        Get-NativeTextLabels | Where-Object { $_.Contains('The dotfiles review is stale or belongs to another SFTP connection. Preview again.') }
    }
    if ([IO.Directory]::Exists($remoteStore)) { throw 'The stale review wrote remote files' }
    Click 'Review dotfiles changes'
    Wait-Review
    Click 'Apply reviewed dotfiles'
    $null = Wait-Until 'the new bundle and startup changes were applied' {
        Find-Element "Dotfiles applied${dot}5 files changed"
    }

    Click 'Review dotfiles changes'
    Wait-Review
    $null = Wait-Until 'an unchanged hook is shown in the review' {
        Find-Element "Already current${dot}/.bashrc"
    }
    Click 'Apply reviewed dotfiles'
    $null = Wait-Until 'the reviewed reinstall is a no-op' { Find-Element 'Dotfiles are already current' }

    $startup = Join-Path $remote '.bashrc'
    $installed = [IO.File]::ReadAllText($startup) + "# keep my trailing shell settings"
    $selectorPath = Join-Path $remoteStore 'bash-current'
    $beforeHookRepair = [IO.File]::ReadAllText($selectorPath)
    $drift = $installed.Replace('/prompt.sh', '/edited-prompt.sh')
    if ($drift -ceq $installed) { throw 'The installed Bash hook was missing' }
    [IO.File]::WriteAllText($startup, $drift)
    Click 'Review dotfiles changes'
    $null = Wait-Until 'the edited startup hook blocks apply and identifies its file' {
        $apply = Find-Element 'Apply reviewed dotfiles' $button
        $apply -and !$apply.Current.IsEnabled -and (Find-Element "Startup hook conflict${dot}/.bashrc")
    }
    if ([IO.File]::ReadAllText($startup) -cne $drift) { throw 'Reading a conflict replaced the edited startup hook' }
    Click 'Review startup hook repair'
    $null = Wait-Until 'repair review finds only the edited hook' {
        (Find-Element "Replace managed startup hook${dot}/.bashrc") -and
            (Find-Element "Already current${dot}/.profile") -and
            (Find-Element 'Repair reviewed startup hooks' $button -Visible)
    }
    Expand-ReviewDetail "Startup hook changes${dot}/.bashrc"
    $null = Wait-Until 'review shows the edited and replacement hook lines' {
        Get-NativeTextLabels | Where-Object { $_.Contains('/edited-prompt.sh') -and $_.Contains('/prompt.sh') }
    }
    if (Get-NativeTextLabels | Where-Object { $_.Contains('# keep my trailing shell settings') }) {
        throw 'The repair review exposed text outside the managed hook'
    }
    if ([IO.File]::ReadAllText($startup) -cne $drift) { throw 'The repair review wrote the hook' }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'the local pane cannot confirm SSH startup hook changes' {
        $repair = Find-Element 'Repair reviewed startup hooks' $button
        $repair -and !$repair.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$pane")
    Wait-Focus $pane
    Click 'Repair reviewed startup hooks'
    $null = Wait-Until 'repair updates the hook without switching the active bundle' {
        (Find-Element "Startup hooks repaired${dot}1 file changed") -and
            [IO.File]::ReadAllText($startup) -ceq $installed -and
            [IO.File]::ReadAllText($selectorPath) -ceq $beforeHookRepair
    }

    $first = [IO.File]::ReadAllText($selectorPath).Trim()
    Write-Bundle 'second'
    Click 'Review dotfiles changes'
    Wait-Review
    Expand-ReviewDetail 'Prompt fragment changes'
    $null = Wait-Until 'install review compares the active and proposed prompt lines' {
        Get-NativeTextLabels | Where-Object {
            $_.Contains("Removed line 1${dot}PS1='verified> '") -and $_.Contains("Added line 1${dot}PS1='second> '")
        }
    }
    Click 'Apply reviewed dotfiles'
    $null = Wait-Until 'a second version is active' { Find-Element "Dotfiles applied${dot}3 files changed" }
    $second = [IO.File]::ReadAllText($selectorPath).Trim()
    if ($first -ceq $second) { throw 'Installing the second version did not switch the selector' }

    Click 'Dotfile versions'
    $null = Wait-Until 'history shows the current version' { Find-Element "Active version $second" }
    Select-RetainedDotfile $pane $first
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'another pane cannot save the retained dotfiles' {
        $save = Find-Element 'Save selected dotfile bundle' $button
        $save -and !$save.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$pane")
    Wait-Focus $pane
    $snapshot = Join-Path $State ('Retained shell bundle ' + [char]0x96ea + '.json')
    Click 'Save selected dotfile bundle'
    $saveDialog = Wait-OwnedDialog 'Save new dotfiles bundle'
    $null = Wait-Until 'cancel the Windows bundle picker' { [FixtureDialog]::ClickButton($saveDialog, 2) }
    $null = Wait-Until 'cancel returns to the retained version' {
        (Find-Element "Selected version $first") -and !(Find-OwnedDialog 'Save new dotfiles bundle')
    }
    Save-RetainedDotfiles $snapshot
    $saved = [IO.File]::ReadAllText($snapshot) | ConvertFrom-Json
    if ($saved.version -ne 1 -or $saved.bash -cne "PS1='verified> '`n" -or
        $saved.environment -cne "export EDITOR=vi`n" -or $null -ne $saved.zsh) {
        throw 'The saved bundle did not contain the chosen older version'
    }
    $cliSnapshot = Join-Path $State 'CLI saved shell bundle.json'
    $receipt = (Control @('sftp','--pane',"$pane",'export-dotfile-bundle',$second,$cliSnapshot,'--shell','bash')).sftp
    $savedCli = [IO.File]::ReadAllText($cliSnapshot) | ConvertFrom-Json
    if ($receipt.bundle_sha256 -cne $second -or ![FixtureDialog]::SameFile($cliSnapshot, $receipt.local) -or
        $savedCli.bash -cne "PS1='second> '`n" -or [IO.File]::ReadAllText($selectorPath).Trim() -cne $second) {
        throw 'CLI export did not save the expected active bundle'
    }
    Click 'Review dotfile restore'
    $null = Wait-Until 'restore reviews the before and after versions' {
        (Find-Element "Reviewed active version $second") -and (Find-Element "Restore version $first") -and
            (Find-Element 'This choice pauses automatic dotfile sync on reconnect.')
    }
    Expand-ReviewDetail 'Prompt fragment changes'
    $null = Wait-Until 'restore review shows the inverse prompt changes' {
        Get-NativeTextLabels | Where-Object {
            $_.Contains("Removed line 1${dot}PS1='second> '") -and $_.Contains("Added line 1${dot}PS1='verified> '")
        }
    }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $resize = $root.GetCurrentPattern([System.Windows.Automation.TransformPattern]::Pattern)
    $resize.Resize(960, 480)
    try {
        $null = Wait-Until 'restore confirmation and Back stay visible in a short window' {
            $bounds = $root.Current.BoundingRectangle
            $details = Find-Element 'This choice pauses automatic dotfile sync on reconnect.'
            $scroll = if ($details) { Find-NativeVerticalScroll $details }
            $restore = Find-Element 'Restore reviewed dotfiles' $button -Visible
            $back = Find-Element 'Back to dotfile installer' $button -Visible
            $scroll -and $restore -and $back -and
                $bounds.Contains($restore.Current.BoundingRectangle) -and $bounds.Contains($back.Current.BoundingRectangle)
        }
    } catch {
        $bounds = $root.Current.BoundingRectangle
        $restore = Find-Element 'Restore reviewed dotfiles' $button
        $back = Find-Element 'Back to dotfile installer' $button
        throw "$_ Window=$bounds Restore=$($restore.Current.BoundingRectangle) Offscreen=$($restore.Current.IsOffscreen) Back=$($back.Current.BoundingRectangle) Offscreen=$($back.Current.IsOffscreen)"
    }
    if ([IO.File]::ReadAllText($selectorPath).Trim() -cne $second) { throw 'Review changed the selector' }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'another pane cannot restore the reviewed version' {
        $restore = Find-Element 'Restore reviewed dotfiles' $button
        $restore -and !$restore.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$pane")
    Wait-Focus $pane
    Click 'Restore reviewed dotfiles'
    $null = Wait-Until 'the prior version is active and automatic sync is paused' {
        (Find-Element "Active version $first") -and (Find-Element 'Automatic dotfile sync paused')
    }
    if ([IO.File]::ReadAllText($selectorPath) -cne "$first`nmanual`n") { throw 'Restore did not hold the first version' }

    Click 'Review disabling managed dotfiles'
    $null = Wait-Until 'disable confirmation stays visible in the short window' {
        (Find-Element 'Disable managed dotfiles for new shells') -and
            (Find-Element 'Disable reviewed dotfiles' $button -Visible)
    }
    Expand-ReviewDetail 'Prompt fragment changes'
    $null = Wait-Until 'disable review shows the prompt that will stop loading' {
        Get-NativeTextLabels | Where-Object {
            $_.Contains("Removed line 1${dot}PS1='verified> '") -and !$_.Contains('Added line')
        }
    }
    Click 'Disable reviewed dotfiles'
    $null = Wait-Until 'history shows disabled dotfiles' { Find-Element 'Managed dotfiles disabled' }
    if ([IO.File]::ReadAllText($selectorPath) -cne "disabled`n") { throw 'Disable did not switch the selector' }

    Click 'Review resuming automatic dotfile sync'
    $null = Wait-Until 'resumption explains that disabled fragments stay inactive' {
        (Find-Element 'Keep managed fragments inactive until a bundle is installed') -and
            (Find-Element 'Resume automatic dotfile sync' $button -Visible)
    }
    if ([IO.File]::ReadAllText($selectorPath) -cne "disabled`n") { throw 'Review changed the disabled selector' }
    Click 'Resume automatic dotfile sync'
    $null = Wait-Until 'no version is active while the profile has yet to sync' {
        Find-Element 'Waiting for an approved profile to sync managed dotfiles'
    }
    if ([IO.File]::ReadAllText($selectorPath) -cne "automatic`n") { throw 'Resuming activated a bundle immediately' }
    Click 'Review disabling managed dotfiles'
    Click 'Disable reviewed dotfiles'
    $null = Wait-Until 'a waiting sync can be disabled again' { Find-Element 'Managed dotfiles disabled' }
    if ([IO.File]::ReadAllText($selectorPath) -cne "disabled`n") { throw 'Disable did not restore the hold' }

    Select-RetainedDotfile $pane $first
    Click 'Review dotfile restore'
    Click 'Restore reviewed dotfiles'
    $null = Wait-Until 'a retained version restores from the disabled state' {
        (Find-Element "Active version $first") -and (Find-Element 'Automatic dotfile sync paused')
    }
    Click 'Review resuming automatic dotfile sync'
    $null = Wait-Until 'resumption keeps the current version active' {
        (Find-Element "Reviewed active version $first") -and (Find-Element 'Keep the reviewed version active') -and
            (Find-Element 'Approved profiles can sync on their next SSH connection.')
    }
    if ([IO.File]::ReadAllText($selectorPath) -cne "$first`nmanual`n") { throw 'Review changed the held selector' }
    Click 'Resume automatic dotfile sync'
    $null = Wait-Until 'automatic sync is resumed without changing the active version' {
        (Find-Element "Active version $first") -and (Find-Element 'Managed dotfiles active') -and
            (Find-Element 'Automatic dotfile sync resumed')
    }
    if ([IO.File]::ReadAllText($selectorPath) -cne "$first`n") { throw 'Resuming changed the active version or left it held' }
    if ([IO.File]::ReadAllText($startup) -cne $installed) { throw 'Switching versions changed the startup hooks' }

    Select-RetainedDotfile $pane $first
    $profileSnapshot = Join-Path $State 'Profile dotfile snapshot.json'
    Save-RetainedDotfiles $profileSnapshot
    $beforeProfile = [IO.File]::ReadAllText($profiles)
    $profileAction = 'Review saved dotfiles in this SSH profile'
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'a different pane cannot use the saved dotfiles' {
        $review = Find-Element $profileAction $button
        $review -and !$review.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$pane")
    Wait-Focus $pane
    Click $profileAction
    $edit = [System.Windows.Automation.ControlType]::Edit
    $value = [System.Windows.Automation.ValuePattern]::Pattern
    $combo = [System.Windows.Automation.ControlType]::ComboBox
    $selection = [System.Windows.Automation.SelectionPattern]::Pattern
    $null = Wait-Until 'review shows the exported version and authenticated profile account' {
        $bundleField = Find-Element 'Dotfiles bundle' $edit
        $homeField = Find-Element 'Remote SFTP home' $edit
        $keyField = Find-Element 'Dotfiles host-key fingerprint' $edit
        $shell = Find-Element 'Dotfiles shell' $combo
        if (!$bundleField -or !$homeField -or !$keyField -or !$shell) { return }
        $chosen = $shell.GetCurrentPattern($selection).Current.GetSelection()
        (Find-Element "Saved dotfile version $first") -and (Find-Element "Upload on connect to $target") -and
            [FixtureDialog]::SameFile($profileSnapshot, $bundleField.GetCurrentPattern($value).Current.Value) -and
            $homeField.GetCurrentPattern($value).Current.Value -ceq '/' -and
            $keyField.GetCurrentPattern($value).Current.Value -ceq $account.host_key -and
            $chosen.Count -eq 1 -and $chosen[0].Current.Name -ceq 'Bash'
    }
    $pinLabel = 'Pin approved dotfile version'
    $null = Wait-CheckBox $pinLabel ([System.Windows.Automation.ToggleState]::On)
    $originalSnapshot = [IO.File]::ReadAllText($profileSnapshot)
    $editedSnapshot = $originalSnapshot.Replace('verified> ', 'modified> ')
    if ($editedSnapshot -ceq $originalSnapshot) { throw 'The saved prompt was missing from the fixture' }
    [IO.File]::WriteAllText($profileSnapshot, $editedSnapshot)
    Click 'Approve dotfiles sync'
    $null = Wait-Until 'approval refuses an edited snapshot' {
        Get-NativeTextLabels | Where-Object { $_.Contains("does not reproduce version $first") }
    }
    if ([IO.File]::ReadAllText($profiles) -cne $beforeProfile) { throw 'Opening the review changed the saved profile' }
    [IO.File]::WriteAllText($profileSnapshot, $originalSnapshot)
    Click 'Approve dotfiles sync'
    $null = Wait-Until 'the exact saved version is pinned in the draft' {
        (Find-Element "Approved for ${target}${dot}/") -and (Find-Element "Pinned dotfile version $first")
    }
    if ([IO.File]::ReadAllText($profiles) -cne $beforeProfile) { throw 'Approval saved the profile before the user confirmed it' }
    Save-ProfileEdit
    $null = Wait-Until 'the exported manifest becomes the SSH profile dotfiles bundle' {
        $approved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.'palette-ssh'.dotfiles
        $approved -and [FixtureDialog]::SameFile($profileSnapshot, $approved.bundle) -and
            $approved.shell -ceq 'bash' -and $approved.bundle_sha256 -ceq $first -and $approved.home -ceq '/' -and
            $approved.host -ceq $account.host -and $approved.user -ceq $account.user -and
            $approved.port -eq $account.port -and $approved.fingerprint -ceq $account.host_key
    }
    (Wait-CheckBox $pinLabel ([System.Windows.Automation.ToggleState]::On)).Toggle()
    $null = Wait-CheckBox $pinLabel ([System.Windows.Automation.ToggleState]::Off)
    $null = Wait-Until 'clearing the pin requires approval' {
        Find-Element 'Approve the edited dotfiles settings to update the saved approval'
    }
    $savedProfile = [IO.File]::ReadAllText($profiles)
    Click 'Approve dotfiles sync'
    $null = Wait-Until 'approval follows the bundle without a pin' {
        (Find-Element "Approved for ${target}${dot}/") -and !(Find-Element "Pinned dotfile version $first")
    }
    if ([IO.File]::ReadAllText($profiles) -cne $savedProfile) { throw 'Unpinning saved the profile before confirmation' }
    Save-ProfileEdit
    $null = Wait-Until 'the profile continues to approve edits to the same bundle' {
        $approved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.'palette-ssh'.dotfiles
        $approved -and [FixtureDialog]::SameFile($profileSnapshot, $approved.bundle) -and
            !$approved.PSObject.Properties['bundle_sha256']
    }
    if ([IO.File]::ReadAllText($selectorPath) -cne "$first`n") { throw 'Profile review changed the active remote version' }
} finally {
    if ($app) { Close-FixtureApp $app }
}
