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
function Wait-Review {
    $null = Wait-Until 'dotfiles review is ready on the named account' {
        $apply = Find-Element 'Apply reviewed dotfiles' $button
        (Find-Element $target) -and (Find-Element 'SFTP home /') -and $apply -and $apply.Current.IsEnabled
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
    $dialog = Wait-Until 'owned local dotfiles bundle picker' { Find-OwnedDialog 'Choose dotfiles bundle' }
    $null = Wait-Until 'enter the local bundle' { [FixtureDialog]::SetFilename($dialog, $bundle, '') }
    $null = Wait-Until 'select the local bundle' { [FixtureDialog]::ClickButton($dialog, 1) }
    Click 'Review dotfiles changes'
    Wait-Review
    if (!(Find-Element "Add startup hook${dot}/.bashrc") -or
        !(Find-Element "Create${dot}/.profile") -or [IO.Directory]::Exists($remoteStore)) {
        throw 'The read-only review did not describe the actual startup changes'
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
    $installed = [IO.File]::ReadAllText($startup)
    $drift = $installed.Replace('/prompt.sh', '/edited-prompt.sh')
    if ($drift -ceq $installed) { throw 'The installed Bash hook was missing' }
    [IO.File]::WriteAllText($startup, $drift)
    Click 'Review dotfiles changes'
    $null = Wait-Until 'the edited startup hook blocks apply and identifies its file' {
        $apply = Find-Element 'Apply reviewed dotfiles' $button
        $apply -and !$apply.Current.IsEnabled -and (Find-Element "Startup hook conflict${dot}/.bashrc")
    }
    if ([IO.File]::ReadAllText($startup) -cne $drift) { throw 'Reading a conflict replaced the edited startup hook' }
    [IO.File]::WriteAllText($startup, $installed)

    $selectorPath = Join-Path $remoteStore 'bash-current'
    $first = [IO.File]::ReadAllText($selectorPath).Trim()
    Write-Bundle 'second'
    Click 'Review dotfiles changes'
    Wait-Review
    Click 'Apply reviewed dotfiles'
    $null = Wait-Until 'a second version is active' { Find-Element "Dotfiles applied${dot}3 files changed" }
    $second = [IO.File]::ReadAllText($selectorPath).Trim()
    if ($first -ceq $second) { throw 'Installing the second version did not switch the selector' }

    Click 'Dotfile versions'
    $null = Wait-Until 'history shows the current version' { Find-Element "Active version $second" }
    Select-RetainedDotfile $pane $first
    Click 'Review dotfile restore'
    $null = Wait-Until 'restore reviews the before and after versions' {
        (Find-Element "Reviewed active version $second") -and (Find-Element "Restore version $first") -and
            (Find-Element 'This choice pauses automatic dotfile sync on reconnect.')
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
    $null = Wait-Until 'disabling can be confirmed' { Find-Element 'Disable managed dotfiles for new shells' }
    Click 'Disable reviewed dotfiles'
    $null = Wait-Until 'history shows disabled dotfiles' { Find-Element 'Managed dotfiles disabled' }
    if ([IO.File]::ReadAllText($selectorPath) -cne "disabled`n") { throw 'Disable did not switch the selector' }

    Click 'Review resuming automatic dotfile sync'
    $null = Wait-Until 'resumption explains that disabled fragments stay inactive' {
        Find-Element 'Keep managed fragments inactive until a bundle is installed'
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
} finally {
    if ($app) { Close-FixtureApp $app }
}
