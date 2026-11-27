[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '
$fixture = Get-Content -Raw -LiteralPath (Join-Path $State 'catalog-tool-fixture.json') | ConvertFrom-Json

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
function Lookup([string]$Name) {
    $field = Wait-Until 'command-reference search box' { Find-Element 'Search command reference' $edit }
    $field.SetFocus()
    $field = Wait-Until 'focused command-reference search box' {
        $current = Find-Element 'Search command reference' $edit -Visible
        if ($current -and $current.Current.HasKeyboardFocus) { $current }
    }
    $field.GetCurrentPattern($value).SetValue($Name)
    $null = Wait-Until "'$Name' manual" { Find-Element $Name }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','"Palette PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $localPane = (Sessions | Where-Object label -eq 'Palette PowerShell').id
    Wait-Text $localPane 'PS '
    Run-Palette 'Command reference'
    Lookup 'jq'
    if (Find-Element 'Review installing jq' $button) { throw 'PowerShell offered an SSH tool install' }
    $pane = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $pane
    $null = Control @('wait-ssh-connected','--pane',"$pane",'--timeout','15')

    Lookup 'rg'
    Click 'Review installing rg'
    $null = Wait-Until 'review retains the active binary and plans to restore its launcher' {
        (Find-Element 'Install rg') -and
            (Get-NativeTextLabels | Where-Object { $_.Contains("rg${dot}Create /.local/bin/rg") -and $_.Contains('Keep installed binary') })
    }
    if (Find-Element 'Download pinned Linux build' $button) { throw 'A retained executable was offered a replacement download' }
    Click 'Install reviewed rg'
    $null = Wait-Until 'reviewed rg launcher repaired' { Find-Element 'Installed rg' }
    Click 'Back to command reference'

    Lookup 'jq'
    Click 'Review installing jq'
    $null = Wait-Until 'user-owned jq blocks the catalog review' {
        (Get-NativeTextLabels | Where-Object { $_.StartsWith("jq${dot}Existing entry at /.local/bin/jq") }) -and
            !(Find-Element 'Install reviewed jq' $button).Current.IsEnabled
    }
    if ((Find-Element 'Download pinned Linux build' $button) -or
        [IO.File]::ReadAllText($fixture.conflict) -cne 'operator-owned command') {
        throw 'The review downloaded or changed a conflicting launcher'
    }

    [IO.File]::Delete($fixture.conflict)
    Click 'Review tool again'
    $null = Wait-Until 'review offers the pinned Linux build for the remote architecture' {
        $download = Find-Element 'Download pinned Linux build' $button
        $download -and $download.Current.IsEnabled -and
            (Find-Element 'Download the pinned x86_64 build to continue') -and
            (Find-Element "SHA-256 $($fixture.pin)")
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'switching panes disables the download' {
        $download = Find-Element 'Download pinned Linux build' $button
        $download -and !$download.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$pane")
    Wait-Focus $pane

    if ($env:W11TERM_TEST_LIVE_CATALOG_DOWNLOAD -ceq '1') {
        Click 'Download pinned Linux build'
        try {
            $null = Wait-Until 'downloaded build is available for reviewed install' {
                $install = Find-Element 'Install reviewed jq' $button
                $install -and $install.Current.IsEnabled -and !(Find-Element 'Download pinned Linux build' $button)
            }
        } catch {
            throw "$_ Observed controls $((Get-NativeTextLabels) -join ' | ')"
        }
        if ([IO.File]::Exists($fixture.conflict)) { throw 'Downloading the executable changed the remote host' }
        $null = Control @('focus-pane','--pane',"$localPane")
        Wait-Focus $localPane
        $null = Wait-Until 'switching panes disables installation' {
            $install = Find-Element 'Install reviewed jq' $button
            $install -and !$install.Current.IsEnabled
        }
        $null = Control @('focus-pane','--pane',"$pane")
        Wait-Focus $pane
        Click 'Install reviewed jq'
        $null = Wait-Until 'jq installation completed' {
            Get-NativeTextLabels | Where-Object { $_.StartsWith('Installed jq') -and $_.Contains('Automatic bundle sync is paused.') }
        }
    }
} finally {
    if ($app) { Close-FixtureApp $app }
}
