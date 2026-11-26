[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$parent = '/shell directory'
$renameAction = 'Rename' + [char]0x2026
$newFile = 'Report ' + [char]0x96ea + '.bin'

function Button([string]$Name) {
    Find-Element $Name ([System.Windows.Automation.ControlType]::Button)
}
function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled $Name" {
        $b = Button $Name
        if ($b -and $b.Current.IsEnabled) { $b }
    })
}
function Name-Field {
    Find-Element 'New name' ([System.Windows.Automation.ControlType]::Edit)
}
function Open-Rename([string]$Expected) {
    Invoke-SftpEntryAction $renameAction
    $null = Wait-Until "rename dialog for $Expected" {
        $edit = Name-Field
        $edit -and $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq $Expected
    }
}
function Set-Name([string]$Name) {
    (Name-Field).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Name)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell neighbor' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    $seed = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes('$w11RenameProof=$PID' + [char]13))
    $null = Control @('send-text','--pane',"$local",'--no-paste','--base64',$seed)
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    Run-Palette 'Open files here'
    $null = Wait-Until 'remote file listing' { (Find-Element $parent) -and (Button 'before.bin') }
    $null = Wait-Until 'entry actions disabled with no selection' { $b = Button 'Selected entry actions'; $b -and !$b.Current.IsEnabled }
    Click 'before.bin'
    Open-Rename 'before.bin'
    Set-Name '../outside'
    $null = Wait-Until 'single component required' { $b = Button 'Rename'; $b -and !$b.Current.IsEnabled }
    Set-Name 'cancel'
    (Name-Field).SetFocus()
    Post-Key 0x102 ([uint32][char]'X')
    Click 'Cancel'
    $null = Wait-Until 'cancel kept the originating file' { !(Name-Field) -and (Button 'before.bin') }

    $options = Wait-Until 'File options becomes available after cancel' { Get-SftpOptionsExpander }
    if ($options.Current.ExpandCollapseState -ne 'Collapsed') {
        throw 'File options should start collapsed'
    }
    Enable-SftpFileOption 'Replace existing files'
    $null = Wait-Until 'collapsed options show overwrite status' { Find-Element 'Overwrite on' }
    Open-Rename 'before.bin'
    Set-Name 'raced.bin'
    $null = Control @('sftp','--pane',"$remote",'put',(Join-Path $State 'collision.bin'),"$parent/raced.bin")
    Click 'Rename'
    $null = Wait-Until 'server reports destination collision' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $text = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
        $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text) |
            Where-Object { $_.Current.Name.StartsWith('Rename before.bin to raced.bin:') } | Select-Object -First 1
    }
    Open-Rename 'before.bin'
    Set-Name $newFile
    Click 'Rename'
    $null = Wait-Until 'renamed file in refreshed listing' { (Find-Element "Renamed before.bin to $newFile") -and (Button $newFile) }
    Open-Rename $newFile
    Click 'Cancel'
    $null = Wait-Until 'returned to listing' { !(Name-Field) }
    Click 'original folder/'
    Open-Rename 'original folder'
    Set-Name 'build results'
    Click 'Rename'
    $null = Wait-Until 'renamed directory selected' { Find-Element 'Renamed original folder to build results' }
    Click 'Open folder'
    $null = Wait-Until 'contents survive directory rename' { (Find-Element "$parent/build results") -and (Button 'inside.bin') }
    Run-Palette 'SFTP files'
    $null = Wait-Until 'drawer closed' { !(Button 'Selected entry actions') }
    foreach ($c in 'rename-ssh-ready'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','rename-ssh-ready')
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $local
    $probe = 'if ($w11RenameProof -eq $PID) { ''rename-local-'' + (40+2) }'
    foreach ($c in $probe.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','10','rename-local-42')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
