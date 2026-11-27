[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$dot = ' ' + [char]0xb7 + ' '
$updatedLabel = "Dotfiles at login${dot}Bash${dot}Updated"
$skippedLabel = "Dotfiles at login${dot}Bash${dot}Skipped"
$bundle = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'palette-ssh'.dotfiles.bundle

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

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $localPane = (Sessions | Where-Object label -eq 'Palette PowerShell').id
    Wait-Text $localPane 'PS '
    $pane = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $pane
    $generation = (Control @('wait-ssh-connected','--pane',"$pane",'--timeout','15')).connection_generation
    Run-Palette 'Command reference'
    $null = Wait-Until 'updated login result in command reference' { Find-Element $updatedLabel }
    Click 'Apply SSH dotfiles'
    $null = Wait-Until 'updated login result in dotfile installer' { Find-Element $updatedLabel }

    [IO.File]::WriteAllText($bundle, '{')
    $null = Control @('send-text','--pane',"$pane",'--connection-generation',"$generation",'--no-paste','--base64','BA==')
    $connected = Control @('wait-ssh-connected','--pane',"$pane",'--after-generation',"$generation",'--timeout','15')
    $null = Wait-Until 'installer opened on the prior connection no longer shows a login result' {
        (Find-Element 'Return to the original SSH connection, or reopen this panel after reconnecting') -and
            !(Find-Element $updatedLabel) -and !(Find-Element $skippedLabel)
    }
    Click 'Back to command reference'
    $null = Wait-Until 'skipped login result on the reconnected pane' { Find-Element $skippedLabel }
    Click 'Apply SSH dotfiles'
    $null = Wait-Until 'installer explains why login sync was skipped' {
        (Find-Element $skippedLabel) -and
            (Get-NativeTextLabels | Where-Object { $_.Contains('Reading the dotfiles bundle JSON') })
    }

    $reports = (Control @('events','--pane',"$pane",'--kind','dotfiles-sync')).journal.events
    if ($reports.Count -ne 2 -or $reports[0].data.status -cne 'updated' -or
        $reports[0].data.generation -ne $generation -or $reports[1].data.status -cne 'skipped' -or
        $reports[1].data.generation -ne $connected.connection_generation) {
        throw 'The dotfiles event journal did not preserve the two connection outcomes'
    }
    Click 'Back to command reference'
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'local PowerShell has no SSH dotfile result' {
        (Find-Element 'Search command reference') -and !(Find-Element $updatedLabel) -and !(Find-Element $skippedLabel)
    }
} finally {
    if ($app) { Close-FixtureApp $app }
}
