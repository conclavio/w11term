[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$account = (Get-Content $profiles -Raw | ConvertFrom-Json).profiles.'palette-ssh'
$parent = '/shell directory'
$action = 'Delete' + [char]0x2026
$button = [System.Windows.Automation.ControlType]::Button

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $button
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}
function Choose-Delete([string]$Name, [string]$Kind) {
    $row = if ($Kind -eq 'folder') { "$Name/" } else { $Name }
    Click $row
    Click $action
    $null = Wait-Until "confirmed identity for $Kind '$Name'" {
        (Find-Element "Delete remote ${Kind}?") -and (Find-Element "$($account.user)@$($account.host):$($account.port)") -and
            (Find-Element "$parent/$Name") -and
            (Find-Element 'Delete' $button) -and (Find-Element 'Cancel' $button)
    }
}
function Expect-Deleted([string]$Name, [string]$Kind) {
    Choose-Delete $Name $Kind
    Click 'Delete'
    $row = if ($Kind -eq 'folder') { "$Name/" } else { $Name }
    $null = Wait-Until "successful deletion and refreshed listing for '$Name'" {
        (Find-Element "Deleted $Name") -and !(Find-Element $row $button)
    }
}
function Try-OldConfirmation($Old) {
    # The UIA element may have been removed before its callback is delivered.
    try { if ($Old.Current.IsEnabled) { Invoke-Button $Old } }
    catch [System.Windows.Automation.ElementNotAvailableException] {}
    catch [System.Windows.Automation.ElementNotEnabledException] {}
}
function Wait-Drawer {
    $null = Wait-Until 'canonical remote directory and preserved file' {
        (Find-Element $parent) -and (Find-Element 'keep.txt' $button)
    }
}
function Expect-Refusal([string]$Name, [string]$Reason) {
    $null = Wait-Until "server refusal for $Name" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $byText = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,[System.Windows.Automation.ControlType]::Text)
        $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$byText) |
            Where-Object { $_.Current.Name.StartsWith("Could not delete ${Name}:") -and $_.Current.Name -like "*$Reason*" } |
            Select-Object -First 1
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Wait-Drawer
    $null = Wait-Until 'Delete disabled with no selection' {
        $b = Find-Element $action $button
        $b -and !$b.Current.IsEnabled
    }
    Choose-Delete 'keep.txt' 'file'
    $cancelled = Find-Element 'Delete' $button
    Click 'Cancel'
    $null = Wait-Until 'Cancel preserved selection' { !(Find-Element 'Delete remote file?') -and (Find-Element 'keep.txt' $button) }
    Click 'purge.bin'
    Try-OldConfirmation $cancelled

    # An open confirmation belongs to its SSH pane even when the control pipe changes tabs.
    Choose-Delete 'keep.txt' 'file'
    $previous = Find-Element 'Delete' $button
    $local = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $local
    Try-OldConfirmation $previous
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    Press-Key 0x77 # F8 selects the first tab
    Wait-Focus $remote
    Wait-Drawer
    $null = Wait-Until 'old confirmation dismissed after pane switch' { !(Find-Element 'Delete remote file?') }
    Choose-Delete 'keep.txt' 'file'
    $previous = Find-Element 'Delete' $button
    $generation = ((Control @('list')).sessions | Where-Object id -eq $remote).connection_generation
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64','BA==')
    $null = Wait-Until 'same pane on fresh SSH transport' {
        ((Control @('list')).sessions | Where-Object id -eq $remote).connection_generation -gt $generation
    }
    Try-OldConfirmation $previous
    $null = Wait-Until 'old confirmation dismissed after reconnect' { !(Find-Element 'Delete remote file?') }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Wait-Drawer

    # The same path acquired a different type while its confirmation was open.
    Choose-Delete 'race.txt' 'file'
    $null = Control @('sftp','--pane',"$remote",'rename',"$parent/race.txt","$parent/original-race.txt")
    $null = Control @('sftp','--pane',"$remote",'mkdir',"$parent/race.txt")
    Click 'Delete'
    Expect-Refusal 'race.txt' 'type changed'
    Choose-Delete 'full' 'folder'
    Click 'Delete'
    Expect-Refusal 'full' ''
    Expect-Deleted 'purge.bin' 'file'
    Expect-Deleted 'directory-link' 'link'
    Expect-Deleted 'dangling-link' 'link'
    Expect-Deleted 'empty' 'folder'
    Click 'full/'
    Click 'Open folder'
    $null = Wait-Until 'nonempty directory and link target still have the original child' {
        (Find-Element "$parent/full") -and (Find-Element 'child' $button)
    }
    Run-Palette 'SFTP files'
    $null = Wait-Until 'drawer closed' { !(Find-Element $action $button) }
    foreach ($c in 'delete-ssh-ready'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','delete-ssh-ready')
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $local
    foreach ($c in "'delete-local-' + (40+2)".ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','delete-local-42')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
