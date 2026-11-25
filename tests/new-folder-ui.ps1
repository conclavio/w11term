# Uses the native file drawer, the fixture's actual SSH/SFTP connection and a local PowerShell.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$parent = '/shell directory/projects'
$folder = 'Logs ' + [char]0x96ea + ' (1)'

function Button([string]$Name) {
    Find-Element $Name ([System.Windows.Automation.ControlType]::Button)
}
function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name' button" {
        $button = Button $Name
        if ($button -and $button.Current.IsEnabled) { $button }
    })
}
function Folder-Field {
    Find-Element 'Folder name' ([System.Windows.Automation.ControlType]::Edit)
}
function Set-Name([string]$Name) {
    (Folder-Field).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Name)
}
function Start-Folder([string]$Name) {
    Click 'New folder'
    $null = Wait-Until 'folder name field' { Folder-Field }
    Set-Name $Name
}
function Wait-Failure([string]$Name) {
    $null = Wait-Until "server rejection for '$Name'" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $text = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Text)
        foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text)) {
            if ($item.Current.Name.StartsWith("Could not create folder ${Name}:")) { return $true }
        }
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-Until 'SSH window handle' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'neighboring PowerShell' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    Run-Palette 'Open files here'
    Click 'projects/'
    Click 'Open folder'
    $null = Wait-Until 'canonical SFTP parent listing' { (Find-Element $parent) -and (Button 'kept/') }

    Start-Folder '../outside'
    $null = Wait-Until 'invalid remote component to disable Create' { $b = Button 'Create'; $b -and !$b.Current.IsEnabled }
    Set-Name 'canceled'
    (Folder-Field).SetFocus()
    Post-Key 0x102 ([uint32][char]'X')
    Press-Key 0x25 # Left stays inside the name field.
    Click 'Cancel'
    $null = Wait-Until 'cancel to return to the SFTP directory' { !(Folder-Field) -and (Find-Element $parent) }

    Start-Folder 'kept'
    Click 'Create'
    Wait-Failure 'kept'
    Start-Folder 'raced.bin'
    if (Button 'raced.bin') { throw 'Collision must be absent from the original listing' }
    # Another actor creates a destination after listing, before the user confirms.
    $null = Control @('sftp','--pane',"$remote",'put',(Join-Path $State 'collision.bin'),"$parent/raced.bin")
    Click 'Create'
    Wait-Failure 'raced.bin'

    Start-Folder $folder
    Click 'Create'
    $null = Wait-Until 'created folder in a refreshed listing' {
        (Find-Element "Created folder $folder") -and (Button "$folder/") -and (Button 'raced.bin')
    }
    # Opening without clicking the row proves that the refreshed directory is selected.
    Click 'Open folder'
    $null = Wait-Until 'new folder opened at the exact canonical remote path' { Find-Element "$parent/$folder" }
    Run-Palette 'SFTP files'
    $null = Wait-Until 'closed file drawer' { !(Button 'New folder') }
    foreach ($c in 'folder-ssh-resumed'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','folder-ssh-resumed')
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $local
    foreach ($c in "'folder-local-' + (40+2)".ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','10','folder-local-42')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
