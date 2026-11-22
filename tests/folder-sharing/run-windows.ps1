#requires -Version 7.4
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$GuestDirectory,
    [Parameter(Mandatory)][string]$TestExecutable,
    [string]$HostCertificateTestExecutable,
    [string]$QemuExecutable = 'qemu-system-x86_64.exe',
    [switch]$LegacyFuse,
    [string]$GuiExecutable,
    [string]$CliExecutable,
    [switch]$CheckClipboard
)
$ErrorActionPreference = 'Stop'
if ([bool]$GuiExecutable -ne [bool]$CliExecutable -or ($LegacyFuse -and $GuiExecutable) -or ($CheckClipboard -and !$GuiExecutable)) {
    throw 'Pass both GUI and CLI paths to run the mounted-folder UI on a modern guest'
}

function Start-FixtureProcess([string]$Program, [string[]]$Arguments, [hashtable]$Environment = @{}) {
    $start = [Diagnostics.ProcessStartInfo]::new($Program)
    $start.WorkingDirectory = $scratch.FullName
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    foreach ($arg in $Arguments) { $start.ArgumentList.Add($arg) }
    foreach ($key in $Environment.Keys) { $start.Environment[$key] = $Environment[$key] }
    return [Diagnostics.Process]::Start($start)
}

function Stop-FixtureProcess($Process) {
    if (!$Process.HasExited) {
        try { $Process.Kill($true) } catch [InvalidOperationException] {
            if (!$Process.HasExited) { throw }
        }
        if (!$Process.WaitForExit(10000)) { throw "Could not stop fixture process $($Process.Id)" }
    }
}

function Invoke-FixtureProcess([string]$Program, [string[]]$Arguments, [int]$TimeoutMs, [hashtable]$Environment = @{}) {
    $process = Start-FixtureProcess $Program $Arguments $Environment
    try {
        $stdout = $process.StandardOutput.ReadToEndAsync()
        $stderr = $process.StandardError.ReadToEndAsync()
        $finished = $process.WaitForExit($TimeoutMs)
        if (!$finished) { Stop-FixtureProcess $process }
        $output = $stdout.WaitAsync([TimeSpan]::FromSeconds(5)).GetAwaiter().GetResult()
        $errors = $stderr.WaitAsync([TimeSpan]::FromSeconds(5)).GetAwaiter().GetResult()
        if ($output) { Write-Host $output.TrimEnd() }
        if ($errors) { Write-Host $errors.TrimEnd() }
        if (!$finished) { throw "$Program timed out after $TimeoutMs ms" }
        if ($process.ExitCode -ne 0) { throw "$Program exited with code $($process.ExitCode)" }
        return $output
    } finally {
        try { Stop-FixtureProcess $process } finally { $process.Dispose() }
    }
}

function Read-Serial {
    if (![IO.File]::Exists($serial)) { return '' }
    $stream = [IO.File]::Open($serial, 'Open', 'Read', 'ReadWrite')
    $reader = [IO.StreamReader]::new($stream)
    try { return $reader.ReadToEnd() } finally { $reader.Dispose() }
}

function Wait-GuestKey {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    while ($timer.Elapsed.TotalSeconds -lt 180) {
        if ($vm.HasExited) { throw "QEMU exited before SSH was ready (exit $($vm.ExitCode))" }
        # An ed25519 public key encodes a 51-byte OpenSSH wire blob. Wait for the complete line.
        if ((Read-Serial) -cmatch '(?m)^W11TERM_SSH_HOST_KEY (ssh-ed25519 [A-Za-z0-9+/]{68})\r*\n') {
            return $Matches[1]
        }
        Start-Sleep -Milliseconds 250
    }
    throw 'Timed out waiting for the guest SSH host key on its serial console'
}

function Confirm-SshBanner([int]$Port) {
    $client = [Net.Sockets.TcpClient]::new([Net.Sockets.AddressFamily]::InterNetwork)
    try {
        $connect = $client.ConnectAsync([Net.IPAddress]::Loopback, $Port)
        if (!$connect.Wait(5000)) { throw 'Timed out connecting to the loopback SSH listener' }
        $reader = [IO.StreamReader]::new($client.GetStream(), [Text.Encoding]::ASCII)
        try {
            $line = $reader.ReadLineAsync()
            if (!$line.Wait(10000) -or $line.Result -cnotmatch '^SSH-2\.0-') {
                throw 'The forwarded port did not provide an SSH identification banner'
            }
        } finally { $reader.Dispose() }
    } finally { $client.Dispose() }
}

$qemu = (Get-Command -Name $QemuExecutable -CommandType Application -TotalCount 1 -ErrorAction Stop).Source
$keygen = (Get-Command ssh-keygen.exe -CommandType Application -TotalCount 1 -ErrorAction Stop).Source
$test = (Get-Item -LiteralPath $TestExecutable).FullName
$kernel = (Get-Item -LiteralPath (Join-Path $GuestDirectory 'vmlinuz')).FullName
$initramfs = (Get-Item -LiteralPath (Join-Path $GuestDirectory 'initramfs.gz')).FullName
$scratch = [IO.Directory]::CreateTempSubdirectory('w11term-mount-')
$identity = Join-Path $scratch.FullName 'client-key'
$serial = Join-Path $scratch.FullName 'serial.log'
$vm = $null
try {
    $null = Invoke-FixtureProcess $keygen @('-q','-t','ed25519','-N','','-C','w11term-ci','-f',$identity) 30000
    $vm = Start-FixtureProcess $qemu @(
        '-machine','pc,accel=tcg','-cpu','max','-smp','2','-m','1536M',
        '-nodefaults','-display','none','-monitor','none','-no-reboot','-serial','file:serial.log',
        '-object','rng-builtin,id=rng0','-device','virtio-rng-pci,rng=rng0',
        '-kernel',$kernel,'-initrd',$initramfs,'-append','console=ttyS0 rdinit=/init',
        '-fw_cfg','name=opt/w11term/authorized_key,file=client-key.pub',
        '-netdev','user,id=fixture,restrict=on,ipv6=off,hostfwd=tcp:127.0.0.1:0-:22',
        '-device','virtio-net-pci,netdev=fixture'
    )
    $vmStdout = $vm.StandardOutput.ReadToEndAsync()
    $vmStderr = $vm.StandardError.ReadToEndAsync()
    $hostKey = Wait-GuestKey
    # QEMU owns its ephemeral port continuously. Discover only sockets of the process we started.
    $listeners = @(Get-NetTCPConnection -OwningProcess $vm.Id -State Listen)
    if ($listeners.Count -ne 1 -or $listeners[0].LocalAddress -ne '127.0.0.1') {
        throw "Expected one loopback listener on QEMU PID $($vm.Id)"
    }
    $port = $listeners[0].LocalPort
    Confirm-SshBanner $port
    Write-Host "Guest SSH is ready on 127.0.0.1:$port"
    if ($HostCertificateTestExecutable) {
        # The guest reports its public CA before its readiness marker. Its signing key stays in RAM.
        if ((Read-Serial) -cnotmatch '(?m)^W11TERM_SSH_HOST_CA (ssh-ed25519 [A-Za-z0-9+/]{68})\r*\n') {
            throw 'Missing the guest host CA on its serial console'
        }
        $certificateTests = (Get-Item -LiteralPath $HostCertificateTestExecutable).FullName
        $output = Invoke-FixtureProcess $certificateTests @('--ignored','--exact','stock_openssh_host_certificate','--nocapture') 120000 @{
            W11TERM_CERT_TEST_PORT = [string]$port
            W11TERM_CERT_TEST_CA = $Matches[1]
            W11TERM_CERT_TEST_IDENTITY = $identity
        }
        if ($output -cnotmatch '(?m)^test result: ok\. 1 passed; 0 failed;') {
            throw 'The OpenSSH host certificate regression did not report exactly one pass'
        }
        # This runs before the mount tests log in. The two rejected certificates must never reach login.
        $logins = [regex]::Matches((Read-Serial), 'Accepted publickey for w11fixture from ').Count
        if ($logins -ne 1) { throw "Expected exactly one accepted public-key login in sshd's log, got $logins" }
    }
    $cases = if ($LegacyFuse) { 'old_fusermount_refuses_before_export' } else { 'native_windows_passive_sshfs','native_windows_writable_sshfs' }
    foreach ($case in $cases) {
        $output = Invoke-FixtureProcess $test @('--ignored','--exact',"ssh::folder_mount_fixture::$case",'--nocapture') 180000 @{
            W11TERM_MOUNT_TEST_PORT = [string]$port
            W11TERM_MOUNT_TEST_IDENTITY = $identity
            W11TERM_MOUNT_TEST_HOST_KEY = $hostKey
        }
        if ($output -cnotmatch '(?m)^test result: ok\. 1 passed; 0 failed;') {
            throw "$case did not report exactly one pass"
        }
    }
    if ($GuiExecutable) {
        $gui = (Get-Item -LiteralPath $GuiExecutable).FullName
        $cli = (Get-Item -LiteralPath $CliExecutable).FullName
        $ui = [IO.Directory]::CreateDirectory((Join-Path $scratch.FullName 'native-ui'))
        try {
            $blob = [Convert]::FromBase64String($hostKey.Split(' ')[1])
            $fingerprint = 'SHA256:' + [Convert]::ToBase64String([Security.Cryptography.SHA256]::HashData($blob)).TrimEnd('=')
            $uiArgs = @('-NoProfile','-NonInteractive','-STA','-File',(Join-Path $PSScriptRoot 'mounted-folder-ui.ps1'),
                '-Gui',$gui,'-Cli',$cli,'-State',$ui.FullName,'-Port',"$port",'-Identity',$identity,'-Fingerprint',$fingerprint)
            if ($CheckClipboard) { $uiArgs += '-CheckClipboard' }
            $null = Invoke-FixtureProcess 'powershell.exe' $uiArgs 180000
        } finally {
            $target = (Resolve-Path -LiteralPath $ui.FullName).ProviderPath
            if ([IO.Path]::GetDirectoryName($target) -ne $scratch.FullName -or [IO.Path]::GetFileName($target) -ne 'native-ui' -or
                ((Get-Item -LiteralPath $target).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw 'Unexpected fixture cleanup directory'
            }
            Remove-Item -LiteralPath $target -Recurse -Force
        }
    }
} catch {
    Write-Host 'Guest serial console (last 60 lines)'
    (Read-Serial) -split '\r?\n' | Select-Object -Last 60 | Write-Host
    throw
} finally {
    try {
        if ($null -ne $vm) {
            try {
                Stop-FixtureProcess $vm
                foreach ($log in $vmStdout,$vmStderr) {
                    if ($null -ne $log) {
                        $message = $log.WaitAsync([TimeSpan]::FromSeconds(5)).GetAwaiter().GetResult()
                        if ($message) { Write-Host $message.TrimEnd() }
                    }
                }
            } finally { $vm.Dispose() }
        }
    } finally {
        [IO.File]::Delete($identity)
        [IO.File]::Delete("$identity.pub")
        [IO.File]::Delete($serial)
        $scratch.Delete()
    }
}
