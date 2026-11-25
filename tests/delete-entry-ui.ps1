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
    $control = Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $button
        if ($control -and $control.Current.IsEnabled) { $control }
    }
    try { Invoke-Button $control }
    catch { throw "Invoking active button '$Name' during '$phase' failed. $_" }
}
function Choose-Delete([string]$Name, [string]$Kind) {
    $row = if ($Kind -eq 'folder') { "$Name/" } else { $Name }
    Click $row
    Invoke-SftpEntryAction $action
    $null = Wait-Until "confirmed identity for $Kind '$Name'" {
        (Find-Element "Delete remote ${Kind}?") -and (Find-Element "$($account.user)@$($account.host):$($account.port)") -and
            (Find-Element "$parent/$Name") -and
            (Find-Element 'Delete' $button) -and (Find-Element 'Cancel' $button)
    }
}
function Expect-Deleted([string]$Name, [string]$Kind) {
    $script:phase = "delete $Kind '$Name'"
    Choose-Delete $Name $Kind
    Click 'Delete'
    $row = if ($Kind -eq 'folder') { "$Name/" } else { $Name }
    $null = Wait-Until "successful deletion and refreshed listing for '$Name'" {
        (Find-Element "Deleted $Name") -and !(Find-Element $row $button)
    }
}
function Test-DeleteConfirmationVisible {
    (Find-Element 'Delete remote file?') -or (Find-Element 'Delete' $button)
}
function Try-OldConfirmation($Old, [string]$Reason) {
    if (!$Old) { throw "Missing saved Delete button before $Reason" }
    $null = Wait-Until "Delete confirmation dismissed after $Reason" { !(Test-DeleteConfirmationVisible) }
    # Exercise the saved handle once. WinUI can return E_FAIL from Invoke on a
    # retired element even when its cached IsEnabled property is still true.
    try { if ($Old.Current.IsEnabled) { Invoke-Button $Old } }
    catch {
        $cause = $_.Exception.GetBaseException()
        $retired = $cause -is [System.Windows.Automation.ElementNotAvailableException] -or
                   $cause -is [System.Windows.Automation.ElementNotEnabledException] -or
                   ($cause -is [Runtime.InteropServices.COMException] -and $cause.HResult -eq -2147467259) # E_FAIL
        if (!$retired -or $app.HasExited -or (Test-DeleteConfirmationVisible)) {
            throw "Invoking the saved Delete button after $Reason failed. $_"
        }
    }
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
    $phase = 'open files here'
    $window = Wait-InputWindow
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Wait-Drawer
    $null = Wait-Until 'entry actions disabled with no selection' {
        $b = Find-Element 'Selected entry actions' $button
        $b -and !$b.Current.IsEnabled
    }
    $phase = 'cancel deletion of keep.txt'
    Choose-Delete 'keep.txt' 'file'
    $cancelled = Find-Element 'Delete' $button
    Click 'Cancel'
    $null = Wait-Until 'Cancel preserved selection' { !(Find-Element 'Delete remote file?') -and (Find-Element 'keep.txt' $button) }
    Click 'purge.bin'
    Try-OldConfirmation $cancelled 'Cancel and selection of purge.bin'

    # An open confirmation belongs to its SSH pane even when the control pipe changes tabs.
    $phase = 'switch panes with the keep.txt confirmation open'
    Choose-Delete 'keep.txt' 'file'
    $previous = Find-Element 'Delete' $button
    $local = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $local
    Try-OldConfirmation $previous 'switching to the PowerShell pane'
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    Press-Key 0x77 # F8 selects the first tab
    Wait-Focus $remote
    Wait-Drawer
    $null = Wait-Until 'old confirmation dismissed after pane switch' { !(Find-Element 'Delete remote file?') }
    $phase = 'reconnect with the keep.txt confirmation open'
    Choose-Delete 'keep.txt' 'file'
    $previous = Find-Element 'Delete' $button
    $generation = ((Control @('list')).sessions | Where-Object id -eq $remote).connection_generation
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64','BA==')
    $null = Wait-Until 'same pane on fresh SSH transport' {
        ((Control @('list')).sessions | Where-Object id -eq $remote).connection_generation -gt $generation
    }
    Try-OldConfirmation $previous 'SSH reconnect'
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Wait-Drawer

    # The same path acquired a different type while its confirmation was open.
    $phase = 'confirm deletion after race.txt changes type'
    Choose-Delete 'race.txt' 'file'
    $null = Control @('sftp','--pane',"$remote",'rename',"$parent/race.txt","$parent/original-race.txt")
    $null = Control @('sftp','--pane',"$remote",'mkdir',"$parent/race.txt")
    Click 'Delete'
    Expect-Refusal 'race.txt' 'type changed'
    $phase = 'confirm deletion of nonempty folder full'
    Choose-Delete 'full' 'folder'
    Click 'Delete'
    Expect-Refusal 'full' ''
    Expect-Deleted 'purge.bin' 'file'
    Expect-Deleted 'directory-link' 'link'
    Expect-Deleted 'dangling-link' 'link'
    Expect-Deleted 'empty' 'folder'
    $phase = 'inspect preserved folder and resume both shells'
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
} catch {
    throw "SFTP delete fixture failed during '$phase'. $_`n$($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
