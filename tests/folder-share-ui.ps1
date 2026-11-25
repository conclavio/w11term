[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_FOLDER_SHARE_SMOKE_DIR = $State
$leaf = 'Selected folder'
$selectedDir = Join-Path $State $leaf
# Compare the full directory Windows resolved. Explorer may expand an 8.3 TEMP
# path (e.g. RUNNER~1) before returning SIGDN_FILESYSPATH.
$canonicalDir = [FixtureDialog]::DirectoryPath($selectedDir)
$profiles = Join-Path $State 'profiles.json'
$savedProfile = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'palette-ssh'
$account = "$($savedProfile.user)@$($savedProfile.host):$($savedProfile.port)"
$identityLabel = 'palette-ssh ' + [char]0xb7 + " $account"
$menuName = 'Shared folders' + [char]0x2026
$add = 'Add folder' + [char]0x2026
$button = [System.Windows.Automation.ControlType]::Button

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $element = Find-Element $Name $button
        if ($element -and $element.Current.IsEnabled) { $element }
    })
}
function Share-Menu {
    Click 'Tab menu'
    Wait-Until 'Shared folders menu item' { Find-Element $menuName ([System.Windows.Automation.ControlType]::MenuItem) }
}
function Open-Shares {
    $item = Share-Menu
    if (!$item.Current.IsEnabled) { throw "Disabled shared folders item for verified SSH pane $remote" }
    Invoke-Button $item
    $null = Wait-Until 'verified profile, account and port' { Find-Element $identityLabel }
}
function Folder-Dialog {
    Find-OwnedDialog 'Choose a folder to share'
}
function Same-SelectedFolder([string]$Path) {
    # Only consider absolute paths on the disposable fixture's local drive.
    if ($Path -notmatch '^[A-Za-z]:[\\/]' -or $Path[0] -ine $selectedDir[0]) { return $false }
    if (![IO.Directory]::Exists($Path)) { return $false }
    return [FixtureDialog]::DirectoryPath($Path) -ieq $canonicalDir
}
function Wait-SelectedPath {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $text = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
    $evidence = @{ paths = @(); draft = $false }
    try {
        $null = Wait-Until 'the full canonical selected directory in the shared folders panel' {
            $evidence.draft = $null -ne (Find-Element 'Mount name' ([System.Windows.Automation.ControlType]::Edit))
            $evidence.paths = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text) |
                ForEach-Object { $_.Current.Name } | Where-Object { $_ -match '^[A-Za-z]:[\\/]' })
            foreach ($path in $evidence.paths) { if (Same-SelectedFolder $path) { return $true } }
        }
    } catch {
        throw "$_ Expected '$selectedDir' ($canonicalDir). Reported paths: [$($evidence.paths -join ', ')]. Draft visible: $($evidence.draft). Picker open: $([bool](Folder-Dialog))."
    }
}
function Pick-Folder([int]$ButtonId, [string]$Item = $leaf) {
    Click $add
    $dialog = Wait-Until 'owned native folder picker' { Folder-Dialog }
    if ($ButtonId -eq 1) { Select-DialogItem $dialog $Item }
    $null = Wait-Until "folder picker button $ButtonId" { [FixtureDialog]::ClickButton($dialog, $ButtonId) }
    $null = Wait-Until 'folder picker dismissed' { !(Folder-Dialog) }
}
function Wait-FailedMount([string]$Name = 'smoke-share') {
    $reason = Wait-Until 'failed SSHFS setup and available retry' {
        $retry = Find-Element "Mount $Name" $button
        if (!$retry -or !$retry.Current.IsEnabled) { return }
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $text = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
        foreach ($row in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text)) {
            if ($row.Current.Name.StartsWith('Mount failed')) { return $row.Current.Name }
        }
    }
    if ($reason -notlike '*SSH server refused the session command*') {
        throw "Expected the loopback SSH peer to reject SSHFS setup for '$Name'; received '$reason'"
    }
    foreach ($action in "Open $Name in SFTP drawer", "Copy Linux path for $Name") {
        $control = Wait-Until "mounted folder shortcut '$action'" { Find-Element $action $button }
        if ($control.Current.IsEnabled) { throw "A failed mount enabled '$action'" }
    }
}
function Send-ToShell([string]$Text) {
    Click 'Back to terminal'
    Wait-Focus $remote
    foreach ($c in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10',$Text)
}
function Test-OfflineRevoke {
    $sessions = @((Control @('list')).sessions)
    if ($sessions.Count -ne 1 -or $sessions[0].label -ne 'Palette PowerShell') { throw 'Expected only the local PowerShell pane' }
    $local = $sessions[0].id
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    $before = Get-Content -Raw -LiteralPath $profiles
    $oldToken = ($before | ConvertFrom-Json).profiles.'palette-ssh'.folder_share_id
    if (!$oldToken) { throw 'Fixture is missing its saved folder permission' }

    Run-Palette 'Settings'
    Click 'Edit profile palette-ssh'
    $muteLabel = 'Mute this profile'
    $toggle = [System.Windows.Automation.TogglePattern]::Pattern
    $box = Wait-Until 'unsaved player setting' { Find-Element $muteLabel ([System.Windows.Automation.ControlType]::CheckBox) }
    if ($box.GetCurrentPattern($toggle).Current.ToggleState -ne 'Off') { throw 'Expected default mute state' }
    $box.GetCurrentPattern($toggle).Toggle()
    $revoke = 'Revoke all shared folders' + [char]0x2026
    Click $revoke
    Click 'Keep permissions'
    $null = Wait-Until 'canceled revoke ready to retry' { $b = Find-Element $revoke $button; $b -and $b.Current.IsEnabled }
    if ((Get-Content -Raw -LiteralPath $profiles) -cne $before) { throw 'Cancel modified the saved catalog' }
    # A second window changes the saved profile while this confirmation is open.
    Click $revoke
    $concurrent = $before | ConvertFrom-Json
    $concurrent.profiles.'palette-ssh' | Add-Member -NotePropertyName external_edit -NotePropertyValue 'preserve me'
    $utf8 = [Text.UTF8Encoding]::new($false)
    [IO.File]::WriteAllText($profiles, ($concurrent | ConvertTo-Json -Depth 30), $utf8)
    Click 'Confirm revoke all shared folders'
    $null = Wait-Until 'stale editor refused before revoking' {
        Find-Element 'Could not revoke folder permissions: Profile changed on disk; reopen it before revoking folder permissions'
    }
    $unchanged = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'palette-ssh'
    if ($unchanged.folder_share_id -cne $oldToken -or $unchanged.external_edit -cne 'preserve me') { throw 'Stale action changed the updated profile' }
    [IO.File]::WriteAllText($profiles, $before, $utf8)
    Click $revoke
    Click 'Confirm revoke all shared folders'
    $null = Wait-Until 'revoke confirmation names the saved profile' {
        Find-Element ('Revoked all shared folders for ' + [char]0x201c + 'palette-ssh' + [char]0x201d + '.')
    }
    if ((Find-Element $revoke $button).Current.IsEnabled) { throw 'Already revoked permission remains actionable' }
    if ((Find-Element $muteLabel).GetCurrentPattern($toggle).Current.ToggleState -ne 'On') { throw 'Revocation discarded the unsaved mute setting' }
    $saved = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'palette-ssh'
    if ($saved.folder_share_id -or !$saved.folder_share_epoch -or $saved.player.mute) { throw 'Revocation did not retire the permission or saved unrelated edits' }
    if (@((Control @('list')).sessions).Count -ne 1) { throw 'Revocation started an SSH connection' }
    Click 'Save profile changes'
    $null = Wait-Until 'unrelated draft can still save' {
        (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'palette-ssh'.player.mute
    }
    $remote = (Control @('open','--profile','palette-ssh','--activate')).pane
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    Open-Shares
    $null = Wait-Until 'reopened verified profile has no saved permissions' {
        Find-Element 'Choose a Windows folder to mount under ~/windows/ on this SSH account.'
    }
    if (Find-Element '~/windows/Selected-folder') { throw 'A revoked grant remounted on reconnect' }
    Pick-Folder 1
    Wait-SelectedPath
    Click 'Save folder'
    $null = Wait-Until 'new explicit folder choice after revocation' {
        $id = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'palette-ssh'.folder_share_id
        $id -and $id -cne $oldToken -and (Find-Element '~/windows/Selected-folder')
    }
    Wait-FailedMount 'Selected-folder'
    Send-ToShell 'share-shell-after-revocation'
}
for ($launch = 1; $launch -le 3; $launch++) {
    $initialProfile = if ($launch -eq 3) { '"Palette PowerShell"' } else { 'palette-ssh' }
    $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile',$initialProfile) -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State "share-ui-$launch.log")
    try {
        $window = Wait-InputWindow
        $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
        if ($launch -eq 3) {
            Test-OfflineRevoke
            continue
        }
        $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
        $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
        if ($launch -eq 1) {
            Open-Shares
        } else {
            Click 'Tab menu'
            Invoke-Button (Wait-Until 'edit verified active profile' { Find-Element ('Edit current profile' + [char]0x2026) })
            Click $menuName
            $null = Wait-Until 'profile editor opens the original live SSH authority' { Find-Element $identityLabel }
        }
        if ($launch -eq 1) {
            Pick-Folder 2 # Cancel
            $null = Wait-Until 'canceled picker restored Add' { $b = Find-Element $add $button; $b -and $b.Current.IsEnabled }
            if (Find-Element 'Mount name') { throw 'Cancel left behind a share draft' }
            Pick-Folder 1
            Wait-SelectedPath
            $field = Wait-Until 'editable remote mount name' { Find-Element 'Mount name' ([System.Windows.Automation.ControlType]::Edit) }
            $value = $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
            $value.SetValue('../invalid')
            $null = Wait-Until 'invalid name disables sharing' { $b = Find-Element 'Save folder' $button; $b -and !$b.Current.IsEnabled }
            $value.SetValue('smoke-share')
            $null = Wait-Until 'specific write consent names the verified account and its file access' {
                Find-Element "Write access lets $account create, edit and delete files in this Windows folder."
            }
            (Wait-CheckBox 'Allow write access' Off).Toggle()
            $null = Wait-Until 'review names both the write mode and guest destination' {
                Find-Element 'Remember this folder and mount it read/write at ~/windows/smoke-share'
            }
            (Wait-CheckBox 'Mount on connect' On).Toggle()
            $null = Wait-CheckBox 'Mount on connect' Off
            Click 'Save folder'
            $null = Wait-Until 'saved remote mount path' { Find-Element '~/windows/smoke-share' }
            Wait-FailedMount
            $null = Wait-Until 'saved row displays its write access' { Find-Element 'Read/write' }
            if (Find-Element 'Allow write access') { throw 'A saved grant offered an in-place access change' }
            $grantsDir = Join-Path $env:W11TERM_DATA_DIR 'folder-shares'
            $grantFiles = @(Get-ChildItem -LiteralPath $grantsDir -Filter '*.json' -File)
            if ($grantFiles.Length -ne 1) { throw 'Expected exactly one isolated local consent file' }
            $consentFile = $grantFiles[0].FullName
            $consent = @(Get-Content -Raw -LiteralPath $consentFile | ConvertFrom-Json)
            if ($consent.Length -ne 1 -or !(Same-SelectedFolder $consent[0].path) -or $consent[0].name -ne 'smoke-share' -or
                $consent[0].auto_mount -isnot [bool] -or $consent[0].auto_mount -or
                $consent[0].writable -isnot [bool] -or !$consent[0].writable) {
                throw 'Persisted consent did not match the selected test directory and mount name'
            }
            Pick-Folder 1 'data'
            (Wait-CheckBox 'Allow write access' Off).Toggle()
            $null = Wait-Until 'review writable data directory request' {
                Find-Element 'Remember this folder and mount it read/write at ~/windows/data'
            }
            Click 'Save folder'
            $null = Wait-Until 'write access to application state refused' {
                Find-Element ('Mount failed ' + [char]0xb7 + " Write access would expose w11term's data directory or profile catalog. Choose a different folder or share it read-only.")
            }
            $savedGrants = @(Get-Content -Raw -LiteralPath $consentFile | ConvertFrom-Json)
            if ($savedGrants.Length -ne 1 -or $savedGrants[0].name -ne 'smoke-share') {
                throw 'Protected app data was persisted as a writable grant'
            }
            Click 'Forget data'
            $null = Wait-Until 'rejected data directory removed' { !(Find-Element '~/windows/data') }
            (Wait-CheckBox 'Mount on connect for smoke-share' Off).Toggle()
            $null = Wait-Until 'enabled preference in the saved grant' {
                $saved = @(Get-Content -Raw -LiteralPath $consentFile | ConvertFrom-Json)
                $saved.Count -eq 1 -and $saved[0].name -ceq 'smoke-share' -and $saved[0].auto_mount -ne $false
            }
            (Wait-CheckBox 'Mount on connect for smoke-share' On).Toggle()
            $null = Wait-Until 'disabled preference in the saved grant' {
                $saved = @(Get-Content -Raw -LiteralPath $consentFile | ConvertFrom-Json)
                $saved.Count -eq 1 -and $saved[0].auto_mount -eq $false
            }
            $null = Wait-CheckBox 'Mount on connect for smoke-share' Off
            Wait-FailedMount
            Send-ToShell 'share-shell-before-restart'
            $null = Control @('open','--profile','Palette PowerShell')
            $local = Wait-Until 'new PowerShell' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
            $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
            Picker-Query 'Palette PowerShell'
            Press-Key 13
            Wait-Focus $local
            if ((Share-Menu).Current.IsEnabled) { throw 'Local PowerShell offered folder sharing' }
        } else {
            $null = Wait-Until 'persisted share in restarted process' { Find-Element '~/windows/smoke-share' }
            $null = Wait-Until 'persisted share retained its write access' { Find-Element 'Read/write' }
            Wait-SelectedPath
            $null = Wait-CheckBox 'Mount on connect for smoke-share' Off
            $null = Wait-Until 'manual-only folder stays unmounted on reconnect' { Find-Element 'Unmounted' }
            Click 'Mount smoke-share'
            Wait-FailedMount
            $null = Wait-CheckBox 'Mount on connect for smoke-share' Off
            Click 'Forget smoke-share'
            $null = Wait-Until 'forgotten share removed' { (Find-Element 'Requested removal of smoke-share') -and !(Find-Element '~/windows/smoke-share') }
            if ((Test-Path -LiteralPath $consentFile) -and (Get-Content -Raw -LiteralPath $consentFile).Trim() -ne '[]') { throw 'A folder grant survived Forget' }
            Send-ToShell 'share-shell-after-restart'
            Run-Palette $menuName
            $null = Wait-Until 'palette opens verified panel' { Find-Element $identityLabel }
            Pick-Folder 1
            Wait-SelectedPath
            $null = Wait-CheckBox 'Allow write access' Off
            $null = Wait-Until 'reselecting the same Windows directory starts a new read-only consent' {
                Find-Element 'Remember this folder and mount it read-only at ~/windows/Selected-folder'
            }
            Click 'Save folder'
            $null = Wait-Until 'saved permission ready for offline revocation' { Find-Element 'Mount Selected-folder' $button }
            Wait-FailedMount 'Selected-folder'
            $null = Wait-Until 'new saved row is read-only' { Find-Element 'Read-only' }
            $newGrant = @(Get-Content -Raw -LiteralPath $consentFile | ConvertFrom-Json)
            if ($newGrant.Count -ne 1 -or $newGrant[0].PSObject.Properties['writable']) {
                throw 'Default sharing retained earlier write consent'
            }
            Click $add
            $dialog = Wait-Until 'owned picker before disconnect' { Folder-Dialog }
            Select-DialogItem $dialog $leaf
            $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64','BA==')
            $stale = 'This folder view has expired. Reopen Shared folders from a connected SSH pane.'
            $null = Wait-Until 'verified connection revoked with the picker open' { Find-Element $stale }
            $null = Wait-Until 'finish stale real folder picker' { [FixtureDialog]::ClickButton($dialog, 1) }
            $null = Wait-Until 'stale picker completion safely discarded' { !(Folder-Dialog) -and (Find-Element $stale) }
            if ((Find-Element 'Mount name') -or (Find-Element $add)) { throw 'A stale picker returned folder access' }
        }
    } catch {
        $log = Get-Content -LiteralPath (Join-Path $State "share-ui-$launch.log") -Tail 15 -ErrorAction SilentlyContinue
        throw "$_ at $($_.ScriptStackTrace). GUI log: $log"
    } finally {
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
}
