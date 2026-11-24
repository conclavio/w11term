[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$config = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
$dot = ' ' + [char]0xb7 + ' '
$ellipsis = [char]0x2026
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Get-Session([int]$Pane) {
    (Control @('list')).sessions | Where-Object id -eq $Pane
}
function Get-ExitToken([int]$Pane) {
    (Control @('events','--pane',"$Pane")).restart_token
}
function Wait-Reconnect([int]$Pane, [int]$Generation) {
    $null = Wait-Until "fresh SSH transport for pane $Pane" {
        (Get-Session $Pane).connection_generation -gt $Generation
    }
}
function Open-Review {
    Click "Disconnect selected$ellipsis"
    $null = Wait-Until 'review lists only the two visible checked hosts' {
        (Find-Element $eastName) -and (Find-Element $westName) -and !(Find-Element $witnessName) -and
            (Find-Element 'Disconnect 2 SSH shells' $button)
    }
}

$argsForApp = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $argsForApp -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'the control pipe' {
        Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json")
    }
    Run-Palette "Open SSH group${dot}Disconnect review"
    $sessions = Wait-Until 'all three SSH panes connected' {
        $live = @((Control @('list')).sessions | Where-Object connection_generation -gt 0 | Where-Object label -ne 'Terminal lab')
        if ($live.Count -eq 3) { ,$live }
    }
    $eastSession = $sessions | Where-Object label -ceq 'Fleet East'
    $westSession = $sessions | Where-Object label -ceq 'Fleet West'
    $witnessSession = $sessions | Where-Object label -ceq 'Witness'
    $east = $eastSession.id
    $west = $westSession.id
    $witness = $witnessSession.id
    $eastName = "Fleet East${dot}test@127.0.0.1:$($config.'Fleet East'.port)${dot}Pane $east"
    $westName = "Fleet West${dot}test@127.0.0.1:$($config.'Fleet West'.port)${dot}Pane $west"
    $witnessName = "Witness${dot}test@127.0.0.1:$($config.Witness.port)${dot}Pane $witness"
    $null = Wait-CheckBox $witnessName On
    $field = Wait-Until 'host search' { Find-Element 'Host filter' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Fleet')
    $null = Wait-Until 'the checked witness is hidden' { !(Find-Element "Focus pane $witness" $button) }

    Open-Review
    Click 'Cancel'
    $null = Wait-CheckBox $eastName On
    if ((Get-ExitToken $east) -or (Get-ExitToken $west)) { throw 'Cancel disconnected an SSH shell' }

    Open-Review
    $null = Control @('send-text','--pane',"$east",'--no-paste','--base64','BA==')
    $token = Wait-Until 'the reviewed shell exits' { Get-ExitToken $east }
    $null = Control @('restart','--pane',"$east",'--exit-token',"$token")
    Wait-Reconnect $east $eastSession.connection_generation
    $eastReplacement = (Get-Session $east).connection_generation
    Click 'Disconnect 2 SSH shells'
    $null = Wait-Until 'the replaced connection requires a new review' {
        Find-Element 'The selected SSH shells changed. Review the current selection.'
    }
    if ((Get-ExitToken $east) -or (Get-ExitToken $west)) { throw 'The stale review disconnected a shell' }

    Open-Review
    Click 'Disconnect 2 SSH shells'
    $null = Wait-Until 'the two shells end and can reconnect' {
        (Find-Element 'Disconnect requested for 2 hosts.') -and
            (Get-ExitToken $east) -and (Get-ExitToken $west)
    }
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On
    $path = Join-Path $State 'disconnected.txt'
    $null = Control @('save-text','--pane',"$east",$path)
    if ((Get-Content -LiteralPath $path -Raw) -notmatch 'east history 00') {
        throw 'Disconnect lost the original SSH scrollback'
    }

    $null = Control @('send-text','--pane',"$witness",'--no-paste','witness-still-live')
    $null = Wait-Until 'the hidden witness still echoes on its original connection' {
        ((Get-Session $witness).connection_generation -eq $witnessSession.connection_generation) -and
            ((Control @('get-text','--pane',"$witness")).text -match 'witness-still-live')
    }
    Click 'Reconnect selected'
    Wait-Reconnect $east $eastReplacement
    Wait-Reconnect $west $westSession.connection_generation
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
