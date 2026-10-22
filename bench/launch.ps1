[CmdletBinding()]
param(
    [Parameter(Mandatory,Position=0)]
    [ValidateSet('typometer','w11term','wezterm','wezterm-openssh','windows-terminal','windows-terminal-preview')]
    [string]$Target
)
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$state = Join-Path $project '.local\benchmark'

if ($Target -eq 'typometer') {
    $java = Join-Path $project '.local\tools\java\jdk8u504-b01-jre\bin\javaw.exe'
    $jar = Join-Path $project '.local\tools\typometer\typometer-1.0.1\typometer-1.0.1.jar'
    if (-not (Test-Path -LiteralPath $jar)) { throw 'Run .\bench\setup.ps1 first' }
    New-Item -ItemType Directory -Path (Join-Path $state 'results') -Force | Out-Null
    Start-Process -FilePath $java -ArgumentList @('-jar', ('"' + $jar + '"')) `
        -WorkingDirectory (Join-Path $state 'results') -WindowStyle Normal `
        -RedirectStandardError (Join-Path $state 'typometer.stderr.log') | Out-Null
    return
}

& (Join-Path $PSScriptRoot 'start-server.ps1')
$server = Get-Content -LiteralPath (Join-Path $state 'server.json') -Raw | ConvertFrom-Json
$known = Join-Path $state 'known_hosts'
$ssh = (Get-Command ssh.exe -ErrorAction Stop).Source
$sshArgs = @('-F','NUL','-tt','-p', [string]$server.port, '-o','BatchMode=yes',
    '-o','PreferredAuthentications=none','-o','PubkeyAuthentication=no',
    '-o','StrictHostKeyChecking=yes','-o',('UserKnownHostsFile="' + $known + '"'),
    '-o','GlobalKnownHostsFile=NUL','-o','LogLevel=ERROR','-o','EscapeChar=none','bench@127.0.0.1')

switch ($Target) {
    'w11term' {
        Start-Process -FilePath (Join-Path $project 'target\release\w11term.exe') `
            -ArgumentList @('--profiles', ('"' + (Join-Path $state 'profiles.json') + '"'), '--profile','loopback') `
            -WorkingDirectory $project -WindowStyle Normal | Out-Null
    }
    { $_ -in 'wezterm','wezterm-openssh' } {
        $wezterm = (Get-Command wezterm.exe -ErrorAction Stop).Source
        $argsForWezterm = @('--config-file', ('"' + (Join-Path $PSScriptRoot 'wezterm.lua') + '"'))
        if ($Target -eq 'wezterm') {
            $argsForWezterm += @('ssh','--class','w11term-benchmark',
                '-o',('UserKnownHostsFile="' + $known + '"'), '-o','StrictHostKeyChecking=yes',
                '-o','IdentityAgent=none', '-o','IdentitiesOnly=yes',
                ('bench@127.0.0.1:' + $server.port))
        } else {
            $argsForWezterm += @('start','--always-new-process','--class','w11term-benchmark', $ssh) + $sshArgs
        }
        Start-Process -FilePath $wezterm -ArgumentList $argsForWezterm -WorkingDirectory $project -WindowStyle Normal | Out-Null
    }
    { $_ -in 'windows-terminal','windows-terminal-preview' } {
        # A dedicated additive fragment leaves existing profiles and settings intact.
        $fragmentDir = Join-Path $env:LOCALAPPDATA 'Microsoft\Windows Terminal\Fragments\w11term-benchmark'
        New-Item -ItemType Directory -Path $fragmentDir -Force | Out-Null
        $fragment = @{ profiles = @(@{
            guid = '{34bc1b4d-4d87-4341-91a7-cc1584586e03}'
            name = 'w11term loopback benchmark'
            commandline = ('"' + $ssh + '" ' + ($sshArgs -join ' '))
            startingDirectory = $project
            font = @{ face = 'Cascadia Mono'; size = 12 }
            foreground = '#dbe0e8'; background = '#0e1014'
            cursorShape = 'bar'; opacity = 100; useAcrylic = $false
            backgroundImage = $null; padding = '12'; closeOnExit = 'graceful'
        }) }
        $fragment | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $fragmentDir 'benchmark.json') -Encoding UTF8
        $packageName = if ($Target -eq 'windows-terminal-preview') { 'Microsoft.WindowsTerminalPreview' } else { 'Microsoft.WindowsTerminal' }
        $package = Get-AppxPackage -Name $packageName | Select-Object -First 1
        if (-not $package) { throw "$packageName is not installed" }
        $terminal = Join-Path $package.InstallLocation 'wt.exe'
        Start-Process -FilePath $terminal -ArgumentList (@('-w','new','--size','110,36','new-tab',
            '-p','"w11term loopback benchmark"','--title',('"' + $Target + ' / OpenSSH loopback"'),
            '--suppressApplicationTitle', $ssh) + $sshArgs) -WorkingDirectory $project -WindowStyle Normal | Out-Null
    }
}
