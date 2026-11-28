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
    Click 'Manage PATH for new shells'
    Click 'Review PATH setup'
    $null = Wait-Until 'review explains the startup edits for a manual install' {
        (Find-Element "Append managed block to${dot}/.bashrc") -and (Find-Element "Create${dot}/.profile") -and
            (Find-Element 'The launcher directory will be appended to PATH for new shells.') -and
            (Find-Element 'Apply reviewed PATH setup' $button).Current.IsEnabled
    }
    if ([IO.File]::Exists($fixture.login) -or [IO.File]::ReadAllText($fixture.startup).Contains('w11term tool PATH')) {
        throw 'Review changed remote startup files before confirmation'
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'another pane cannot apply the PATH review' {
        $apply = Find-Element 'Apply reviewed PATH setup' $button
        $apply -and !$apply.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$pane")
    Wait-Focus $pane
    Click 'Apply reviewed PATH setup'
    $null = Wait-Until 'manual PATH setup completed' { Find-Element 'Managed PATH added for new shells' }
    $startupWithPath = [IO.File]::ReadAllText($fixture.startup)
    $loginWithPath = [IO.File]::ReadAllText($fixture.login)
    if (!$startupWithPath.Contains('PATH="${PATH:+$PATH:}$HOME/.local/bin"') -or !$loginWithPath.Contains('$HOME/.local/bin')) {
        throw 'PATH setup did not append the managed launcher directory'
    }
    Click 'Back to tool installer'
    Click 'Back to command reference'

    $probe = [IO.File]::ReadAllText($fixture.probe)
    [IO.File]::WriteAllText($fixture.probe, $probe.Replace("/usr/bin`n", "/usr/bin:/.local/bin`n"))
    Lookup 'rg'
    Click 'Review installing rg'
    $null = Wait-Until 'installed tool in PATH still offers management' {
        (Find-Element 'Launcher directory is in the probed PATH') -and
            (Find-Element 'Manage PATH for new shells' $button)
    }
    Click 'Manage PATH for new shells'
    [IO.File]::AppendAllText($fixture.startup, "# added after setup`n")
    Click 'Review PATH removal'
    $null = Wait-Until 'review identifies both managed blocks to remove' {
        (Find-Element "Remove managed block from${dot}/.bashrc") -and
            (Find-Element "Remove managed block from${dot}/.profile") -and
            (Get-NativeTextLabels | Where-Object { $_.StartsWith('Block to remove') }) -and
            (Find-Element 'Remove reviewed PATH blocks' $button).Current.IsEnabled
    }
    if ([IO.File]::ReadAllText($fixture.startup) -cne ($startupWithPath + "# added after setup`n") -or
        [IO.File]::ReadAllText($fixture.login) -cne $loginWithPath) {
        throw 'Removal review changed a startup file before confirmation'
    }
    $null = Control @('focus-pane','--pane',"$localPane")
    Wait-Focus $localPane
    $null = Wait-Until 'another pane cannot confirm the PATH removal' {
        $remove = Find-Element 'Remove reviewed PATH blocks' $button
        $remove -and !$remove.Current.IsEnabled
    }
    $null = Control @('focus-pane','--pane',"$pane")
    Wait-Focus $pane
    Click 'Remove reviewed PATH blocks'
    $null = Wait-Until 'managed PATH removal completed' { Find-Element 'Managed PATH blocks removed for new shells' }

    Select-NativeComboItem 'PATH setup shell' 'Fish'
    Click 'Review PATH setup'
    $null = Wait-Until 'Fish PATH review lists the new startup directory and file' {
        (Find-Element "Create directory${dot}/.config/fish") -and
            (Find-Element "Create${dot}/.config/fish/config.fish") -and
            (Find-Element 'Apply reviewed PATH setup' $button).Current.IsEnabled
    }
    if ([IO.Directory]::Exists((Split-Path -Parent $fixture.fishStartup))) {
        throw 'Fish PATH review created the startup directory before confirmation'
    }
    Click 'Apply reviewed PATH setup'
    $null = Wait-Until 'Fish PATH setup writes a native marked block' {
        (Find-Element 'Managed PATH added for new shells') -and
            [IO.File]::ReadAllText($fixture.fishStartup).Contains('builtin set -gx PATH')
    }
    [IO.File]::AppendAllText($fixture.fishStartup, "# keep Fish settings`n")
    Click 'Review PATH removal'
    $null = Wait-Until 'Fish removal names the reviewed startup file' {
        (Find-Element "Remove managed block from${dot}/.config/fish/config.fish") -and
            (Find-Element 'Remove reviewed PATH blocks' $button).Current.IsEnabled
    }
    Click 'Remove reviewed PATH blocks'
    $null = Wait-Until 'Fish PATH removal preserves unrelated config' {
        (Find-Element 'Managed PATH blocks removed for new shells') -and
            [IO.File]::ReadAllText($fixture.fishStartup) -ceq "# keep Fish settings`n"
    }
    [IO.File]::WriteAllText($fixture.probe, $probe)
    Click 'Back to tool installer'
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
