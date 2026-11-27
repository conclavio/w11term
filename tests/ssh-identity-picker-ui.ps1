param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$keyFolder = Join-Path $State 'other keys'
$env:USERPROFILE = Join-Path $State 'ssh-home'
$sshFolder = Join-Path $env:USERPROFILE '.ssh'
$null = [IO.Directory]::CreateDirectory($keyFolder)
$null = [IO.Directory]::CreateDirectory($sshFolder)
$typed = 'other keys\typed key'
$chosen = Join-Path $keyFolder 'new key'
$homeKey = Join-Path $sshFolder 'home key'
foreach ($path in (Join-Path $State $typed), $chosen, $homeKey) {
    [IO.File]::WriteAllText($path, 'test placeholder')
}
$catalog = [IO.File]::ReadAllText($profiles)
$menu = [System.Windows.Automation.ControlType]::MenuItem
$combo = [System.Windows.Automation.ControlType]::ComboBox
$edit = [System.Windows.Automation.ControlType]::Edit
$item = [System.Windows.Automation.ControlType]::ListItem
$value = [System.Windows.Automation.ValuePattern]::Pattern
$pickTitle = 'Choose SSH private key'
$browseLabel = 'Browse for private key'

function Click([string]$Name, $Type = $button) {
    Invoke-Button (Wait-Until "enabled '$Name' control" {
        $control = Find-Element $Name $Type
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}
function Key-Field {
    $field = Wait-Until 'private key path field' { Find-Element 'Private key file' $edit }
    return $field.GetCurrentPattern($value)
}
function Set-Field([string]$Name, [string]$Text) {
    $field = Wait-Until "'$Name' field" { Find-Element $Name $edit }
    $field.SetFocus()
    $field = Wait-Until "'$Name' editor focused" {
        $current = Find-Element $Name $edit -Visible
        if ($current -and $current.Current.HasKeyboardFocus) { $current }
    }
    $field.GetCurrentPattern($value).SetValue($Text)
    $null = Wait-Until "'$Name' contains the entered value" {
        (Find-Element $Name $edit).GetCurrentPattern($value).Current.Value -ceq $Text
    }
}
function Wait-Browse {
    $null = Wait-Until 'key picker releases the draft' {
        $control = Find-Element $browseLabel $button
        $control -and $control.Current.IsEnabled
    }
}
function File-Sha256([string]$Path) {
    $hash = [System.Security.Cryptography.SHA256]::Create()
    try {
        [BitConverter]::ToString($hash.ComputeHash([IO.File]::ReadAllBytes($Path))).Replace('-', '').ToLowerInvariant()
    } finally {
        $hash.Dispose()
    }
}
function Pick-Key([string]$Name) {
    Click $browseLabel
    $dialog = Wait-Until 'owned SSH key picker' { Find-OwnedDialog $pickTitle }
    if ($Name) {
        Select-DialogItem $dialog $Name
        $id = 1
    } else { $id = 2 }
    $null = Wait-Until 'activate picker button' { [FixtureDialog]::ClickButton($dialog, $id) }
    $null = Wait-Until 'close SSH key picker' { !(Find-OwnedDialog $pickTitle) }
    Wait-Browse
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    Click 'New tab'
    Click ('Quick connect' + [char]0x2026) $menu
    $auth = Wait-Until 'authentication chooser' { Find-Element 'Authentication' $combo }
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $auth.GetCurrentPattern($expand).Expand()
    $key = Wait-Until 'visible private-key auth choice' {
        $choice = Find-Element 'Private key file' $item
        if ($choice -and !$choice.Current.IsOffscreen) { $choice }
    }
    $key.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $popup = (Find-Element 'Authentication' $combo).GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
    (Key-Field).SetValue($typed)
    $null = Wait-Until 'manual key path in the draft' { (Key-Field).Current.Value -ceq $typed }
    Pick-Key
    if ((Key-Field).Current.Value -cne $typed) { throw 'Cancelling the picker changed the manual path' }
    Pick-Key 'new key'
    if (![FixtureDialog]::SameFile($chosen, (Key-Field).Current.Value)) { throw 'Quick Connect did not use the selected path' }
    if ([IO.File]::ReadAllText($profiles) -cne $catalog) { throw 'Quick Connect browsing changed the catalog' }

    Click 'Tab menu'
    Click 'Settings' $menu
    Click 'Edit profile fixture'
    $null = Wait-Until 'profile key has an unavailable starting folder' {
        (Key-Field).Current.Value -like '*missing-key-folder*'
    }
    Pick-Key 'home key'
    if (![FixtureDialog]::SameFile($homeKey, (Key-Field).Current.Value)) { throw 'The profile editor did not use the selected path' }
    Click 'Save profile changes'
    $null = Wait-Until 'profile changes saved' { Find-Element 'Changes saved.' }
    $saved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture
    if ($saved.auth -cne 'identity' -or ![FixtureDialog]::SameFile($homeKey, $saved.identity)) {
        throw 'The profile did not save the selected identity path'
    }
    if ([IO.File]::ReadAllText($homeKey) -cne 'test placeholder' -or [IO.File]::ReadAllText($chosen) -cne 'test placeholder') {
        throw 'The key picker changed a selected file'
    }

    $bundleName = 'Fleet dotfiles.json'
    $bundle = Join-Path $State $bundleName
    [IO.File]::WriteAllText($bundle, '{"version":2,"bash":"export EDITOR=vi"}')
    Set-NativeExpansion 'SSH dotfiles' $true
    Click 'Approve dotfiles sync'
    $null = Wait-Until 'fingerprint required' {
        Find-Element 'Enter the verified SHA256 host-key fingerprint for this account'
    }
    Click 'Browse for dotfiles bundle'
    $bundleTitle = 'Choose dotfiles bundle'
    $bundleDialog = Wait-Until 'owned dotfiles bundle picker' { Find-OwnedDialog $bundleTitle }
    Select-DialogItem $bundleDialog $bundleName
    $null = Wait-Until 'select dotfiles bundle' { [FixtureDialog]::ClickButton($bundleDialog, 1) }
    $null = Wait-Until 'use selected dotfiles bundle' {
        $field = Find-Element 'Dotfiles bundle' $edit
        if ($field) { [FixtureDialog]::SameFile($bundle, $field.GetCurrentPattern($value).Current.Value) }
    }
    $fingerprint = 'SHA256:' + ('A' * 43)
    Set-Field 'Remote SFTP home' '/home/test'
    Set-Field 'Dotfiles host-key fingerprint' $fingerprint
    Click 'Approve dotfiles sync'
    $null = Wait-Until 'unsupported bundle rejected' {
        Find-Element 'Checking the dotfiles bundle: Unsupported dotfiles bundle version'
    }
    [IO.File]::WriteAllText($bundle, '{"version":1,"bash":"export EDITOR=vi"}')
    Click 'Approve dotfiles sync'
    $approval = 'Approved for test@example.test:22 ' + [char]0x00b7 + ' /home/test'
    $null = Wait-Until 'approval for account and home' { Find-Element $approval }
    Click 'Save profile changes'
    $null = Wait-Until 'dotfiles approval saved' {
        $dotfiles = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.dotfiles
        $dotfiles.shell -ceq 'bash' -and $dotfiles.host -ceq 'example.test' -and
            $dotfiles.user -ceq 'test' -and $dotfiles.port -eq 22 -and
            $dotfiles.home -ceq '/home/test' -and $dotfiles.fingerprint -ceq $fingerprint -and
            [FixtureDialog]::SameFile($bundle, $dotfiles.bundle)
    }
    Click 'Disable dotfiles sync'
    $null = Wait-Until 'dotfiles sync disabled in the editor' { Find-Element 'Sync off' }
    Click 'Save profile changes'
    $null = Wait-Until 'dotfiles approval removed' {
        !(([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.dotfiles)
    }

    Set-NativeExpansion 'SSH dotfiles' $false
    Set-NativeExpansion 'SSH tools' $true
    $toolName = 'Fleet tools.json'
    $toolBundle = Join-Path $State $toolName
    [IO.File]::WriteAllText($toolBundle, ('{"version":1,"name":"portable","tools":[{"name":"rg","builds":[{"architecture":"x86_64","path":"rg","sha256":"' + ('a' * 64) + '"}]}]}'))
    Click 'Browse for tool bundle'
    $toolDialog = Wait-Until 'owned tool bundle picker' { Find-OwnedDialog 'Choose SSH tool bundle' }
    Select-DialogItem $toolDialog $toolName
    $null = Wait-Until 'choose the tool manifest' { [FixtureDialog]::ClickButton($toolDialog, 1) }
    $null = Wait-Until 'tool manifest in draft' {
        $field = Find-Element 'Tool bundle manifest' $edit
        if ($field) { [FixtureDialog]::SameFile($toolBundle, $field.GetCurrentPattern($value).Current.Value) }
    }
    $example = Join-Path $PSScriptRoot '../examples/tool-bundles/essentials.json'
    $starterDigest = File-Sha256 $example
    $starterDirectory = Join-Path $env:W11TERM_DATA_DIR "tools/bundles/$starterDigest"
    $starterManifest = Join-Path $starterDirectory 'linux-essentials.json'
    $null = [IO.Directory]::CreateDirectory($starterDirectory)
    [IO.File]::WriteAllText($starterManifest, 'edited starter')
    Click 'Use Linux essentials'
    $null = Wait-Until 'edited starter reported without downloading' {
        Find-Element 'The saved Linux essentials manifest was modified. Browse for that manifest to use your copy, or move it aside and retry.'
    }
    $toolField = (Find-Element 'Tool bundle manifest' $edit).GetCurrentPattern($value).Current.Value
    if (![FixtureDialog]::SameFile($toolBundle, $toolField) -or
        [IO.File]::ReadAllText($starterManifest) -cne 'edited starter') {
        throw 'Preparing the starter overwrote an existing manifest or changed the draft on failure'
    }
    [IO.File]::Delete($starterManifest)
    Set-Field 'Tool bundle SFTP home' '/home/test'
    Set-Field 'Tool bundle host-key fingerprint' $fingerprint
    $pathMode = Wait-Until 'tool PATH selector' { Find-Element 'Tool bundle shell PATH' $combo }
    $pathMode.SetFocus()
    $modePopup = $pathMode.GetCurrentPattern($expand)
    $modePopup.Expand()
    $bash = Wait-Until 'Bash PATH selection' { Find-Element 'Manage Bash PATH' $item -Visible }
    $bash.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    if ($modePopup.Current.ExpandCollapseState -eq 'Expanded') { $modePopup.Collapse() }
    Click 'Review tool bundle'
    $review = 'portable ' + [char]0x2192 + ' test@example.test:22 ' + [char]0x00b7 + ' /home/test/.local/bin'
    $null = Wait-Until 'tool bundle and launcher directory reviewed' { Find-Element $review }
    Click 'Approve tool sync'
    $null = Wait-Until 'tool sync approved' { Find-Element 'Approved tool bundle: portable' }
    Click 'Save profile changes'
    $null = Wait-Until 'tool approvals persisted' {
        $tools = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.tool_bundle
        $tools.name -ceq 'portable' -and $tools.bin_directory -ceq '.local/bin' -and
            $tools.host -ceq 'example.test' -and $tools.home -ceq '/home/test' -and
            $tools.fingerprint -ceq $fingerprint -and $tools.add_to_path.shell -ceq 'bash' -and
            [FixtureDialog]::SameFile($toolBundle, $tools.bundle)
    }
    Click 'Disable tool sync'
    $null = Wait-Until 'tool sync disabled' { Find-Element 'Tool sync off' }
    Click 'Save profile changes'
    $null = Wait-Until 'tool approvals removed' {
        !(([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.tool_bundle)
    }

    if ($env:W11TERM_VERIFY_STARTER_DOWNLOAD -eq '1') {
        Click 'Use Linux essentials'
        Click 'Cancel tool download'
        $null = Wait-Until 'starter download cancelled' { Find-Element 'Tool download cancelled' }
        $toolField = (Find-Element 'Tool bundle manifest' $edit).GetCurrentPattern($value).Current.Value
        if (![FixtureDialog]::SameFile($toolBundle, $toolField)) { throw 'Cancelling changed the selected manifest' }

        Click 'Use Linux essentials'
        $null = Wait-Until 'verified starter builds ready' {
            Find-Element 'Linux essentials is ready. Review the bundle to approve sync.'
        }
        $toolField = (Find-Element 'Tool bundle manifest' $edit).GetCurrentPattern($value).Current.Value
        if (![FixtureDialog]::SameFile($starterManifest, $toolField)) { throw 'Starter manifest was not selected' }
        $starter = [IO.File]::ReadAllText($starterManifest) | ConvertFrom-Json
        if (@($starter.tools.builds).Count -ne 8) { throw 'Expected eight builds in the starter manifest' }
        foreach ($build in $starter.tools.builds) {
            $cached = Join-Path $env:W11TERM_DATA_DIR "tools/cache/v1/$($build.sha256)"
            if ((File-Sha256 $cached) -cne $build.sha256) {
                throw "Starter cache did not contain the pinned $($build.architecture) build"
            }
        }
        if (([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.tool_bundle) {
            throw 'Selecting the starter authorized deployment before review'
        }
        Click 'Review tool bundle'
        $review = 'linux-essentials ' + [char]0x2192 + ' test@example.test:22 ' + [char]0x00b7 + ' /home/test/.local/bin'
        $null = Wait-Until 'starter destination reviewed' { Find-Element $review }
        Click 'Approve tool sync'
        Click 'Save profile changes'
        $null = Wait-Until 'starter approval saved' {
            $tools = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.tool_bundle
            $tools.name -ceq 'linux-essentials' -and $tools.add_to_path.shell -ceq 'bash' -and
                [FixtureDialog]::SameFile($starterManifest, $tools.bundle)
        }
    }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
