[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$gates = [IO.File]::ReadAllText((Join-Path $State 'shell-gates.json')) | ConvertFrom-Json
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button

function Click-Button([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

$launch = @('--profiles', ('"' + $profiles + '"'), '--demo', '--cols', '100', '--rows', '35')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $manifest = Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json"
    $null = Wait-Until 'the fixture control pipe' { Test-Path -LiteralPath $manifest }
    Run-Palette "Open SSH group${dot}Notices" -Enter
    Click-Button 'Notify when logins finish'
    Click-Button 'Cancel login notification'
    Click-Button 'Notify when logins finish'
    $null = Wait-Until 'the renewed watch is armed' { Find-Element 'Cancel login notification' $button }

    Click-Button 'Attention inbox, 0 unread'
    $null = Wait-Until 'the inbox is active' { Find-Element 'No bells or notifications' }

    $west = ((Control @('list')).sessions | Where-Object label -ceq 'West').id
    $east = ((Control @('list')).sessions | Where-Object label -ceq 'East').id
    Remove-Item -LiteralPath $gates.West
    $null = Control @('wait-for-text','--pane',"$west",'--timeout','10','ready')
    if ((Control @('events','--pane',"$east")).ssh_connection.phase -ne 'connecting') {
        throw 'East settled before the remaining shell was released'
    }
    if (!(Find-Element 'No bells or notifications')) { throw 'The partial result opened or filled the inbox' }

    Remove-Item -LiteralPath $gates.East
    $message = "SSH logins finished${dot}1 connected${dot}1 failed${dot}0 stopped${dot}0 closed"
    $null = Wait-Until 'one new unread notification while the group is in the background' {
        Find-Element 'Attention inbox, 1 unread' $button
    }
    Click-Button "Unread${dot}SSH group${dot}Notices${dot}$message"
    $null = Wait-Until 'visiting the notice selects the original roster and marks it read' {
        (Find-Element 'Host filter') -and (Find-Element 'Attention inbox, 0 unread' $button)
    }
    if (Find-Element 'Cancel login notification' $button) { throw 'The one-shot watch is still armed' }
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
