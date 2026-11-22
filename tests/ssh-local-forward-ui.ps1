[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
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
function Start-Forward {
    Field 'Remote localhost port' "$destination"
    Field 'Windows local port' ''
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
function Open-Client([int]$Port) {
    $client = [Net.Sockets.TcpClient]::new([Net.Sockets.AddressFamily]::InterNetwork)
    try {
        if (!$client.ConnectAsync($loopback, $Port).Wait(3000)) { throw "Timed out connecting to local forward $Port" }
        $client.ReceiveTimeout = 4000
        $client.SendTimeout = 4000
        [IO.File]::AppendAllText($ports, "$($client.Client.LocalEndPoint.Port)" + [Environment]::NewLine)
        return $client
    } catch { $client.Dispose(); throw }
}
function Confirm-HttpEcho($Client) {
    $stream = $Client.GetStream()
    $request = [Text.Encoding]::ASCII.GetBytes("GET /$token HTTP/1.1`r`nHost: fixture`r`n`r`n")
    $stream.Write($request, 0, $request.Length)
    $expected = "HTTP/1.1 200 OK`r`nContent-Length: $($token.Length)`r`n`r`n$token"
    $reply = [byte[]]::new($expected.Length)
    $n = 0
    while ($n -lt $reply.Length) {
        $received = $stream.Read($reply, $n, $reply.Length - $n)
        if ($received -eq 0) { throw "HTTP echo closed at byte $n" }
        $n += $received
    }
    if ([Text.Encoding]::ASCII.GetString($reply) -cne $expected) { throw 'Forwarded HTTP reply did not match the isolated service' }
}
function Expect-Closed($Client) {
    try {
        if ($Client.GetStream().ReadByte() -ne -1) { throw 'Unexpected data after channel close' }
    } catch {
        $cause = $_.Exception.GetBaseException()
        if ($cause -isnot [Net.Sockets.SocketException] -or
            $cause.SocketErrorCode -notin @('ConnectionReset','ConnectionAborted','Shutdown')) { throw }
    } finally { $Client.Dispose() }
}
function Expect-Refused([int]$Port) {
    $client = [Net.Sockets.TcpClient]::new([Net.Sockets.AddressFamily]::InterNetwork)
    try {
        if (!$client.ConnectAsync($loopback, $Port).Wait(3000)) { throw "Local port $Port timed out instead of refusing" }
        throw "Local port $Port still accepts clients"
    } catch {
        $cause = $_.Exception.GetBaseException()
        if ($cause -isnot [Net.Sockets.SocketException] -or $cause.SocketErrorCode -ne 'ConnectionRefused') { throw }
    } finally { $client.Dispose() }
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
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
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
    try {
        Field 'Remote localhost port' "$destination"
        Field 'Windows local port' "$($busy.LocalEndpoint.Port)"
        Click 'Start forwarding'
        $null = Wait-Until 'bind failure displayed in manager' {
            (Window-Elements $text) | Where-Object { $_.Current.Name.StartsWith('Could not start forwarding') }
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
