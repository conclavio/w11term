[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Set-HostQuery([string]$Query) {
    $field = Wait-Until 'host query' { Find-Element 'Host filter' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Query)
}
function Get-ExitToken([int]$Pane) {
    (Control @('events','--pane',"$Pane")).restart_token
}
function Wait-Stopped([int]$Pane) {
    $null = Wait-Until "pane $Pane to stop and offer reconnect" {
        (Find-Element "Pane $Pane${dot}Connection stopped${dot}reconnect available") -and
            (Find-Element "Reconnect SSH pane $Pane" $button) -and (Get-ExitToken $Pane)
    }
}

$argsForApp = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $argsForApp -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'the control pipe' {
        Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json")
    }
    Run-Palette "Open SSH group${dot}Stop attempts"
    $sessions = Wait-Until 'three SSH panes' {
        $live = @((Control @('list')).sessions | Where-Object label -ne 'Terminal lab')
        if ($live.Count -eq 3) { ,$live }
    }
    $east = ($sessions | Where-Object label -ceq 'Pending East').id
    $west = ($sessions | Where-Object label -ceq 'Pending West').id
    $live = ($sessions | Where-Object label -ceq 'Live shell').id
    $stop = 'Stop selected SSH attempts'
    $null = Wait-Until 'the password and host-key prompts are pending' {
        (Find-Element "Pane $east${dot}Authentication required") -and
            (Find-Element "Pane $west${dot}Host key verification required")
    }
    $ready = Control @('wait-for-text','--pane',"$live",'--timeout','15','ready')
    $generation = ((Control @('list')).sessions | Where-Object id -eq $live).connection_generation
    if (!$generation -or !$ready) { throw 'The live control shell did not connect' }

    Set-HostQuery 'East'
    $null = Wait-Until 'the West attempt is hidden' { !(Find-Element "Focus pane $west" $button) }
    Click $stop
    Wait-Stopped $east
    if (Get-ExitToken $west) { throw 'The hidden West attempt was stopped' }
    if (!(Find-Element 'Stop requested for 1 host.')) { throw 'The stop count did not match the shown selection' }
    Set-HostQuery ''
    $null = Wait-Until 'West still needs its host key verified' {
        Find-Element "Pane $west${dot}Host key verification required"
    }

    Click "Reconnect SSH pane $east"
    $null = Wait-Until 'the same East pane starts a fresh authentication attempt' {
        (Find-Element "Pane $east${dot}Authentication required") -and !(Get-ExitToken $east)
    }
    Run-Palette 'Stop pending SSH connections' -Enter
    Wait-Stopped $east
    Wait-Stopped $west
    if (!(Find-Element 'Stop requested for 2 hosts.')) { throw 'The palette did not stop both selected attempts' }

    $null = Control @('send-text','--pane',"$live",'--no-paste','live-witness')
    $null = Control @('wait-for-text','--pane',"$live",'--timeout','10','live-witness')
    if (((Control @('list')).sessions | Where-Object id -eq $live).connection_generation -ne $generation) {
        throw 'The stop action replaced the established shell'
    }
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
