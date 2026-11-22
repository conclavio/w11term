[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/directory-actions.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_SPLIT_CWD = Join-Path $State ('100% ; # [pilot] ' + [char]0x96ea)
$null = [IO.Directory]::CreateDirectory($env:W11TERM_SPLIT_CWD)
$env:W11TERM_CWD_PROOF = '.w11term-' + [guid]::NewGuid().ToString('N')
[IO.File]::WriteAllText((Join-Path $env:W11TERM_SPLIT_CWD $env:W11TERM_CWD_PROOF), 'expected folder')
$profiles = Join-Path $State 'profiles.json'
$catalog = [IO.File]::ReadAllText($profiles) | ConvertFrom-Json
$localProfile = $catalog.profiles.'Palette PowerShell'
$localProfile | Add-Member NoteProperty powershell_integration $true
$localProfile | Add-Member NoteProperty cwd $State
$catalog.profiles | Add-Member NoteProperty 'Windows Terminal prompt' @{
    backend = 'local'; executable = 'powershell.exe'; args = @('-NoProfile', '-NoLogo'); cwd = $State
    powershell_integration = $false
}
[IO.File]::WriteAllText($profiles, ($catalog | ConvertTo-Json -Depth 12))
$right = 'Split right in current directory'
$below = 'Split below in current directory'
$button = [System.Windows.Automation.ControlType]::Button
$menuItem = [System.Windows.Automation.ControlType]::MenuItem

function Send-Line([int]$Pane, [string]$Line) {
    $bytes = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Line + [char]13))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$bytes)
}

function Confirm-Directory([int]$Pane, [string]$Token) {
    # Keep the result short enough to fit a split. Only executed output contains the marker.
    # A hosted runner can reach its temp folder using its short or long name.
    # PS 5.1 mis-escapes brackets in the current folder when resolving even a
    # -LiteralPath relative to it. Look up the sentinel using the shell's
    # actual absolute folder, which may use the runner's short or long spelling.
    Send-Line $Pane ('if ($PWD.Provider.Name -eq "FileSystem" -and [IO.File]::Exists([IO.Path]::Combine($PWD.ProviderPath,$env:W11TERM_CWD_PROOF))) { "cwd-" + "' + $Token + '" + "-" + $PID } else { "wrong cwd: " + $PWD.ProviderPath }')
    $pattern = "cwd-$Token-([0-9]+)"
    try {
        $frame = Control @('wait-for-text','--pane',"$Pane",'--timeout','20','--regex',$pattern)
        return [int]([regex]::Match($frame.text, $pattern).Groups[1].Value)
    } catch {
        throw "$_ Expected directory $env:W11TERM_SPLIT_CWD. PowerShell pane $Pane showed: $((Control @('get-text','--pane',"$Pane")).text)"
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','144','--rows','36') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    Run-Palette 'Split below with Palette PowerShell'
    $parent = Wait-Until 'PowerShell sharing the SSH tab' {
        $sessions = @((Control @('list')).sessions)
        $local = $sessions | Where-Object label -eq 'Palette PowerShell'
        $ssh = $sessions | Where-Object id -eq $remote
        if ($local.active -and $ssh.visible -and $local.tab_id -eq $ssh.tab_id) { $local }
    }
    $null = Control @('wait-for-text','--pane',"$($parent.id)",'--timeout','45','PS ')
    $change = 'Set-Location -LiteralPath $env:W11TERM_SPLIT_CWD'
    Send-Line $parent.id $change
    $parentPid = Confirm-Directory $parent.id 'first'
    Check-DirectoryActions $true
    Send-Line $parent.id 'Set-Location -LiteralPath Env:'
    $null = Control @('wait-for-text','--pane',"$($parent.id)",'--timeout','15','PS Env:')
    Check-DirectoryActions $false
    Send-Line $parent.id $change
    if ((Confirm-Directory $parent.id 'restored') -ne $parentPid) { throw 'Parent PowerShell changed' }
    Run-Palette $right
    $child = Wait-DirectoryChild $parent 3 'right'
    $null = Control @('wait-for-text','--pane',"$($child.id)",'--timeout','45','PS ')
    $childPid = Confirm-Directory $child.id 'right'
    if ($childPid -eq $parentPid) { throw 'Right split reused the parent PowerShell process' }

    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' $button })
    $panes = Wait-Until 'Panes submenu' { Find-Element 'Panes' $menuItem }
    $panes.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    Invoke-Button (Wait-Until 'Split below from the new local shell' {
        $item = Find-Element $below $menuItem
        if ($item -and $item.Current.IsEnabled) { $item }
    })
    $lower = Wait-DirectoryChild $child 4 'below'
    $null = Control @('wait-for-text','--pane',"$($lower.id)",'--timeout','45','PS ')
    $lowerPid = Confirm-Directory $lower.id 'below'
    if ($lowerPid -in $parentPid,$childPid) { throw 'Below split reused a PowerShell process' }
    Run-Palette 'New tab in current directory'
    $newTab = Wait-DirectoryChild $lower 5 'tab'
    $null = Control @('wait-for-text','--pane',"$($newTab.id)",'--timeout','45','PS ')
    $tabPid = Confirm-Directory $newTab.id 'tab'
    if ($tabPid -in $parentPid,$childPid,$lowerPid) { throw 'New tab reused a PowerShell process' }
    if ((Confirm-Directory $lower.id 'source') -ne $lowerPid) { throw 'The new tab replaced its source shell' }
    if ((Confirm-Directory $parent.id 'still') -ne $parentPid) { throw 'Original PowerShell was replaced' }
    $saved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.'Palette PowerShell'
    if ($saved.cwd -cne $State) { throw 'A local split changed the saved profile working directory' }

    $custom = (Control @('open','--profile','Windows Terminal prompt','--activate')).pane
    Wait-Focus $custom
    $null = Control @('wait-for-text','--pane',"$custom",'--timeout','45','PS ')
    Send-Line $custom $change
    $customPid = Confirm-Directory $custom 'before'
    Check-DirectoryActions $false
    # Install the quoted OSC 9;9 PowerShell prompt in this test process only.
    $promptDefinition = @'
function global:prompt { [string][char]27 + ']9;9;"' + $PWD.Path + '"' + [char]7 + 'custom-' + $PWD.Provider.Name + '> ' }
'@
    Send-Line $custom $promptDefinition
    $null = Control @('wait-for-text','--pane',"$custom",'--timeout','15','custom-FileSystem>')
    Check-DirectoryActions $true
    Send-Line $custom 'Set-Location -LiteralPath Env:'
    $null = Control @('wait-for-text','--pane',"$custom",'--timeout','15','custom-Environment>')
    Check-DirectoryActions $false
    Send-Line $custom $change
    if ((Confirm-Directory $custom 'reported') -ne $customPid) { throw 'Custom-prompt shell changed' }
    $origin = (Control @('list')).sessions | Where-Object id -eq $custom
    Run-Palette $right
    $peer = Wait-DirectoryChild $origin 7 'right'
    $null = Control @('wait-for-text','--pane',"$($peer.id)",'--timeout','45','PS ')
    if ((Confirm-Directory $peer.id 'custom') -eq $customPid) { throw 'OSC 9;9 split reused the reporting process' }

    $null = Control @('send-text','--pane',"$remote",'--no-paste','directory-split-ssh-live')
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','directory-split-ssh-live')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
