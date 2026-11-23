[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$port = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles.Appliance.port
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

    $previous = (Control @('inspect','--pane',"$pane")).connection_generation
    Send-Raw 'drop'
    $null = Wait-Until 'replacement appliance transport' {
        (Control @('inspect','--pane',"$pane")).connection_generation -gt $previous
    }
    Pane-Status 'Prompt unverified'
    Send-Raw 'redraw'
    Pane-Status "Ready${dot}prompt match"
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
