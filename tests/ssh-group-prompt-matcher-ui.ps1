[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_TEST_COPY_FILE = Join-Path $State 'prompt-setup-copy.txt'
$profiles = Join-Path $State 'profiles.json'
$original = [IO.File]::ReadAllText($profiles)
$port = ($original | ConvertFrom-Json).profiles.Appliance.port
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

function Pane-Status([string]$Status) {
    $null = Wait-Until "pane status '$Status'" { Find-Element "Pane $pane${dot}Connected${dot}$Status" }
}
function Outcome([string]$Status) {
    $null = Wait-Until "appliance outcome '$Status'" { Find-Element "$target${dot}$Status" }
}
function Send-Raw([string]$Text) {
    $null = Control @('send-text','--pane',"$pane",'--no-paste',$Text)
}
function Click([string]$Name) {
    $invoke = Wait-Until "enabled '$Name'" {
        $element = Find-Element $Name $button
        $provider = $null
        if ($element -and $element.Current.IsEnabled -and $element.TryGetCurrentPattern(
                [System.Windows.Automation.InvokePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $invoke.Invoke()
}
function Pattern-Field {
    Wait-Until 'pane prompt regex field' {
        $element = Find-Element 'Pane prompt regex' $edit
        $provider = $null
        if ($element -and $element.Current.IsEnabled -and $element.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider)) { $provider }
    }
}
function Reconnect {
    $previous = (Control @('inspect','--pane',"$pane")).connection_generation
    Send-Raw 'drop'
    $null = Wait-Until 'replacement appliance transport' {
        (Control @('inspect','--pane',"$pane")).connection_generation -gt $previous
    }
    Pane-Status 'Prompt unverified'
}

$launchArgs = @('--profiles',('"' + $profiles + '"'),'--profile','Appliance','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $launchArgs -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $pane = (Control @('list')).sessions[0].id
    $target = "Appliance${dot}test@127.0.0.1:${port}${dot}Pane $pane"
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','rack-1>>')
    Run-Palette 'SSH host group'
    Pane-Status "Ready${dot}prompt match"
    (Wait-CheckBox $target Off).Toggle()
    $field = Wait-Until 'group command editor' {
        $element = Find-Element 'Group command' $edit
        $provider = $null
        if ($element -and $element.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider)) { $provider }
    }
    $field.SetValue('show')
    Invoke-Button (Wait-Until 'enabled group send' {
        $entry = Find-Element 'Send to 1 host' $button
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
    Outcome 'Sent to SSH channel'
    Outcome 'Waiting for a matching prompt'
    Pane-Status 'Waiting for a fresh prompt'
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','appliance-output')
    Send-Raw 'finish'
    Outcome "Prompt returned${dot}exit status unavailable"
    Pane-Status "Ready${dot}prompt match"

    Reconnect
    Send-Raw 'redraw'
    Pane-Status "Ready${dot}prompt match"

    Click "Prompt setup for pane $pane"
    if ((Pattern-Field).Current.Value -cne 'rack-[12]>>') { throw 'Setup did not display the current profile pattern' }
    $null = Wait-Until 'integrations can be copied from the roster' {
        (Find-Element 'Copy Zsh shell integration' $button) -and (Find-Element 'Copy Fish shell integration' $button)
    }
    Click 'Copy Bash shell integration'
    $null = Wait-Until 'Bash integration copied' {
        $content = Read-ClipboardFixture
        $content -and $content.Contains('133;B')
    }
    (Pattern-Field).SetValue('[')
    Click 'Apply to pane'
    $null = Wait-Until 'invalid regex is reported' { Find-Element 'Invalid group prompt regex' }
    (Pattern-Field).SetValue('rack-1>>')
    Click 'Apply to pane'
    $null = Wait-Until 'worker acknowledges prompt configuration' {
        Find-Element 'Prompt pattern applied. Wait for a freshly printed matching prompt.'
    }
    Click 'Back to host group'
    Pane-Status 'Prompt unverified'
    $null = Wait-CheckBox $target On
    $null = Wait-Until 'a previously printed prompt does not unlock Send' {
        $element = Find-Element 'Send to 1 host' $button
        $element -and !$element.Current.IsEnabled
    }
    Send-Raw 'finish'
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','rack-2>>')
    Pane-Status 'Prompt unverified'
    Send-Raw 'redraw'
    Pane-Status "Ready${dot}prompt match"
    Reconnect
    Send-Raw 'finish'
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','rack-2>>')
    Pane-Status 'Prompt unverified'
    Send-Raw 'redraw'
    Pane-Status "Ready${dot}prompt match"

    Click "Prompt setup for pane $pane"
    if ((Pattern-Field).Current.Value -cne 'rack-1>>') { throw 'The live pattern did not survive reconnecting' }
    (Pattern-Field).SetValue('')
    Click 'Apply to pane'
    $null = Wait-Until 'matching disabled' { Find-Element 'Prompt matching disabled for this pane.' }
    Click 'Back to host group'
    Pane-Status 'Prompt unverified'
    Send-Raw 'redraw'
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','rack-1>>')
    Pane-Status 'Prompt unverified'
    if ([IO.File]::ReadAllText($profiles) -cne $original) { throw 'Live setup changed the saved profile' }
} catch {
    $failure = $_
    $visible = if ($window) {
        try {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            $root.FindAll(
                [System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.Condition]::TrueCondition
            ) | ForEach-Object { $_.Current.Name } | Where-Object { $_ -match 'Appliance|Pane |Submitted|Waiting' }
        } catch {}
    }
    throw "$($failure.Exception.Message)`nUI: $($visible -join ' | ')`n$($failure.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
