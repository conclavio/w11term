param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$folderAction = 'Go to folder' + [char]0x2026
$parent = '/Pilot''s $Logs [2026]'
$child = 'year 1 ' + [char]0x96ea

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name' button" {
        $b = Find-Element $Name $button
        if ($b -and $b.Current.IsEnabled) { $b }
    })
}
function Enter-Path([string]$Path, [string]$Action = 'Open') {
    Click $folderAction
    $field = Wait-Until 'remote folder editor' { Find-Element 'Remote folder path' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Path)
    Click $Action
    $null = Wait-Until 'folder editor closed' { !(Find-Element 'Remote folder path' $edit) }
}
function Listing([string]$Directory, [string]$Entry) {
    $null = Wait-Until "listing for $Directory" { (Find-Element $Directory) -and (Find-Element $Entry $button) }
}
function History-Buttons([bool]$CanGoBack, [bool]$CanGoForward) {
    $null = Wait-Until "history controls Back=$CanGoBack, Forward=$CanGoForward" {
        $back = Find-Element 'Back' $button
        $forward = Find-Element 'Forward' $button
        $back -and $forward -and ($back.Current.IsEnabled -eq $CanGoBack) -and ($forward.Current.IsEnabled -eq $CanGoForward)
    }
}
function Wait-FolderError([string]$Path) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $text = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    $null = Wait-Until "remote error for folder $Path" {
        foreach ($element in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$text)) {
            if ($element.Current.Name.StartsWith("Could not open folder ${Path}:")) { return $true }
        }
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','src/trace.rs')
    # The shell has reported its cwd. Another actor moves it before the drawer opens.
    $null = Control @('sftp','--pane',"$pane",'rename','/shell directory','/moved')
    Run-Palette 'Open files here'
    Enter-Path $parent
    Listing $parent "$child/"
    History-Buttons $false $false
    Enter-Path $child
    Listing "$parent/$child" 'witness.txt'
    History-Buttons $true $false
    Click 'witness.txt'
    Enter-Path 'never-requested' 'Cancel'
    Enter-Path 'missing'
    Wait-FolderError 'missing'
    Listing "$parent/$child" 'witness.txt'
    if (!(Find-Element 'Copy selected path' $button).Current.IsEnabled) { throw 'Failed navigation cleared the selection' }
    Click 'Back'
    Listing $parent "$child/"
    History-Buttons $false $true

    # A previously visited destination can disappear. Restore it and retry the same history entry.
    $null = Control @('sftp','--pane',"$pane",'rename',"$parent/$child",'/held-child')
    Click 'Forward'
    Wait-FolderError "$parent/$child"
    Listing $parent "$child/"
    History-Buttons $false $true
    $null = Control @('sftp','--pane',"$pane",'rename','/held-child',"$parent/$child")
    Click 'Forward'
    Listing "$parent/$child" 'witness.txt'
    History-Buttons $true $false

    Click 'Back'
    Listing $parent "$child/"
    Enter-Path '/'
    Listing '/' 'moved/'
    History-Buttons $true $false
    Click 'Back'
    Listing $parent "$child/"
    History-Buttons $false $true
    Click 'Forward'
    Listing '/' 'moved/'
    Run-Palette 'SFTP files'
    $null = Wait-Until 'drawer closed' { !(Find-Element $folderAction $button) }
    foreach ($c in 'after-navigation'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','10','after-navigation')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
