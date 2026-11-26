[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/forward-http.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$destination = [int][IO.File]::ReadAllText((Join-Path $State 'target-port'))
$token = [IO.File]::ReadAllText((Join-Path $State 'token'))
$ports = Join-Path $State 'client-ports'
$loopback = [Net.IPAddress]::Loopback
$forward = 'Forward a local port' + [char]0x2026
$overview = 'Active port forwards'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$text = [System.Windows.Automation.ControlType]::Text
$tab = [System.Windows.Automation.ControlType]::TabItem

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $entry = Find-Element $Name $button -Visible
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
}
function Start-Forward {
    Run-Palette $forward
    $field = Wait-Until 'remote port field' { Find-Element 'Remote localhost port' $edit -Visible }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue("$destination")
    Click 'Start forwarding'
    Wait-Until 'an active ephemeral Windows listener' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $button)
        foreach ($entry in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) {
            if ($entry.Current.Name -match '^Stop forwarding port (\d+)$' -and !$entry.Current.IsOffscreen) {
                return [int]$Matches[1]
            }
        }
    }
}

$arguments = @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh', '--cols', '110', '--rows', '32')
$app = Start-Process -FilePath $Gui -ArgumentList $arguments -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control manifest' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $first = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$first",'--timeout','15','ready')
    $firstPort = Start-Forward
    $second = (Control @('open','--profile','palette-ssh','--activate')).pane
    $null = Control @('wait-for-text','--pane',"$second",'--timeout','15','ready')
    Wait-Focus $second
    $secondPort = Start-Forward

    Run-Palette $overview
    $null = Wait-Until 'the overview lists both originating panes and Windows endpoints' {
        (Find-Element $overview $tab) -and
            (Find-Element "Focus pane $first for port $firstPort" $button) -and
            (Find-Element "Focus pane $second for port $secondPort" $button) -and
            (Find-Element ("Windows 127.0.0.1:$firstPort " + [char]0x2192 + " remote 127.0.0.1:$destination") $text)
    }
    $left = Open-Client $firstPort
    $right = Open-Client $secondPort
    Confirm-HttpEcho $left
    Confirm-HttpEcho $right
    $null = Wait-Until 'both background listeners report one active client' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty,
            ('Listening ' + [char]0xb7 + ' 1 active connection'))
        $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition).Count -eq 2
    }
    Click "Stop forwarding port $firstPort"
    $null = Wait-Until 'only the stopped listener disappears' {
        !(Find-Element "Stop forwarding port $firstPort" $button) -and
            (Find-Element "Stop forwarding port $secondPort" $button)
    }
    Expect-Closed $left
    Expect-Refused $firstPort
    Confirm-HttpEcho $right
    if (@((Control @('list')).sessions).Count -ne 2) { throw 'Stopping the listener closed a terminal' }

    Click "Focus pane $second for port $secondPort"
    Wait-Focus $second
    Run-Palette $overview
    $null = Wait-Until 'the remaining listener is displayed when reopening the overview' {
        Find-Element "Stop forwarding port $secondPort" $button -Visible
    }
    $null = Control @('send-text','--pane',"$second",'--no-paste','--base64','BA==')
    Expect-Closed $right
    Expect-Refused $secondPort
    $null = Wait-Until 'the overview updates after the background SSH transport ends' {
        Find-Element 'No active port forwards in this window' $text
    }
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
