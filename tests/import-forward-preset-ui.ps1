[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/forward-http.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$fingerprint = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.'palette-ssh'.host_key
$aliasName = 'imported-loopback'
$localPort = [int][IO.File]::ReadAllText((Join-Path $State 'local-port'))
$remotePort = [int][IO.File]::ReadAllText((Join-Path $State 'target-port'))
$wildcard = [int][IO.File]::ReadAllText((Join-Path $State 'wildcard-port'))
$token = [IO.File]::ReadAllText((Join-Path $State 'token'))
$ports = Join-Path $State 'client-ports'
$loopback = [Net.IPAddress]::Loopback
$preset = "Windows $localPort " + [char]0x2192 + " remote $remotePort"
$use = 'Use saved forward' + [char]0x2026
$action = 'Forward a local port' + [char]0x2026
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$text = [System.Windows.Automation.ControlType]::Text
$menu = [System.Windows.Automation.ControlType]::MenuItem
$valuePattern = [System.Windows.Automation.ValuePattern]::Pattern

function Click([string]$Name, $Type = $button) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $entry = Find-Element $Name $Type
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
}
function Elements($Type) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, $Type)
    $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)
}
function Preview-Diagnostics {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $pathBox = Find-Element 'OpenSSH config path' $edit
    $path = if ($pathBox) { $pathBox.GetCurrentPattern($valuePattern).Current.Value }
    $preview = Find-Element 'Preview hosts' $button
    $enabled = if ($preview) { $preview.Current.IsEnabled } else { 'absent' }
    $aliasPicker = Find-Element 'Host alias' ([System.Windows.Automation.ControlType]::ComboBox)
    $selection = if ($aliasPicker) {
        $aliasPicker.GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection() | ForEach-Object { $_.Current.Name }
    }
    $labels = @(Elements $text | ForEach-Object { $_.Current.Name } | Where-Object { $_ } | Select-Object -Last 24)
    return ("Window {0}; config '{1}'; Preview hosts enabled: {2}; selected alias '{3}'; expected '{4}'; UI text: {5}" -f $root.Current.BoundingRectangle, $path, $enabled, ($selection -join ', '), $preset, ($labels -join ' | '))
}
function Field([string]$Name) {
    (Wait-Until $Name { Find-Element $Name $edit }).GetCurrentPattern($valuePattern)
}
function Select-Keyless {
    $auth = Wait-Until 'authentication selector with Windows SSH agent selected' {
        foreach ($combo in (Elements ([System.Windows.Automation.ControlType]::ComboBox))) {
            $selection = $combo.GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
            if ($selection.Count -and $selection[0].Current.Name -eq 'Windows SSH agent') { return $combo }
        }
    }
    $auth.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $choice = Wait-Until 'keyless loopback authentication' {
        Find-Element 'None (keyless server)' ([System.Windows.Automation.ControlType]::ListItem)
    }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
}
function Saved-Profile { ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.$aliasName }
function Assert-OnlySupportedPair {
    $pairs = @( (Saved-Profile).local_forwards )
    if ($pairs.Count -ne 1 -or $pairs[0].local_port -ne $localPort -or $pairs[0].remote_port -ne $remotePort) {
        throw "Unexpected saved forwarding presets: $($pairs | ConvertTo-Json -Compress)"
    }
}
function Apply-Preset {
    Click $use
    Click $preset $menu
    $null = Wait-Until 'preset fills both draft fields' {
        (Field 'Remote localhost port').Current.Value -ceq "$remotePort" -and (Field 'Windows local port').Current.Value -ceq "$localPort"
    }
    if (Find-Element "Stop forwarding port $localPort" $button) { throw 'Selecting a preset created a listener' }
    Expect-Refused $localPort
}

for ($launch = 1; $launch -le 2; $launch++) {
    $source = if ($launch -eq 1) { @('--demo') } else { @('--profile',$aliasName) }
    $argsForGui = @('--profiles',('"' + $profiles + '"'),'--cols','86','--rows','15') + $source
    $log = Join-Path $State 'gui.log'
    $app = Start-Process -FilePath $Gui -ArgumentList $argsForGui -WindowStyle Hidden -PassThru -RedirectStandardError $log
    try {
        $window = Wait-InputWindow
        $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
        if ($launch -eq 1) {
            Run-Palette 'Settings'
            Click ('Import OpenSSH config' + [char]0x2026)
            $configPath = Join-Path $State 'config'
            # Preview starts enabled for ~/.ssh/config. Observing it disable and
            # re-enable proves the app processed the path change before invoking it.
            (Field 'OpenSSH config path').SetValue('')
            $null = Wait-Until 'empty path disables Preview hosts' {
                $preview = Find-Element 'Preview hosts' $button
                $preview -and !$preview.Current.IsEnabled
            }
            (Field 'OpenSSH config path').SetValue($configPath)
            $null = Wait-Until 'fixture config path enables Preview hosts' {
                $preview = Find-Element 'Preview hosts' $button
                $box = Find-Element 'OpenSSH config path' $edit
                $preview -and $preview.Current.IsEnabled -and $box -and $box.GetCurrentPattern($valuePattern).Current.Value -ceq $configPath
            }
            Click 'Preview hosts'
            $null = Wait-Until 'supported endpoints in import preview' { Find-Element $preset $text }
            $null = Wait-Until 'warning identifies the unsupported wildcard bind' {
                (Elements $text) | Where-Object { $_.Current.Name.Contains('LocalForward') -and $_.Current.Name.Contains("*:$wildcard") }
            }
            if (Saved-Profile) { throw 'Preview saved a profile before confirmation' }
            Click 'Save profile'
            $null = Wait-Until 'saved imported profile' { Find-Element "Saved profile $aliasName" $text }
            Assert-OnlySupportedPair
            Expect-Refused $localPort
            Expect-Refused $wildcard
            # The imported config defaults to the Windows agent. Configure the
            # real keyless test peer and its fingerprint in the regular editor.
            Run-Palette 'Settings'
            Click "Edit profile $aliasName"
            Select-Keyless
            (Field 'Host fingerprint').SetValue($fingerprint)
            Click 'Save profile changes'
            $null = Wait-Until 'pinned keyless profile saved' {
                $saved = Saved-Profile
                $saved.auth -ceq 'none' -and $saved.host_key -ceq $fingerprint
            }
            Assert-OnlySupportedPair
            continue
        }
        $pane = ((Control @('list')).sessions | Where-Object label -eq $aliasName).id
        $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
        $generation = ((Control @('list')).sessions | Where-Object id -eq $pane).connection_generation
        Expect-Refused $localPort
        Run-Palette $action
        Apply-Preset
        Click 'Start forwarding'
        $null = Wait-Until 'explicitly started saved port' { Find-Element "Stop forwarding port $localPort" $button }
        $client = Open-Client $localPort
        Confirm-HttpEcho $client
        Expect-Refused $wildcard
        [IO.File]::Delete([IO.File]::ReadAllText((Join-Path $State 'disconnect-gate')))
        Expect-Closed $client
        $null = Wait-Until 'same imported pane on a new SSH transport' {
            $s = (Control @('list')).sessions | Where-Object id -eq $pane
            $s.connection_generation -gt $generation -and $s.label -eq $aliasName
        }
        Expect-Refused $localPort
        Picker-Query $aliasName
        Press-Key 13
        Wait-Focus $pane
        Run-Palette $action
        Apply-Preset
        Assert-OnlySupportedPair
    } catch {
        $failure = "$_ on launch $launch at $($_.ScriptStackTrace)"
        if ($launch -eq 1 -and !$app.HasExited) {
            try { $failure += [Environment]::NewLine + (Preview-Diagnostics) }
            catch { $failure += [Environment]::NewLine + "Preview diagnostics unavailable: $_" }
        }
        throw $failure
    } finally {
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
}
