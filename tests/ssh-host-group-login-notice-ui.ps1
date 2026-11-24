[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_EXPORT_SMOKE_DIR = $State
$env:W11TERM_TEST_COPY_FILE = Join-Path $State 'login-report-clipboard.txt'
$profiles = Join-Path $State 'profiles.json'
$config = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles
$gates = [IO.File]::ReadAllText((Join-Path $State 'shell-gates.json')) | ConvertFrom-Json
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button

function Click-Button([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Check-LoginReportExport([string]$Expected) {
    Click-Button 'Copy login report'
    $null = Wait-Until 'copy includes the original host results and diagnostic' {
        (Read-ClipboardFixture) -ceq $Expected
    }
    $save = 'Save login report' + [char]0x2026
    $title = 'Save SSH login report'
    $destination = Join-Path $State 'verified-logins.txt'
    Click-Button $save
    $dialog = Wait-Until 'owned login report dialog' { Find-OwnedDialog $title }
    $null = Wait-Until 'cancel login report export' { [FixtureDialog]::ClickButton($dialog, 2) }
    $null = Wait-Until 'the canceled dialog has closed' { !(Find-OwnedDialog $title) }
    if ([IO.File]::Exists((Join-Path $State 'ssh-login-results-2.txt'))) { throw 'Cancel saved a login report' }

    Click-Button $save
    $dialog = Wait-Until 'reopened login report dialog' { Find-OwnedDialog $title }
    $null = Wait-Until 'enter the login report destination' {
        [FixtureDialog]::SetFilename($dialog, $destination, 'ssh-login-results-2.txt')
    }
    $null = Wait-Until 'accept the login report destination' { [FixtureDialog]::ClickButton($dialog, 1) }
    $utf8 = [Text.UTF8Encoding]::new($false, $true)
    $null = Wait-Until 'the saved UTF-8 report matches the copy' {
        [IO.File]::Exists($destination) -and [IO.File]::ReadAllText($destination, $utf8) -ceq $Expected
    }
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
    $exit = Wait-Until 'the failed login is restartable' {
        (Control @('events','--pane',"$east")).restart_token
    }
    Remove-Item -LiteralPath (Join-Path (Split-Path -LiteralPath $gates.East) 'reject-shell-request')
    $null = Control @('restart','--pane',"$east",'--exit-token',"$exit")
    $null = Control @('wait-for-text','--pane',"$east",'--timeout','10','ready')

    $viewResults = "View SSH login results for SSH group${dot}Notices, notice 2"
    Click-Button $viewResults
    $success = "West${dot}test@127.0.0.1:$($config.West.port)${dot}Pane $west${dot}Connected"
    $failure = "East${dot}test@127.0.0.1:$($config.East.port)${dot}Pane $east${dot}Failed"
    $reason = 'remote server rejected a PTY or shell request (1 outstanding)'
    $null = Wait-Until 'the report retains the original failure after East reconnects' {
        (Find-Element $success) -and (Find-Element $failure) -and
            (Find-Element $reason)
    }
    $report = @('SSH login results', "SSH group${dot}Notices", $message, '', $success, $failure, "  $reason", '') -join "`n"
    Check-LoginReportExport $report
    Click-Button "Open SSH login pane $east"
    Wait-Focus $east
    Click-Button 'Attention inbox, 0 unread'
    Click-Button $viewResults
    Click-Button 'Back to inbox'
    Click-Button "Read${dot}SSH group${dot}Notices${dot}$message"
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
