[CmdletBinding()]
param([string]$GuiPath = (Join-Path $PSScriptRoot '..\target\debug\w11term.exe'))
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$gui = (Resolve-Path -LiteralPath $GuiPath).Path
$state = (New-Item -ItemType Directory -Force -Path (Join-Path $PSScriptRoot ('..\.local\openssh-import-smoke\' + [guid]::NewGuid().ToString('N')))).FullName
$config = Join-Path $state 'config'
$profiles = Join-Path $state 'profiles.json'
$fragments = (New-Item -ItemType Directory -Path (Join-Path $state 'host fragments')).FullName
$data = (New-Item -ItemType Directory -Path (Join-Path $state 'data')).FullName
$variables = @('W11TERM_DATA_DIR','W11TERM_SETTINGS_PATH','W11TERM_UI_SMOKE','W11TERM_IMPORT_SMOKE')
$previous = @{}
foreach ($name in $variables) { $previous[$name] = [Environment]::GetEnvironmentVariable($name,'Process') }
$terminal = $null
$utf8 = [Text.UTF8Encoding]::new($false)
function Write-Config([string]$Path, [string]$Content) {
    [IO.File]::WriteAllText($Path, $Content, $utf8)
    # Windows OpenSSH checks the ACL on Include files. These new fixture files
    # can inherit writable runner or sandbox groups from the checkout.
    $acl = Get-Acl -LiteralPath $Path
    $acl.SetAccessRuleProtection($true, $false)
    $acl.SetAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
        [Security.Principal.WindowsIdentity]::GetCurrent().User, 'FullControl', 'Allow'))
    Set-Acl -LiteralPath $Path -AclObject $acl
}
function Ssh-Reference([string]$Alias) {
    $lines = & $sshExe -T -G -F $config $Alias
    if ($LASTEXITCODE -ne 0) { throw "ssh.exe -G failed for fixture alias $Alias ($LASTEXITCODE)" }
    $endpoint = @{}
    foreach ($line in $lines) {
        if ($line -cmatch '^(hostname|user|port|proxyjump) (.*)$') { $endpoint[$Matches[1]] = $Matches[2] }
    }
    return $endpoint
}
try {
    # Absolute local paths keep both readers inside the selected fixture even
    # when the system client's -F config lives outside the user's SSH directory.
    Write-Config $config ('Include "' + $fragments.Replace('\','/') + '/*.conf"')
    $prior = Join-Path $state 'previous.conf'
    Write-Config $prior "Host previous`n    User pilot`n    HostName previous.test`n"
    Write-Config (Join-Path $fragments '10-destination.conf') @'
Host via
    HostName target.test
    User pilot
    Port 2222
    ProxyJump gateway
    ServerAliveInterval 15
'@
    Write-Config (Join-Path $fragments '20-gateway.conf') @'
Host gateway
    HostName gateway.test
    User jump
    Port 2201
'@
    Write-Config (Join-Path $fragments '30-blocked.conf') @'
# This alias cannot be represented by a native SSH profile.
Host blocked
    ProxyCommand never-executed %h
'@
    [IO.File]::WriteAllText($profiles,'{"profiles":{}}',$utf8)
    $env:W11TERM_DATA_DIR = $data
    $env:W11TERM_SETTINGS_PATH = Join-Path $state 'settings.json'
    $env:W11TERM_UI_SMOKE = '1'
    $env:W11TERM_IMPORT_SMOKE = $config
    $log = Join-Path $state 'stderr.log'
    $terminal = Start-Process -FilePath $gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError $log
    $app = $terminal
    $window = Wait-InputWindow
    $priorEndpoint = "Destination`npilot@previous.test:22 (SSH agent)"
    $null = Wait-Until 'the original config preview' { Find-Element $priorEndpoint }
    $title = 'Open OpenSSH config'
    $button = [System.Windows.Automation.ControlType]::Button
    foreach ($choice in @('Cancel', 'Open')) {
        Invoke-Button (Wait-Until 'Browse button' { Find-Element ('Browse' + [char]0x2026) $button })
        $dialog = Wait-Until 'owned native config picker' { Find-OwnedDialog $title }
        if ($choice -eq 'Open') {
            Select-DialogItem $dialog 'config'
            $buttonId = 1
        } else { $buttonId = 2 }
        $null = Wait-Until "enabled $choice in config picker" { [FixtureDialog]::ClickButton($dialog, $buttonId) }
        $null = Wait-Until 'config picker dismissed' { !(Find-OwnedDialog $title) }
        if ($choice -eq 'Cancel') {
            $null = Wait-Until 'cancellation preserving the preview and typed path' {
                $field = Find-Element 'OpenSSH config path' ([System.Windows.Automation.ControlType]::Edit)
                $field -and $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq $prior -and (Find-Element $priorEndpoint)
            }
            $before = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
            if (@($before.psobject.Properties).Count -ne 0) { throw 'Browsing unexpectedly saved a profile' }
        }
    }
    if (-not $terminal.WaitForExit(30000) -or $terminal.ExitCode -ne 0 -or -not (Select-String -LiteralPath $log -SimpleMatch 'OpenSSH import UI smoke passed')) {
        throw "OpenSSH import smoke failed ($log). $((Get-Content -LiteralPath $log -Tail 15) -join [Environment]::NewLine)"
    }
    $saved = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
    $via = $saved.via
    if (@($saved.psobject.Properties).Count -ne 1 -or $via.host -cne 'target.test' -or $via.user -cne 'pilot' -or $via.port -ne 2222 -or $via.proxy_jump.Count -ne 1 -or $via.terminal_type -cne 'xterm-sixel') {
        throw "Unexpected saved destination or profile edit. See $profiles"
    }
    $hop = $via.proxy_jump[0]
    if ($hop.host -cne 'gateway.test' -or $hop.user -cne 'jump' -or $hop.port -ne 2201) { throw "Unexpected saved included gateway. See $profiles" }
    $sshExe = Join-Path $env:SystemRoot 'System32\OpenSSH\ssh.exe'
    if (Test-Path -LiteralPath $sshExe -PathType Leaf) {
        $expected = Ssh-Reference 'via'
        $gateway = Ssh-Reference 'gateway'
        if ($expected.proxyjump -cne 'gateway' -or $expected.hostname -cne $via.host -or $expected.user -cne $via.user -or [int]$expected.port -ne $via.port -or
            $gateway.hostname -cne $hop.host -or $gateway.user -cne $hop.user -or [int]$gateway.port -ne $hop.port) {
            throw "Saved profile differs from Windows OpenSSH -G. See $profiles"
        }
        Write-Output 'Windows OpenSSH -G independently confirmed the destination and gateway.'
    }
    Write-Output "OpenSSH import native smoke passed. Artifacts in $state"
} finally {
    if ($terminal -and -not $terminal.HasExited) { $terminal.Kill(); $terminal.WaitForExit() }
    foreach ($name in $variables) {
        if ($null -eq $previous[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name,$previous[$name],'Process') }
    }
}
