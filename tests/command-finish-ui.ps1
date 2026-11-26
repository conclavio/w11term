[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$packets = Get-Content -Raw -LiteralPath (Join-Path $State 'commands.json') | ConvertFrom-Json
$arm = 'Notify when command finishes'
$cancel = 'Cancel command notification'
$dot = [char]0xb7

function Send-PeerOutput([string]$Witness) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($packets.$Witness))
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64',$encoded)
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10',$Witness)
}
function Assert-CompletionCodes([object[]]$Expected) {
    $journal = (Control @('events','--pane',"$remote",'--since',"$since",'--timeout','0')).journal
    $events = @($journal.events | Where-Object kind -ceq 'command-finished')
    if ($journal.lost -or $events.Count -ne $Expected.Count) {
        throw "Unexpected command completion journal: $($journal | ConvertTo-Json -Depth 5 -Compress)"
    }
    for ($i = 0; $i -lt $Expected.Count; $i++) {
        $data = $events[$i].data
        if (@($data.PSObject.Properties.Name | Where-Object { $_ -cnotin @('exit_code','duration_ms') }).Count -gt 0 -or
            $data.PSObject.Properties.Name -notcontains 'exit_code' -or
            ($null -ne $data.exit_code -and $data.exit_code -isnot [int] -and $data.exit_code -isnot [long]) -or
            $data.exit_code -ne $Expected[$i]) {
            throw "Unexpected completion payload: $($data | ConvertTo-Json -Compress)"
        }
    }
}
function Read-ActiveShellPid([string]$Tag) {
    $command = "'$Tag=' + " + '$PID'
    foreach ($c in $command.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $pattern = "$Tag=([0-9]+)"
    $text = (Control @('wait-for-text','--pane',"$local",'--regex','--timeout','15',$pattern)).text
    return [int]([regex]::Match($text, $pattern).Groups[1].Value)
}
function Visit-Notification([string]$Status) {
    Invoke-Button (Wait-Until 'one unread command notification' { Find-Element 'Attention inbox, 1 unread' })
    $name = "Unread $dot palette-ssh $dot Pane $remote $dot Command finished $dot $Status"
    Invoke-Button (Wait-Until $name { Find-Element $name })
    Wait-Focus $remote
    $null = Wait-Until 'command notification to be acknowledged' { Find-Element 'Attention inbox, 0 unread' }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $manifest = Get-ControlManifestPath $app.Id
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath $manifest }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','command-started')
    $since = (Control @('events','--pane',"$remote",'--since','0','--timeout','0')).journal.next
    Run-Palette "New tab with Palette PowerShell $dot Local"
    $local = Wait-Until 'new PowerShell pane' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $local
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    $shellPid = Read-ActiveShellPid 'shell-before'

    Run-Palette ('Find running commands' + [char]0x2026)
    Wait-PickerEditor 'Search running commands' 'the background SSH command picker'
    $field = Find-Element 'Search running commands' ([System.Windows.Automation.ControlType]::Edit)
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('build')
    $preview = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'recent-command-output')
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $null = Wait-Until 'output preview action' { Find-Element 'Show recent command output' }
    Press-Key 0x78 # F9 opens the running output preview
    $null = Wait-Until 'preview is open' { Find-Element 'Hide recent command output' }
    try {
        $null = Wait-Until 'recent SSH command output appears from the background pane' {
            $text = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $preview)
            $text -and ($text.Current.Name -split '\r?\n') -ccontains 'command-started'
        }
    } catch {
        $visible = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) |
            ForEach-Object { "$($_.Current.AutomationId)=$($_.Current.Name)" } | Select-Object -First 45
        throw "$_ Controls: $($visible -join ', ')"
    }
    Send-PeerOutput 'preview-update'
    $null = Wait-Until 'new output appears in the same preview without changing panes' {
        $text = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $preview)
        if ($text) {
            $lines = $text.Current.Name -split '\r?\n'
            $lines -ccontains 'command-started' -and $lines[-1] -ceq 'preview-update'
        }
    }
    Wait-Focus $local
    Invoke-Button (Wait-Until 'hide the running output preview' { Find-Element 'Hide recent command output' })
    $null = Wait-Until 'closing the preview removes the output text' {
        !$root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $preview)
    }
    $armSelected = 'Notify when selected command finishes'
    $cancelSelected = 'Cancel selected command notification'
    Invoke-Button (Wait-Until 'arm the selected SSH command' { Find-Element $armSelected })
    Invoke-Button (Wait-Until 'cancel the selected SSH command notification' { Find-Element $cancelSelected })
    $null = Wait-Until 'can rearm the same SSH command' { Find-Element $armSelected }
    Press-Key 0x77 # F8 arms the highlighted command
    $null = Wait-Until 'the SSH notification is armed again' { Find-Element $cancelSelected }
    Press-Key 0x1b
    $null = Wait-Until 'close the notification picker' { !(Find-Element 'Search running commands') }
    Wait-Focus $local

    Send-PeerOutput 'first-complete'
    $null = Wait-Until 'completion badge while PowerShell is active' { Find-Element 'Attention inbox, 1 unread' }
    Wait-Focus $local
    # A second command finishes before anyone even opens the first notification.
    Send-PeerOutput 'not-rearmed'
    Assert-CompletionCodes @(17)
    if ((Read-ActiveShellPid 'shell-during') -ne $shellPid) { throw 'The notification interrupted PowerShell' }
    $first = ((Control @('list-commands','--pane',"$remote")).commands | Where-Object exit_code -eq 17).id
    Invoke-Button (Wait-Until 'one unread command notification' { Find-Element 'Attention inbox, 1 unread' })
    $view = "View command $first for palette-ssh, pane $remote"
    Invoke-Button (Wait-Until $view { Find-Element $view })
    $selected = "Selected, build $dot exit 17"
    $null = Wait-Until 'older failed command selected' { Find-Element $selected }
    $null = Wait-Until 'newer success shown beside the selected command' { Find-Element "build $dot exit 0" }
    $null = Wait-Until 'command notification acknowledged' { Find-Element 'Attention inbox, 0 unread' }
    Invoke-Button (Wait-Until 'return from command history' { Find-Element 'Return to pane' })
    Wait-Focus $remote

    Run-Palette $arm
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    $submenu = Wait-Until 'Panes menu' { Find-Element 'Panes' ([System.Windows.Automation.ControlType]::MenuItem) }
    $submenu.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    Invoke-Button (Wait-Until 'armed pane menu action' { Find-Element $cancel })
    Send-PeerOutput 'cancel-complete'
    Assert-CompletionCodes @(17)
    if (!(Find-Element 'Attention inbox, 0 unread')) { throw 'Canceled command produced an alert' }

    Run-Palette $arm
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $local
    Send-PeerOutput 'next-complete'
    $null = Wait-Until 'rearmed notification badge' { Find-Element 'Attention inbox, 1 unread' }
    Assert-CompletionCodes @(17, $null)
    Wait-Focus $local
    if ((Read-ActiveShellPid 'shell-after') -ne $shellPid) { throw 'The second alert interrupted PowerShell' }
    Visit-Notification 'exit status unavailable'

    Run-Palette 'Notify for long commands: Off'
    Send-PeerOutput 'short-automatic-command'
    Assert-CompletionCodes @(17, $null)
    Send-PeerOutput 'long-command-started'
    $running = [Diagnostics.Stopwatch]::StartNew()
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $local
    # Wait for the measured threshold after the peer's start marker was committed.
    $remaining = 10000 - $running.ElapsedMilliseconds
    if ($remaining -gt 0) { Start-Sleep -Milliseconds $remaining }
    Send-PeerOutput 'long-command-finished'
    Assert-CompletionCodes @(17, $null, 23)
    $completion = (Control @('events','--pane',"$remote",'--since',"$since",'--timeout','0')).journal.events |
        Where-Object kind -CEQ 'command-finished' | Select-Object -Last 1
    if ($completion.data.duration_ms -lt 10000) { throw 'The automatic alert must report the elapsed time' }
    Wait-Focus $local
    Visit-Notification 'exit code 23'
    Run-Palette 'Notify for long commands: On'

    foreach ($c in 'ssh-resumed'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','ssh-resumed')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
