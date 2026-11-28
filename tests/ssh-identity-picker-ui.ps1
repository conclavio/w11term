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
function Pick-LinuxExecutable([string]$Action, [string]$Name) {
    Click $Action
    $dialog = Wait-OwnedDialog 'Choose a Linux executable'
    $path = Join-Path $State $Name
    $null = Wait-Until "enter the absolute path for '$Name'" {
        [FixtureDialog]::SetFilename($dialog, $path, '')
    }
    $null = Wait-Until "accept '$Name' in the native picker" { [FixtureDialog]::ClickButton($dialog, 1) }
}
function Wait-DotfileControl([string]$Name, [System.Windows.Automation.ControlType]$Type) {
    $search = @{ scroll = $null }
    Wait-Until "visible dotfile control '$Name'" {
        $control = Find-Element $Name $Type
        if ($control -and $control.Current.IsEnabled) {
            if (!$control.Current.IsOffscreen) { return $control }
            Move-NativeControlIntoView $control $search
        }
    }
}
function Dotfile-Fragment([string]$Name) {
    (Wait-DotfileControl $Name $edit).GetCurrentPattern($value).Current.Value -replace '\r\n?', "`n"
}
function Set-DotfileFragment([string]$Name, [string]$Text) {
    $field = Wait-DotfileControl $Name $edit
    $field.SetFocus()
    $field.GetCurrentPattern($value).SetValue($Text.Replace("`n", "`r"))
    $null = Wait-Until "the multiline dotfile fragment '$Name' is retained" { (Dotfile-Fragment $Name) -ceq $Text }
}
function Import-ShellFragment([string]$Destination, [string]$Source) {
    $import = 'Import shell fragment'
    $search = @{ scroll = $null }
    Invoke-Button (Get-NativeMenuAction $import $Destination -VisibleItem -Reveal {
        Move-NativeControlIntoView (Find-Element $import $button) $search
    })
    $title = 'Choose a shell fragment file'
    $dialog = Wait-OwnedDialog $title
    if ($Source) {
        $null = Wait-Until "enter the path for '$([IO.Path]::GetFileName($Source))'" {
            [FixtureDialog]::SetFilename($dialog, $Source, '')
        }
        $command = 1
    } else { $command = 2 }
    $null = Wait-Until 'close the shell fragment picker' { [FixtureDialog]::ClickButton($dialog, $command) }
    $null = Wait-Until 'the editor is ready after the import picker' {
        $control = Find-Element $import $button
        $control -and $control.Current.IsEnabled -and !(Find-OwnedDialog $title)
    }
}
function Save-DotfilesBundle([string]$Action, [string]$Path) {
    Invoke-Button (Wait-DotfileControl $Action $button)
    $dialog = Wait-Until 'owned dotfiles bundle save dialog' { Find-OwnedDialog 'Save new dotfiles bundle' }
    $null = Wait-Until "enter a draft filename for '$Action'" {
        [FixtureDialog]::SetCommonDialogFilename($dialog, "$Path.draft")
    }
    $null = Wait-Until "replace the draft filename for '$Action'" {
        [FixtureDialog]::SetCommonDialogFilename($dialog, $Path)
    }
    $null = Wait-Until 'save the local dotfiles bundle' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the created bundle is selected in the profile' {
        $field = Find-Element 'Dotfiles bundle' $edit
        $field -and [FixtureDialog]::SameFile($Path, $field.GetCurrentPattern($value).Current.Value)
    }
}
function Select-ToolPreset([string]$Name) {
    $presets = Wait-Until 'built-in tool preset selector' { Find-Element 'Built-in Linux tool bundle' $combo }
    $presets.SetFocus()
    $pattern = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $popup = $presets.GetCurrentPattern($pattern)
    $popup.Expand()
    $choice = Wait-Until "visible tool preset '$Name'" { Find-Element $Name $item -Visible }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}
function Tool-Check([string]$Name) {
    $type = [System.Windows.Automation.ControlType]::CheckBox
    $check = Wait-Until "checkbox for '$Name'" { Find-Element "Include $Name" $type }
    return $check.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
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
    $null = Wait-Until 'enter the full dotfiles manifest path' { [FixtureDialog]::SetFilename($bundleDialog, $bundle, '') }
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
    Invoke-Button (Wait-DotfileControl 'Create dotfiles bundle' $button)
    $shared = "export EDITOR=vi`nexport PAGER=less"
    $bash = "PS1='fleet> '`nalias ll='ls -l'"
    $zsh = "PROMPT='fleet%# '`nsetopt autocd"
    Set-DotfileFragment 'Shared environment fragment' $shared
    Set-DotfileFragment 'Bash startup fragment' $bash
    Set-DotfileFragment 'Zsh startup fragment' $zsh
    $created = Join-Path $State 'Authored dotfiles.json'
    Save-DotfilesBundle 'Save dotfiles bundle' $created
    $originalHash = File-Sha256 $created
    $initial = [IO.File]::ReadAllText($created) | ConvertFrom-Json
    if ($initial.version -ne 1 -or $initial.environment -cne $shared -or
        $initial.bash -cne $bash -or $initial.zsh -cne $zsh) {
        throw 'The authored dotfiles did not preserve all three multiline fragments'
    }
    Invoke-Button (Wait-DotfileControl 'Revise selected dotfiles bundle' $button)
    if ((Dotfile-Fragment 'Bash startup fragment') -cne $bash -or
        (Dotfile-Fragment 'Zsh startup fragment') -cne $zsh) {
        throw 'The revision editor did not load the complete multiline fragments'
    }
    $revisedBash = "PS1='revised> '`nalias ll='ls -lh'"
    Set-DotfileFragment 'Bash startup fragment' $revisedBash
    Import-ShellFragment 'Append to Zsh fragment'
    if ((Dotfile-Fragment 'Zsh startup fragment') -cne $zsh) {
        throw 'Cancelling the import changed the Zsh fragment'
    }
    $invalidFragment = Join-Path $State 'oversized shell fragment.sh'
    [IO.File]::WriteAllText($invalidFragment, ('x' * 16385))
    Import-ShellFragment 'Append to Bash fragment' $invalidFragment
    $null = Wait-Until 'the editor explains the fragment byte limit' {
        Find-Element 'Importing shell fragment: Dotfile fragments must be at most 16 KiB of UTF-8 without NUL or a BOM'
    }
    if ((Dotfile-Fragment 'Bash startup fragment') -cne $revisedBash) {
        throw 'An invalid import changed the draft'
    }
    $bashSource = Join-Path $State '.bashrc'
    $extraBash = "export VISUAL=nano`n# " + [char]0x96ea + "`n"
    [IO.File]::WriteAllText($bashSource, $extraBash.Replace("`n", "`r`n"), [Text.UTF8Encoding]::new($true))
    $importHash = File-Sha256 $bashSource
    Import-ShellFragment 'Append to Bash fragment' $bashSource
    $revisedBash += "`n$extraBash"
    $null = Wait-Until 'the Bash editor shows the appended UTF-8 text' {
        (Dotfile-Fragment 'Bash startup fragment') -ceq $revisedBash
    }
    $revision = Join-Path $State 'Revised dotfiles.json'
    Save-DotfilesBundle 'Save dotfiles revision' $revision
    $updated = [IO.File]::ReadAllText($revision) | ConvertFrom-Json
    $approved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.dotfiles.bundle
    if ($updated.environment -cne $shared -or $updated.bash -cne $revisedBash -or
        $updated.zsh -cne $zsh -or (File-Sha256 $created) -cne $originalHash -or
        (File-Sha256 $bashSource) -cne $importHash -or
        ![FixtureDialog]::SameFile($bundle, $approved)) {
        throw 'Revising a bundle changed its source, another fragment, or the saved approval'
    }
    Invoke-Button (Wait-DotfileControl 'Approve dotfiles sync' $button)
    $null = Wait-Until 'the revision has been approved' { Find-Element $approval }
    Click 'Save profile changes'
    $null = Wait-Until 'the saved profile uses the approved revision' {
        $path = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.dotfiles.bundle
        [FixtureDialog]::SameFile($revision, $path)
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
    $null = Wait-Until 'enter the full tool manifest path' { [FixtureDialog]::SetFilename($toolDialog, $toolBundle, '') }
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
    Select-NativeComboItem 'Tool bundle shell PATH' 'Manage Fish PATH'
    Click 'Review tool bundle'
    $null = Wait-Until 'the Fish PATH approval has the same tool destination' { Find-Element $review }
    Click 'Approve tool sync'
    $null = Wait-Until 'Fish PATH approved for the tool bundle' { Find-Element 'Approved tool bundle: portable' }
    Click 'Save profile changes'
    $null = Wait-Until 'Fish PATH approval persisted' {
        $tools = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.tool_bundle
        $tools.add_to_path.shell -ceq 'fish' -and $tools.bin_directory -ceq '.local/bin'
    }
    Click 'Disable tool sync'
    $null = Wait-Until 'tool sync disabled' { Find-Element 'Tool sync off' }
    Click 'Save profile changes'
    $null = Wait-Until 'tool approvals removed' {
        !(([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.tool_bundle)
    }

    Select-ToolPreset 'Linux admin tools'
    $null = Wait-Until 'admin preset selected without modifying the custom bundle' {
        (Find-Element 'Use Linux admin tools' $button) -and
            [FixtureDialog]::SameFile($toolBundle, (Find-Element 'Tool bundle manifest' $edit).GetCurrentPattern($value).Current.Value)
    }

    Select-ToolPreset 'Custom tools'
    $null = Wait-Until 'empty selection cannot start a download' {
        $use = Find-Element 'Use selected tools' $button
        $use -and !$use.Current.IsEnabled
    }
    foreach ($tool in @('rg','jq','mdd')) {
        $check = Tool-Check $tool
        if ($check.Current.ToggleState -ne 'Off') { throw "Fresh custom selection preselected $tool" }
        $check.Toggle()
        $null = Wait-Until "tool '$tool' selected" { (Tool-Check $tool).Current.ToggleState -eq 'On' }
    }
    $null = Wait-Until 'three-tool selection enables the download' {
        (Find-Element 'Use selected tools' $button).Current.IsEnabled
    }
    if (![FixtureDialog]::SameFile($toolBundle, (Find-Element 'Tool bundle manifest' $edit).GetCurrentPattern($value).Current.Value)) {
        throw 'Toggling the catalog selection prematurely changed the manifest'
    }

    if ($env:W11TERM_VERIFY_STARTER_DOWNLOAD -eq '1') {
        Click 'Use selected tools'
        Click 'Cancel tool download'
        $null = Wait-Until 'starter download cancelled' { Find-Element 'Tool download cancelled' }
        $toolField = (Find-Element 'Tool bundle manifest' $edit).GetCurrentPattern($value).Current.Value
        if (![FixtureDialog]::SameFile($toolBundle, $toolField)) { throw 'Cancelling changed the selected manifest' }

        Click 'Use selected tools'
        $null = Wait-Until 'selected builds ready' {
            Find-Element 'Selected tools are ready. Review the bundle to approve sync.'
        }
        $selectedManifest = (Find-Element 'Tool bundle manifest' $edit).GetCurrentPattern($value).Current.Value
        $starter = [IO.File]::ReadAllText($selectedManifest) | ConvertFrom-Json
        if (($starter.tools.name -join ',') -cne 'rg,jq,mdd' -or @($starter.tools.builds).Count -ne 6) {
            throw 'The custom manifest did not contain just the six selected tool builds'
        }
        foreach ($build in $starter.tools.builds) {
            $cached = Join-Path $env:W11TERM_DATA_DIR "tools/cache/v1/$($build.sha256)"
            if ((File-Sha256 $cached) -cne $build.sha256) {
                throw "Starter cache did not contain the pinned $($build.architecture) build"
            }
        }
        if (([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.tool_bundle) {
            throw 'Selecting the starter authorized deployment before review'
        }
        $cacheDirectory = Join-Path $env:W11TERM_DATA_DIR 'tools/cache/v1'
        if ([IO.Directory]::GetFiles($cacheDirectory).Count -ne 6) { throw 'The picker cached an unselected build' }
        Click 'Review tool bundle'
        $review = 'linux-essentials ' + [char]0x2192 + ' test@example.test:22 ' + [char]0x00b7 + ' /home/test/.local/bin'
        $null = Wait-Until 'starter destination reviewed' { Find-Element $review }
        Click 'Approve tool sync'
        Click 'Save profile changes'
        $null = Wait-Until 'starter approval saved' {
            $tools = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.tool_bundle
            $tools.name -ceq 'linux-essentials' -and $tools.add_to_path.shell -ceq 'bash' -and
                [FixtureDialog]::SameFile($selectedManifest, $tools.bundle)
        }

        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { throw 'w11term did not close after saving' }
        $app.Dispose()
        $app = Start-Process -FilePath $Gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
        $window = Wait-InputWindow
        Click 'Tab menu'
        Click 'Settings' $menu
        Click 'Edit profile fixture'
        Set-NativeExpansion 'SSH tools' $true
        foreach ($tool in @('rg','jq','mdd')) {
            if ((Tool-Check $tool).Current.ToggleState -ne 'On') { throw "Reopened profile lost $tool" }
        }
        if ((Tool-Check 'netwatch').Current.ToggleState -ne 'Off') { throw 'Reopened profile selected an omitted tool' }
    }

    Click 'Create bundle from local tools'
    Set-Field 'New bundle name' 'team-tools'
    Set-Field 'Linux command name' 'operator'
    $dot = ' ' + [char]0xb7 + ' '
    foreach ($build in @(@('operator-x64', 'x86-64'), @('operator-arm','ARM64'))) {
        Pick-LinuxExecutable 'Add Linux executable' $build[0]
        $null = Wait-Until "detected $($build[1]) executable" { Find-Element "operator${dot}$($build[1])" }
    }
    Click 'Save new tool bundle'
    $picker = Wait-Until 'owned bundle save dialog' { Find-OwnedDialog 'Save new SSH tool bundle' }
    $null = Wait-Until 'accept new bundle file' { [FixtureDialog]::ClickButton($picker, 1) }
    $created = Join-Path $State 'tools.json'
    $null = Wait-Until 'new manifest selected before review' {
        $field = Find-Element 'Tool bundle manifest' $edit
        $field -and [FixtureDialog]::SameFile($created, $field.GetCurrentPattern($value).Current.Value)
    }
    $createdBundle = [IO.File]::ReadAllText($created) | ConvertFrom-Json
    if ($createdBundle.name -cne 'team-tools' -or $createdBundle.tools.name -cne 'operator' -or
        ($createdBundle.tools.builds.architecture -join ',') -cne 'x86_64,aarch64') {
        throw 'Created manifest did not contain the named command and both architectures'
    }
    foreach ($build in $createdBundle.tools.builds) {
        $source = if ([IO.Path]::IsPathRooted($build.path)) { $build.path } else { Join-Path $State $build.path }
        if ((File-Sha256 $source) -cne $build.sha256) {
            throw 'The saved manifest pin did not match the selected executable'
        }
    }
    Click 'Review tool bundle'
    $review = 'team-tools ' + [char]0x2192 + " test@example.test:22${dot}/home/test/.local/bin"
    $null = Wait-Until 'new local bundle was reviewed' { Find-Element $review }
    Click 'Approve tool sync'
    Click 'Save profile changes'
    $null = Wait-Until 'local bundle approval saved to the profile' {
        $tools = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.tool_bundle
        $tools.name -ceq 'team-tools' -and [FixtureDialog]::SameFile($created, $tools.bundle)
    }
    $originalPin = File-Sha256 $created
    Click 'Revise selected tool bundle'
    $null = Wait-Until 'both previous builds loaded' {
        (Find-Element "operator${dot}x86-64") -and (Find-Element "operator${dot}ARM64")
    }
    Pick-LinuxExecutable 'Replace operator x86-64' 'operator-x64-next'
    Click 'Save bundle revision'
    $picker = Wait-Until 'owned save revision dialog' { Find-OwnedDialog 'Save new SSH tool bundle' }
    $null = Wait-Until 'save the separate bundle revision' { [FixtureDialog]::ClickButton($picker, 1) }
    $revision = Join-Path $State 'tools-revision.json'
    $null = Wait-Until 'revision selected and awaiting review' {
        $field = Find-Element 'Tool bundle manifest' $edit
        $approve = Find-Element 'Approve tool sync' $button
        $field -and $approve -and !$approve.Current.IsEnabled -and
            [FixtureDialog]::SameFile($revision, $field.GetCurrentPattern($value).Current.Value)
    }
    $tools = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.tool_bundle
    if (![FixtureDialog]::SameFile($created, $tools.bundle) -or (File-Sha256 $created) -cne $originalPin) {
        throw 'Saving a revision changed the previously approved manifest or profile'
    }
    $revisionBundle = [IO.File]::ReadAllText($revision) | ConvertFrom-Json
    if ($revisionBundle.tools.builds[0].sha256 -cne (File-Sha256 (Join-Path $State 'operator-x64-next')) -or
        $revisionBundle.tools.builds[1].sha256 -cne $createdBundle.tools.builds[1].sha256) {
        throw 'The revision did not replace only the chosen build'
    }
    Click 'Review tool bundle'
    $null = Wait-Until 'new revision reviewed' { Find-Element $review }
    Click 'Approve tool sync'
    Click 'Save profile changes'
    $null = Wait-Until 'revision approval saved' {
        $tools = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.fixture.tool_bundle
        [FixtureDialog]::SameFile($revision, $tools.bundle)
    }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
