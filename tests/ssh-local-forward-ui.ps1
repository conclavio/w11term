[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/forward-http.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","focus_left":"F6","close_pane":"F9"}}')
$profiles = Join-Path $State 'profiles.json'
$destination = [int][IO.File]::ReadAllText((Join-Path $State 'target-port'))
$token = [IO.File]::ReadAllText((Join-Path $State 'token'))
$refuse = [IO.File]::ReadAllText((Join-Path $State 'refuse-flag'))
$ports = Join-Path $State 'client-ports'
$loopback = [Net.IPAddress]::Loopback
$action = 'Forward a local port' + [char]0x2026
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$text = [System.Windows.Automation.ControlType]::Text

function Click([string]$Label) {
    Invoke-Button (Wait-Until "enabled '$Label'" {
        $item = Find-Element $Label $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Field([string]$Label, [string]$Value) {
    $entry = Wait-Until $Label { Find-Element $Label $edit }
    $entry.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value)
}
function Window-Elements($Type) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, $Type)
    $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)
}
function Wait-RoutePreview([string]$WindowsEndpoint) {
    $route = "Windows $WindowsEndpoint " + [char]0x2192 + " remote 127.0.0.1:$destination"
    $null = Wait-Until "reviewed route '$route'" {
        $start = Find-Element 'Start forwarding' $button
        $start -and $start.Current.IsEnabled -and (Find-Element $route $text)
    }
}
function Start-Forward {
    Field 'Remote localhost port' "$destination"
    Field 'Windows local port' ''
    Wait-RoutePreview '127.0.0.1 (automatic port)'
    Click 'Start forwarding'
    Wait-Until 'ephemeral listener and its remote loopback endpoint' {
        foreach ($entry in (Window-Elements $button)) {
            if ($entry.Current.Name -match '^Stop forwarding port (\d+)$') {
                $n = [int]$Matches[1]
                $route = "127.0.0.1:$n " + [char]0x2192 + " 127.0.0.1:$destination"
                if ($n -gt 0 -and (Find-Element $route $text)) { return $n }
            }
        }
    }
}
function Select-Pane([string]$Label, [int]$Pane) {
    Picker-Query $Label
    Press-Key 13
    Wait-Focus $Pane
}
function PowerShell-Line([string]$Command, [string]$Expected) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Command + [char]13))
    $null = Control @('send-text','--pane',"$local",'--no-paste','--base64',$encoded)
    $null = Control @('wait-for-text','--pane',"$local",'--regex','--timeout','15',([regex]::Escape($Expected) + ' *\r?\nPS fixture>'))
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--workspace','forwarding','--cols','128','--rows','32') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $ssh = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','workspace-command-started')
    $generation = ((Control @('list')).sessions | Where-Object id -eq $ssh).connection_generation
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell in the same tab' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    PowerShell-Line 'function global:prompt { "PS fixture> " }; $global:forwardPid=$PID; "seed-"+"ready"' 'seed-ready'
    Press-Key 0x75
    Wait-Focus $ssh
    Run-Palette $action
    $busy = [Net.Sockets.TcpListener]::new($loopback, 0)
    $busy.ExclusiveAddressUse = $true
    $busy.Start()
    $busyPort = $busy.LocalEndpoint.Port
    try {
        Field 'Remote localhost port' "$destination"
        Wait-RoutePreview '127.0.0.1 (automatic port)'
        Field 'Windows local port' '0'
        $null = Wait-Until 'an invalid Windows port cannot be started' {
            $start = Find-Element 'Start forwarding' $button
            $message = 'Enter a Windows TCP port between 1 and 65535, or leave it blank.'
            $start -and !$start.Current.IsEnabled -and (Find-Element $message $text)
        }
        Field 'Windows local port' "$busyPort"
        Wait-RoutePreview "127.0.0.1:$busyPort"
        Click 'Start forwarding'
        $null = Wait-Until 'bind failure displayed in manager' {
            (Window-Elements $text) | Where-Object {
                $_.Current.Name.StartsWith('Could not start forwarding') -and
                    $_.Current.Name.Contains("Could not listen on 127.0.0.1:$busyPort")
            }
        }
        if ((Window-Elements $button) | Where-Object { $_.Current.Name -match '^Stop forwarding port \d+$' }) {
            throw 'Starting the occupied port created an unexpected forwarding listener'
        }
    } finally { $busy.Stop() }

    $port = Start-Forward
    $held = Open-Client $port
    Confirm-HttpEcho $held
    $null = Wait-Until 'one active forwarded client' { Find-Element ('Listening ' + [char]0xb7 + ' 1 active connection') $text }
    [IO.File]::WriteAllText($refuse, '')
    try { Expect-Closed (Open-Client $port) } finally { [IO.File]::Delete($refuse) }
    $null = Wait-Until 'remote channel refusal reported for the intended endpoint' {
        (Window-Elements $text) | Where-Object { $_.Current.Name.Contains("Could not open remote 127.0.0.1:$destination") }
    }
    Confirm-HttpEcho $held
    Select-Pane 'Palette PowerShell' $local
    Confirm-HttpEcho $held
    Wait-Focus $local
    PowerShell-Line '"forward-pid-" + ($forwardPid -eq $PID)' 'forward-pid-True'
    Press-Key 0x75
    Wait-Focus $ssh
    foreach ($c in 'ssh-forward-live'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','ssh-forward-live')
    if (((Control @('list')).sessions | Where-Object id -eq $ssh).connection_generation -ne $generation) { throw 'Forwarding replaced the source SSH transport' }
    Run-Palette $action
    Click "Stop forwarding port $port"
    $null = Wait-Until 'stop removes the original listener' { !(Find-Element "Stop forwarding port $port" $button) }
    Expect-Closed $held
    Expect-Refused $port

    $port = Start-Forward
    $held = Open-Client $port
    Confirm-HttpEcho $held
    Select-Pane 'palette-ssh' $ssh
    Press-Key 0x78
    $null = Wait-Until 'source SSH pane closed, PowerShell still present' {
        $sessions = (Control @('list')).sessions
        !($sessions | Where-Object id -eq $ssh) -and ($sessions | Where-Object id -eq $local)
    }
    Expect-Closed $held
    Expect-Refused $port
    $second = (Control @('open','--profile','palette-ssh','--activate')).pane
    $null = Control @('wait-for-text','--pane',"$second",'--timeout','15','ready')
    Wait-Focus $second
    Run-Palette $action
    $port = Start-Forward
    $held = Open-Client $port
    Confirm-HttpEcho $held
    $null = Control @('send-text','--pane',"$second",'--no-paste','--base64','BA==')
    Expect-Closed $held
    Expect-Refused $port
    Select-Pane 'Palette PowerShell' $local
    PowerShell-Line '"survived-disconnect-" + ($forwardPid -eq $PID)' 'survived-disconnect-True'
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    [IO.File]::Delete($refuse)
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
