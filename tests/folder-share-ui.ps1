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
$identityLabel = 'palette-ssh ' + [char]0xb7 + " $($savedProfile.user)@$($savedProfile.host):$($savedProfile.port) " + [char]0xb7 + ' Read-only'
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
    $null = Wait-Until 'verified profile, account, port and access mode' { Find-Element $identityLabel }
}
function Folder-Dialog {
    foreach ($hwnd in [FixtureDialog]::Windows($app.Id)) {
        if ([FixtureDialog]::GetWindow($hwnd, 4) -ne $window) { continue }
        $element = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
        if ($element.Current.ClassName -eq '#32770' -and $element.Current.Name -eq 'Choose a folder to share') { return $hwnd }
    }
}
function Select-FixtureFolder([IntPtr]$Dialog) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($Dialog)
    $byName = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $leaf)
    $selectable = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::IsSelectionItemPatternAvailableProperty, $true)
    $match = [System.Windows.Automation.AndCondition]::new($byName, $selectable)
    $null = Wait-Until "native picker to select the fixture child '$leaf'" {
        $item = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $match)
        if ($item) {
            $selection = $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
            if (!$selection.Current.IsSelected) { $selection.Select() }
            return $selection.Current.IsSelected
        }
    }
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
function Pick-Folder([int]$ButtonId) {
    Click $add
    $dialog = Wait-Until 'owned native folder picker' { Folder-Dialog }
    if ($ButtonId -eq 1) { Select-FixtureFolder $dialog }
    $null = Wait-Until "folder picker button $ButtonId" { [FixtureDialog]::ClickButton($dialog, $ButtonId) }
    $null = Wait-Until 'folder picker dismissed' { !(Folder-Dialog) }
}
function Wait-FailedMount {
    $null = Wait-Until 'failed SSHFS setup and available retry' {
        $retry = Find-Element 'Mount smoke-share' $button
        if (!$retry -or !$retry.Current.IsEnabled) { return }
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $text = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
        foreach ($row in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text)) {
            if ($row.Current.Name.StartsWith('Mount failed')) { return $true }
        }
    }
}
function Send-ToShell([string]$Text) {
    Click 'Back to terminal'
    Wait-Focus $remote
    foreach ($c in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10',$Text)
}
for ($launch = 1; $launch -le 2; $launch++) {
    $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile','palette-ssh') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State "share-ui-$launch.log")
    try {
        $window = Wait-InputWindow
        $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
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
            Click 'Save folder'
            $null = Wait-Until 'saved remote mount path' { Find-Element '~/windows/smoke-share' }
            Wait-FailedMount
            $grantsDir = Join-Path $env:W11TERM_DATA_DIR 'folder-shares'
            $grantFiles = @(Get-ChildItem -LiteralPath $grantsDir -Filter '*.json' -File)
            if ($grantFiles.Length -ne 1) { throw 'Expected exactly one isolated local consent file' }
            $consentFile = $grantFiles[0].FullName
            $consent = @(Get-Content -Raw -LiteralPath $consentFile | ConvertFrom-Json)
            if ($consent.Length -ne 1 -or !(Same-SelectedFolder $consent[0].path) -or $consent[0].name -ne 'smoke-share') {
                throw 'Persisted consent did not match the selected test directory and mount name'
            }
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
            Wait-SelectedPath
            Wait-FailedMount
            Click 'Forget smoke-share'
            $null = Wait-Until 'forgotten share removed' { (Find-Element 'Requested removal of smoke-share') -and !(Find-Element '~/windows/smoke-share') }
            Send-ToShell 'share-shell-after-restart'
            Run-Palette $menuName
            $null = Wait-Until 'palette opens verified panel' { Find-Element $identityLabel }
            Click $add
            $dialog = Wait-Until 'owned picker before disconnect' { Folder-Dialog }
            Select-FixtureFolder $dialog
            $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64','BA==')
            $stale = 'The verified SSH connection has changed or closed. Reopen Shared folders from a connected SSH pane.'
            $null = Wait-Until 'verified connection revoked with the picker open' { Find-Element $stale }
            $null = Wait-Until 'finish stale real folder picker' { [FixtureDialog]::ClickButton($dialog, 1) }
            $null = Wait-Until 'stale picker completion safely discarded' { !(Folder-Dialog) -and (Find-Element $stale) }
            if ((Find-Element 'Mount name') -or (Find-Element $add)) { throw 'A stale picker returned folder access' }
            if ((Test-Path -LiteralPath $consentFile) -and (Get-Content -Raw -LiteralPath $consentFile).Trim() -ne '[]') { throw 'A folder grant survived Forget' }
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
