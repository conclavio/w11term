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
} finally {
    if ($app) { Close-FixtureApp $app }
}
