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
