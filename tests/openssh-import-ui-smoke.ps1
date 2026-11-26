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
        if ($line -cmatch '^(hostname|user|port|proxyjump|hostkeyalias|connecttimeout|serveraliveinterval|serveralivecountmax) (.*)$') {
            $endpoint[$Matches[1]] = $Matches[2]
        }
    }
    return $endpoint
}
function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $entry = Find-Element $Name $button
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
}
function Field([string]$Name) {
    (Wait-Until $Name { Find-Element $Name ([System.Windows.Automation.ControlType]::Edit) }).GetCurrentPattern(
        [System.Windows.Automation.ValuePattern]::Pattern)
}
function Write-PublicIdentity([string]$Path, [string]$Blob) {
    [IO.File]::WriteAllText($Path, "ssh-ed25519 $Blob import-fixture`n", $utf8)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return 'SHA256:' + [Convert]::ToBase64String($sha.ComputeHash([Convert]::FromBase64String($Blob))).TrimEnd('=')
    } finally { $sha.Dispose() }
}
try {
    # Absolute local paths keep both readers inside the selected fixture even
    # when the system client's -F config lives outside the user's SSH directory.
    Write-Config $config ('Include "' + $fragments.Replace('\','/') + '/*.conf"')
    $prior = Join-Path $state 'previous.conf'
    Write-Config $prior "Host previous`n    User pilot`n    HostName previous.test`n"
    $destinationIdentity = (Join-Path $state 'build identity.pub').Replace('\','/')
    $gatewayIdentity = (Join-Path $state 'gateway identity').Replace('\','/')
    $destinationFingerprint = Write-PublicIdentity $destinationIdentity 'AAAAC3NzaC1lZDI1NTE5AAAAILIG2T/B0l0gaqj3puu510tu9N1OkQ4znY3LYuEm5zCF'
    $gatewayFingerprint = Write-PublicIdentity $gatewayIdentity 'AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti'
    Write-Config (Join-Path $fragments '10-destination.conf') @"
Host via
    HostName target.test
    User pilot
    Port 2222
    ProxyJump gateway
    ConnectTimeout 23
    ServerAliveInterval 15
    HostKeyAlias Build.Key.Test
    IdentityFile "$destinationIdentity"
    IdentitiesOnly yes
"@
    Write-Config (Join-Path $fragments '20-gateway.conf') @"
Host gateway
    HostName gateway.test
    HostKeyAlias Bastion.Key.Test
    User jump
    Port 2201
    ConnectTimeout 11
    ServerAliveInterval 0
    IdentityFile "$gatewayIdentity"
    IdentitiesOnly yes
"@
    Write-Config (Join-Path $fragments '30-blocked.conf') @'
# This alias cannot be represented by a native SSH profile.
Host blocked
    ProxyCommand never-executed %h
'@
    Write-Config (Join-Path $fragments '40-web.conf') @'
Host web
    HostName web.test
    User pilot
    Port 2300
    HostKeyAlias Web.Key.Test
'@
    [IO.File]::WriteAllText($profiles,'{"profiles":{}}',$utf8)
    $env:W11TERM_DATA_DIR = $data
    $env:W11TERM_SETTINGS_PATH = Join-Path $state 'settings.json'
    $env:W11TERM_UI_SMOKE = '1'
    $env:W11TERM_IMPORT_SMOKE = $config
    $log = Join-Path $state 'stderr.log'
    $terminal = Start-Process -FilePath $gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError $log
    # Keep the process handle so the exit code remains readable after the window closes.
    $null = $terminal.Handle
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
    if (-not $terminal.WaitForExit(30000)) { throw "OpenSSH import smoke did not close the GUI. See $log" }
    $code = $terminal.ExitCode
    if ($code -ne 0 -or -not (Select-String -LiteralPath $log -SimpleMatch 'OpenSSH import UI smoke passed')) {
        throw "OpenSSH import smoke failed (exit $code, $log). $((Get-Content -LiteralPath $log -Tail 15) -join [Environment]::NewLine)"
    }
    $saved = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
    $via = $saved.via
    if (@($saved.psobject.Properties).Count -ne 1 -or $via.host -cne 'target.test' -or $via.user -cne 'pilot' -or $via.port -ne 2222 -or $via.proxy_jump.Count -ne 1 -or $via.terminal_type -cne 'xterm-sixel') {
        throw "Unexpected saved destination or profile edit. See $profiles"
    }
    $hop = $via.proxy_jump[0]
    if ($hop.host -cne 'gateway.test' -or $hop.user -cne 'jump' -or $hop.port -ne 2201) { throw "Unexpected saved included gateway. See $profiles" }
    if ($via.auth -cne 'agent-key' -or $via.agent_fingerprint -cne $destinationFingerprint -or
        $hop.auth -cne 'agent-key' -or $hop.agent_fingerprint -cne $gatewayFingerprint) {
        throw "The imported destination and jump did not retain their public key fingerprints. See $profiles"
    }
    if ($via.host_key_alias -cne 'build.key.test' -or $hop.host_key_alias -cne 'bastion.key.test') { throw "Host key aliases were lost during import or edit. See $profiles" }
    if ($via.keepalive.interval_seconds -ne 15 -or $via.keepalive.max_unanswered -ne 3 -or
        $hop.keepalive.interval_seconds -ne 0) { throw "Keepalive policy was lost during import or edit. See $profiles" }
    if ($via.connect_timeout_seconds -ne 23 -or $hop.connect_timeout_seconds -ne 11) {
        throw "Connection timeouts were lost during import or edit. See $profiles"
    }
    $sshExe = Join-Path $env:SystemRoot 'System32\OpenSSH\ssh.exe'
    if (Test-Path -LiteralPath $sshExe -PathType Leaf) {
        $expected = Ssh-Reference 'via'
        $gateway = Ssh-Reference 'gateway'
        if ($expected.proxyjump -cne 'gateway' -or $expected.hostname -cne $via.host -or $expected.user -cne $via.user -or [int]$expected.port -ne $via.port -or
            $gateway.hostname -cne $hop.host -or $gateway.user -cne $hop.user -or [int]$gateway.port -ne $hop.port -or
            $expected.hostkeyalias -cne $via.host_key_alias -or $gateway.hostkeyalias -cne $hop.host_key_alias -or
            [int]$expected.connecttimeout -ne $via.connect_timeout_seconds -or
            [int]$gateway.connecttimeout -ne $hop.connect_timeout_seconds -or
            [int]$expected.serveraliveinterval -ne $via.keepalive.interval_seconds -or
            [int]$expected.serveralivecountmax -ne $via.keepalive.max_unanswered -or
            [int]$gateway.serveraliveinterval -ne $hop.keepalive.interval_seconds) {
            throw "Saved profile differs from Windows OpenSSH -G. See $profiles"
        }
        Write-Output 'Windows OpenSSH -G independently confirmed the destination, gateway, timeouts, and keepalive policy.'
    }

    Remove-Item Env:W11TERM_UI_SMOKE, Env:W11TERM_IMPORT_SMOKE
    [IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH,'{"shortcuts":{"command_palette":"F7"}}',$utf8)
    $groupLog = Join-Path $state 'group-stderr.log'
    $terminal = Start-Process -FilePath $gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError $groupLog
    $app = $terminal
    $window = Wait-InputWindow
    Run-Palette 'Settings'
    Click ('Import OpenSSH config' + [char]0x2026)
    (Field 'OpenSSH config path').SetValue('')
    $null = Wait-Until 'empty path disables Preview hosts' {
        $entry = Find-Element 'Preview hosts' $button
        $entry -and !$entry.Current.IsEnabled
    }
    (Field 'OpenSSH config path').SetValue($config)
    Click 'Preview hosts'
    $groupAction = 'Create SSH group from aliases' + [char]0x2026
    $null = Wait-Until 'imported aliases' { Find-Element $groupAction $button }
    $picker = Wait-Until 'host alias picker' {
        Find-Element 'Host alias' ([System.Windows.Automation.ControlType]::ComboBox)
    }
    $picker.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    $gatewayEntry = Wait-Until 'gateway option' {
        Find-Element 'gateway' ([System.Windows.Automation.ControlType]::ListItem)
    }
    $gatewayEntry.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'gateway destination preview' {
        Find-Element "Destination`njump@gateway.test:2201 (SSH agent key $gatewayFingerprint, host key alias bastion.key.test)"
    }
    Click 'Save profile'
    $null = Wait-Until 'gateway saved for reuse' { Find-Element 'Saved profile gateway' }
    Click $groupAction
    $checkBox = [System.Windows.Automation.ControlType]::CheckBox
    foreach ($alias in @('gateway','web','via','blocked')) {
        $check = Wait-Until "reviewed alias $alias" { Find-Element "Include OpenSSH alias $alias" $checkBox }
        if ($check.Current.IsEnabled -ne ($alias -in @('gateway','web'))) {
            throw "Unexpected review availability for $alias"
        }
    }
    (Field 'OpenSSH group name').SetValue('Imported fleet')
    Click 'Check shown aliases'
    $dot = [char]0xb7
    $null = Wait-Until 'one existing and one new profile checked' {
        Find-Element "2 checked $dot 1 new $dot 1 existing $dot Page 1 of 1"
    }
    (Field 'Filter OpenSSH aliases').SetValue('WEB')
    $null = Wait-Until 'filter hides gateway without clearing either check' {
        !(Find-Element 'Include OpenSSH alias gateway' $checkBox) -and
            (Find-Element "2 checked $dot 1 new $dot 1 existing $dot Page 1 of 1")
    }
    (Field 'Filter OpenSSH aliases').SetValue('VIA')
    $null = Wait-Until 'filter isolates the conflicting alias' {
        (Find-Element 'Include OpenSSH alias via' $checkBox) -and
            !(Find-Element 'Include OpenSSH alias web' $checkBox)
    }
    (Field 'Profile name for OpenSSH alias via').SetValue('web')
    (Wait-CheckBox 'Include OpenSSH alias via' Off).Toggle()
    $null = Wait-Until 'duplicate draft names are flagged even when one alias is hidden' {
        $save = Find-Element 'Save SSH group' $button
        ($save -and !$save.Current.IsEnabled) -and (Find-Element 'Profile web is selected more than once')
    }
    (Field 'Profile name for OpenSSH alias via').SetValue('via with jump')
    $null = Wait-Until 'the corrected draft can save all three profiles' {
        $save = Find-Element 'Save SSH group' $button
        ($save -and $save.Current.IsEnabled) -and (Find-Element "3 checked $dot 2 new $dot 1 existing $dot Page 1 of 1")
    }
    $concurrent = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    $concurrent.profiles | Add-Member -NotePropertyName 'web' -NotePropertyValue $concurrent.profiles.via
    [IO.File]::WriteAllText($profiles, ($concurrent | ConvertTo-Json -Depth 30), $utf8)
    Click 'Save SSH group'
    $null = Wait-Until 'a late name collision is reported without closing the review' {
        Find-Element 'Profile web already exists'
    }
    $afterRace = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    if ($afterRace.ssh_host_groups.'Imported fleet' -or $afterRace.profiles.'via with jump' -or
            @($afterRace.profiles.psobject.Properties).Count -ne 3) {
        throw 'A late profile collision saved part of the group'
    }
    (Field 'Filter OpenSSH aliases').SetValue('WEB')
    (Field 'Profile name for OpenSSH alias web').SetValue('Imported web')
    Click 'Save SSH group'
    $null = Wait-Until 'saved imported group' { Find-Element 'Saved SSH group Imported fleet' }
    $catalog = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    if (($catalog.ssh_host_groups.'Imported fleet' -join ',') -cne 'gateway,via with jump,Imported web' -or
        @($catalog.profiles.psobject.Properties).Count -ne 5 -or
        $catalog.profiles.gateway.host_key_alias -cne 'bastion.key.test' -or
        $catalog.profiles.'Imported web'.host -cne 'web.test' -or $catalog.profiles.'Imported web'.port -ne 2300 -or
        $catalog.profiles.'Imported web'.host_key_alias -cne 'web.key.test' -or
        $catalog.profiles.via.terminal_type -cne 'xterm-sixel' -or
        $catalog.profiles.web.terminal_type -cne 'xterm-sixel' -or
        $catalog.profiles.'via with jump'.host -cne 'target.test' -or
        $catalog.profiles.'via with jump'.host_key_alias -cne 'build.key.test' -or
        $catalog.profiles.gateway.agent_fingerprint -cne $gatewayFingerprint -or
        $catalog.profiles.'via with jump'.agent_fingerprint -cne $destinationFingerprint -or
        $catalog.profiles.'via with jump'.proxy_jump[0].agent_fingerprint -cne $gatewayFingerprint -or
        $catalog.profiles.'via with jump'.proxy_jump[0].host_key_alias -cne 'bastion.key.test') {
        throw "Unexpected saved group membership or profile options. See $profiles"
    }

    Run-Palette 'Settings'
    Click 'Edit profile via'
    $interval = Field 'SSH keepalive interval (seconds)'
    $unanswered = Field 'Unanswered keepalives allowed'
    $retryWindow = Field 'SSH retry window (seconds)'
    $connectTimeout = Field 'SSH connection timeout (seconds)'
    if ((Field 'SSH agent key fingerprint').Current.Value -cne $destinationFingerprint) {
        throw 'The profile editor did not restore the imported agent fingerprint'
    }
    if ($interval.Current.Value -cne '15' -or $unanswered.Current.Value -cne '3') {
        throw 'The profile editor did not show the imported keepalive values'
    }
    if ($retryWindow.Current.Value -cne '300') { throw 'The editor did not load the default retry window' }
    if ($connectTimeout.Current.Value -cne '23') { throw 'The editor did not load the imported connection timeout' }
    $interval.SetValue('40')
    $unanswered.SetValue('6')
    $retryWindow.SetValue('45')
    $connectTimeout.SetValue('31')
    $null = Wait-Until 'edited SSH recovery fields' {
        (Field 'SSH keepalive interval (seconds)').Current.Value -ceq '40' -and
            (Field 'Unanswered keepalives allowed').Current.Value -ceq '6' -and
            (Field 'SSH retry window (seconds)').Current.Value -ceq '45' -and
            (Field 'SSH connection timeout (seconds)').Current.Value -ceq '31'
    }
    Click 'Save profile changes'
    $null = Wait-Until 'saved keepalive changes' { Find-Element 'Changes saved.' }
    $updated = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles.via
    if ($updated.keepalive.interval_seconds -ne 40 -or $updated.keepalive.max_unanswered -ne 6 -or
        $updated.proxy_jump[0].keepalive.interval_seconds -ne 0 -or $updated.player.reconnect_window_seconds -ne 45 -or
        $updated.connect_timeout_seconds -ne 31 -or $updated.proxy_jump[0].connect_timeout_seconds -ne 11 -or
        $updated.agent_fingerprint -cne $destinationFingerprint -or $updated.proxy_jump[0].agent_fingerprint -cne $gatewayFingerprint) {
        throw "Editing destination recovery changed the wrong policy. See $profiles"
    }
    Write-Output "OpenSSH import native smoke passed. Artifacts in $state"
} finally {
    if ($terminal -and -not $terminal.HasExited) { $terminal.Kill(); $terminal.WaitForExit() }
    foreach ($name in $variables) {
        if ($null -eq $previous[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name,$previous[$name],'Process') }
    }
}
