[CmdletBinding()]
param([string]$Configuration = 'release', [string]$Profile = 'root')
# Opt-in: creates one unique remote /tmp directory, then removes exactly its known files.
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$gui = Join-Path $project "target\$Configuration\w11term.exe"
$ctl = Join-Path $project "target\$Configuration\w11term-cli.exe"
$token = [guid]::NewGuid().ToString('N')
$state = Join-Path $project ".local\sftp-smoke\$token"
$remote = "/tmp/w11term-sftp-$token"
New-Item -ItemType Directory -Path $state -Force | Out-Null
$terminal = $null
$created = $false
$handles = @()
function Invoke-Sftp([string[]]$Arguments) {
    $raw = & $ctl --pid $terminal.Id sftp --pane 1 @Arguments
    $reply = $raw | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or -not $reply.ok) { throw "SFTP failed: $raw" }
    return $reply.result.sftp
}
try {
    $terminal = Start-Process -FilePath $gui -WorkingDirectory $project -ArgumentList @('--profile', $Profile, '--cols', '120', '--rows', '32') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $state 'gui.log') -RedirectStandardError (Join-Path $state 'gui.err')
    $manifest = Join-Path $env:LOCALAPPDATA "w11term\instances\$($terminal.Id).json"
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while (-not (Test-Path -LiteralPath $manifest)) {
        if ($terminal.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw "GUI did not start; see $state\gui.err" }
        Start-Sleep -Milliseconds 50
    }
    $info = Invoke-Sftp @('info')
    $info | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $state 'server.json')
    Invoke-Sftp @('mkdir', $remote, '--permissions', '0700') | Out-Null
    $created = $true
    $source = Join-Path $state 'binary source.bin'
    $dest = Join-Path $state 'download.bin'
    $bytes = [byte[]]::new(250003)
    [System.Random]::new(42).NextBytes($bytes)
    [System.IO.File]::WriteAllBytes($source, $bytes)
    Invoke-Sftp @('put', $source, "$remote/original") | Out-Null
    Invoke-Sftp @('get', "$remote/original", $dest) | Out-Null
    if ((Get-FileHash -LiteralPath $source).Hash -ne (Get-FileHash -LiteralPath $dest).Hash) { throw 'Binary transfer mismatch' }
    $stat = Invoke-Sftp @('stat', "$remote/original")
    if ($stat.attrs.size -ne $bytes.Length) { throw 'Incorrect remote size' }
    Invoke-Sftp @('setstat', "$remote/original", '--permissions', '0640', '--atime', '1700000000', '--mtime', '1700000000') | Out-Null
    $file = Invoke-Sftp @('open', "$remote/original", '--flags', 'read,write')
    $handles += $file.handle
    Invoke-Sftp @('fsetstat', '--handle', $file.handle, '--size', '64') | Out-Null
    Invoke-Sftp @('write', '--handle', $file.handle, '--offset', '1', '--base64', 'AAf/') | Out-Null
    $read = Invoke-Sftp @('read', '--handle', $file.handle, '--offset', '1', '--length', '3')
    if ($read.data_base64 -ne 'AAf/') { throw 'Offset read/write mismatch' }
    $stat = Invoke-Sftp @('fstat', '--handle', $file.handle)
    if ($stat.attrs.size -ne 64) { throw 'fsetstat/fstat mismatch' }
    if ($info.extensions.'fsync@openssh.com') { Invoke-Sftp @('fsync', '--handle', $file.handle) | Out-Null }
    Invoke-Sftp @('close', '--handle', $file.handle) | Out-Null
    $handles = @()
    Invoke-Sftp @('rename', "$remote/original", "$remote/renamed") | Out-Null
    Invoke-Sftp @('symlink', 'renamed', "$remote/link") | Out-Null
    $link = Invoke-Sftp @('readlink', "$remote/link")
    if ($link.entries[0].filename -ne 'renamed') { throw 'Symlink/readlink mismatch' }
    $linkStat = Invoke-Sftp @('lstat', "$remote/link")
    if (($linkStat.attrs.permissions -band 0xF000) -ne 0xA000) { throw 'lstat followed the symlink' }
    if ($info.extensions.'hardlink@openssh.com') { Invoke-Sftp @('hardlink', "$remote/renamed", "$remote/hard") | Out-Null }
    if ($info.extensions.'statvfs@openssh.com') { Invoke-Sftp @('statvfs', $remote) | Out-Null }
    if ($info.extensions.'limits@openssh.com') { Invoke-Sftp @('limits') | Out-Null }
    if ($info.extensions.'expand-path@openssh.com') { Invoke-Sftp @('expand-path', $remote) | Out-Null }
    Invoke-Sftp @('realpath', $remote) | Out-Null
    $directory = Invoke-Sftp @('opendir', $remote)
    $handles += $directory.handle
    $names = @()
    do {
        $page = Invoke-Sftp @('readdir', '--handle', $directory.handle)
        $names += $page.entries.filename
    } while (-not $page.eof)
    if ($names -notcontains 'renamed' -or $names -notcontains 'link') { throw 'Missing directory entries' }
    Invoke-Sftp @('close', '--handle', $directory.handle) | Out-Null
    $handles = @()
    $listing = Invoke-Sftp @('ls', $remote)
    if ($listing.entries.filename -notcontains 'renamed') { throw 'ls did not contain uploaded file' }
    Write-Output "SFTP live smoke passed (GUI PID $($terminal.Id), binary SHA256 verified, v$($info.version)). Artifacts: $state"
}
finally {
    if ($terminal -and -not $terminal.HasExited) {
        foreach ($handle in $handles) { & $ctl --pid $terminal.Id sftp --pane 1 close --handle $handle | Out-Null }
        if ($created) {
            foreach ($name in @('link', 'hard', 'renamed', 'original')) { & $ctl --pid $terminal.Id sftp --pane 1 remove-file "$remote/$name" | Out-Null }
            $cleanup = & $ctl --pid $terminal.Id sftp --pane 1 remove-dir $remote | ConvertFrom-Json
            if (-not $cleanup.ok) { Write-Warning "Remote test directory remains: $remote ($($cleanup.error))" }
        }
        $null = $terminal.CloseMainWindow()
        if (-not $terminal.WaitForExit(5000)) { Stop-Process -Id $terminal.Id }
    }
}
