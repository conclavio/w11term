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
function Wait-Browse {
    $null = Wait-Until 'key picker releases the draft' {
        $control = Find-Element $browseLabel $button
        $control -and $control.Current.IsEnabled
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
    (Key-Field).SetValue('')
    $null = Wait-Until 'empty profile key path' { (Key-Field).Current.Value -ceq '' }
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
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
