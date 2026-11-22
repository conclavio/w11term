[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_SPLIT_CWD = Join-Path $State ('100% # [pilot] ' + [char]0x96ea)
$null = [IO.Directory]::CreateDirectory($env:W11TERM_SPLIT_CWD)
$env:W11TERM_CWD_PROOF = '.w11term-' + [guid]::NewGuid().ToString('N')
[IO.File]::WriteAllText((Join-Path $env:W11TERM_SPLIT_CWD $env:W11TERM_CWD_PROOF), 'expected folder')
$profiles = Join-Path $State 'profiles.json'
$catalog = [IO.File]::ReadAllText($profiles) | ConvertFrom-Json
$localProfile = $catalog.profiles.'Palette PowerShell'
$localProfile | Add-Member NoteProperty powershell_integration $true
$localProfile | Add-Member NoteProperty cwd $State
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
    # Only the intended literal folder contains this unique relative filename.
    Send-Line $Pane ('if ($PWD.Provider.Name -eq "FileSystem" -and (Test-Path -LiteralPath $env:W11TERM_CWD_PROOF -PathType Leaf)) { "cwd-" + "' + $Token + '" + "-" + $PID } else { "wrong cwd: " + $PWD.ProviderPath }')
    $pattern = "cwd-$Token-([0-9]+)"
    try {
        $frame = Control @('wait-for-text','--pane',"$Pane",'--timeout','20','--regex',$pattern)
        return [int]([regex]::Match($frame.text, $pattern).Groups[1].Value)
    } catch {
        throw "$_ Expected directory $env:W11TERM_SPLIT_CWD. PowerShell pane $Pane showed: $((Control @('get-text','--pane',"$Pane")).text)"
    }
}

function Check-Actions([bool]$Enabled) {
    $field = Open-Palette 'current directory'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('current directory')
    $null = Wait-Until "both local directory split actions enabled=$Enabled" {
        $a = Find-Element $right $button
        $b = Find-Element $below $button
        $a -and $b -and $a.Current.IsEnabled -eq $Enabled -and $b.Current.IsEnabled -eq $Enabled
    }
    Press-Key 27
    $null = Wait-Until 'command palette closed' { !(Find-Element 'Search commands') }
}

function Wait-Child($Origin, [int]$Count, [string]$Direction) {
    Wait-Until "new active PowerShell pane split $Direction" {
        $sessions = @((Control @('list')).sessions)
        $parent = $sessions | Where-Object id -eq $Origin.id
        $child = $sessions | Where-Object active
        if ($sessions.Count -ne $Count -or !$parent.visible -or !$child.visible -or
            $child.id -eq $parent.id -or $child.tab_id -ne $Origin.tab_id -or
            $child.label -ne $Origin.label) { return }
        if ($Direction -eq 'right') {
            if ($parent.cols -lt 0.8 * $Origin.cols -and $child.cols -lt 0.8 * $Origin.cols -and
                [Math]::Abs($child.rows - $parent.rows) -le 1) { $child }
        } elseif ($parent.rows -lt 0.8 * $Origin.rows -and $child.rows -lt 0.8 * $Origin.rows -and
            [Math]::Abs($child.cols - $parent.cols) -le 1) { $child }
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
    Check-Actions $true
    Send-Line $parent.id 'Set-Location -LiteralPath Env:'
    $null = Control @('wait-for-text','--pane',"$($parent.id)",'--timeout','15','PS Env:')
    Check-Actions $false
    Send-Line $parent.id $change
    if ((Confirm-Directory $parent.id 'restored') -ne $parentPid) { throw 'Parent PowerShell changed' }
    Run-Palette $right
    $child = Wait-Child $parent 3 'right'
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
    $lower = Wait-Child $child 4 'below'
    $null = Control @('wait-for-text','--pane',"$($lower.id)",'--timeout','45','PS ')
    $lowerPid = Confirm-Directory $lower.id 'below'
    if ($lowerPid -in $parentPid,$childPid) { throw 'Below split reused a PowerShell process' }
    if ((Confirm-Directory $parent.id 'still') -ne $parentPid) { throw 'Original PowerShell was replaced' }
    $saved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.'Palette PowerShell'
    if ($saved.cwd -cne $State) { throw 'A local split changed the saved profile working directory' }
    $null = Control @('send-text','--pane',"$remote",'--no-paste','directory-split-ssh-live')
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','directory-split-ssh-live')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
