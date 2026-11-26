[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$appearance = Get-Content -LiteralPath $env:W11TERM_SETTINGS_PATH -Raw | ConvertFrom-Json
$appearance.shortcuts | Add-Member NoteProperty 'find_command_history' 'F6'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, ($appearance | ConvertTo-Json -Depth 8))
$profiles = Join-Path $State 'profiles.json'
$startFolder = Join-Path $State ('history start % ' + [char]0x96ea)
$env:W11TERM_HISTORY_LATER = Join-Path $State 'history later'
$env:W11TERM_HISTORY_INSPECT_GATE = Join-Path $State 'finish-inspected-command'
$env:W11TERM_TEST_EXPLORER_TRACE = Join-Path $State 'history-explorer.json'
$null = [IO.Directory]::CreateDirectory($startFolder)
$null = [IO.Directory]::CreateDirectory($env:W11TERM_HISTORY_LATER)
[IO.File]::WriteAllText((Join-Path $startFolder 'history-folder-proof'), 'original Windows directory')
$catalog = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
$catalog.profiles.'Palette PowerShell' | Add-Member NoteProperty cwd $startFolder
$labels = @('Palette PowerShell')
if ($pwsh = Get-Command pwsh.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1) {
    $catalog.profiles | Add-Member NoteProperty 'Fixture PowerShell 7' @{
        backend='local'; executable=$pwsh.Source; args=@('-NoLogo','-NoProfile'); cwd=$startFolder
    }
    $labels += 'Fixture PowerShell 7'
}
[IO.File]::WriteAllText($profiles, ($catalog | ConvertTo-Json -Depth 12))
$explorer = $null
$priorExplorerWindows = @()

function Click([string]$Name) {
    try {
        $item = Wait-Until $Name { Find-Element $Name ([System.Windows.Automation.ControlType]::Button) }
        Invoke-Button $item
    } catch {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $visible = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) |
            ForEach-Object { "$($_.Current.LocalizedControlType)=$($_.Current.Name)" } | Select-Object -First 65
        throw "$_  Looking for '$Name'. Current controls: $($visible -join ', ')"
    }
}

function Set-Integration([string]$Label) {
    Run-Palette 'Settings'
    Click "Edit local profile $Label"
    $name = 'Enable PowerShell integration'
    $box = Wait-Until 'interactive PowerShell profile checkbox' {
        Find-Element $name ([System.Windows.Automation.ControlType]::CheckBox)
    }
    $toggle = $box.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
    if ($toggle.Current.ToggleState -ne 'Off') { throw "Expected integration to default off for $Label" }
    $toggle.Toggle()
    $null = Wait-Until 'checked integration setting' {
        (Find-Element $name).GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -eq 'On'
    }
    Click 'Save profile'
    $null = Wait-Until 'saved PowerShell integration' {
        (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles.$Label.powershell_integration -eq $true
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    foreach ($label in $labels) {
        Set-Integration $label
        $pane = (Control @('open','--profile',$label,'--activate')).pane
        Wait-Focus $pane
        $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','PS ')
        Run-Palette 'Notify when command finishes'
        $capture = Join-Path $State "shell-$pane.vt"
        $null = Control @('record','--pane',"$pane",$capture)

        # Type an ordinary line. The shell supplies every semantic marker.
        $line = 'Write-Output (''shell-output-'' + (40 + 2)); cd -LiteralPath $env:W11TERM_HISTORY_LATER; cmd.exe /d /c exit 17'
        $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($line + [char]13))
        $null = Control @('send-text','--pane',"$pane",'--no-paste','--base64',$encoded)
        $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','shell-output-42')
        try {
            $null = Wait-Until "$label to report exit 17" {
                $journal = (Control @('events','--pane',"$pane")).journal
                $events = @($journal.events | Where-Object kind -ceq 'command-finished')
                if ($events.Count -gt 1) { throw "Duplicate completion in $label" }
                if ($events.Count -eq 1 -and $events[0].data.exit_code -ne 17) {
                    throw "Incorrect exit code in ${label}: $($events[0].data.exit_code)"
                }
                $events.Count -eq 1
            }
        } catch {
            $null = Control @('record','--pane',"$pane",'--stop')
            $bytes = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($capture))
            $markers = [regex]::Matches($bytes, ([char]27 + '\]133;[A-D](?:;-?\d+)?')) |
                ForEach-Object { $_.Value.Substring(2) }
            $screen = (Control @('get-text','--pane',"$pane")).text.TrimEnd()
            throw "$_  Observed OSC markers: [$($markers -join ', ')]. Pane: $screen"
        }
        $null = Control @('record','--pane',"$pane",'--stop')
        Run-Palette 'Command history'
        $selected = "Selected, $line " + [char]0xb7 + ' exit 17'
        $null = Wait-Until "$label command text and exit status in history" { Find-Element $selected }
        $null = Wait-Until "$label command output preview" {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            $text = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
            $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text) |
                Where-Object { $_.Current.Name.Trim() -ceq 'shell-output-42' } | Select-Object -First 1
        }
        $saved = ((Control @('list-commands','--pane',"$pane")).commands)[0]
        $cwd = ([Uri]$saved.reported_directory).LocalPath
        if (!$cwd -or ![IO.File]::Exists((Join-Path $cwd 'history-folder-proof'))) {
            throw "History lost the original command folder for $label"
        }
        $dot = ' ' + [char]0xb7 + ' '
        $null = Wait-Until "$label recorded working directory is shown" { Find-Element "Working directory${dot}$cwd" }
        Click 'Return to pane'
        Wait-Focus $pane
        Write-Output "Verified $label"
    }
    $picker = 'Search commands across panes'
    Press-Key 0x75
    Wait-PickerEditor $picker 'the remapped history shortcut'
    Press-Key 0x1b
    $null = Wait-Until 'Escape closes window history' { !(Find-Element $picker) }
    Wait-Focus $pane

    $find = 'Find command history' + [char]0x2026 + '    F6'
    Run-Palette $find
    Wait-PickerEditor $picker 'the window history picker'
    Press-Key 0x0d
    $null = Wait-Until 'Enter opens the newest capture across panes' { Find-Element $selected }
    Click 'Return to pane'
    Wait-Focus $pane

    Run-Palette $find
    Wait-PickerEditor $picker 'the window output search input'
    $field = Find-Element $picker ([System.Windows.Automation.ControlType]::Edit)
    $value = $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    $value.SetValue('REMOTE RESULT [42]')
    $null = Wait-Until 'default history search excludes output' {
        Find-Element 'No matching commands in this window'
    }
    Press-Key 0x78 # F9 includes retained command output
    Press-Key 0x76 # F7 filters to Failed
    Press-Key 0x77 # F8 uses duration order
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $buttons = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button)
    $null = Wait-Until 'case-insensitive literal matches show a remote output excerpt' {
        $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $buttons) |
            Where-Object { $_.Current.Name.Contains('remote result [42]') -and $_.Current.Name.Contains('palette-ssh') } |
            Select-Object -First 1
    }
    $field.SetFocus()
    Press-Key 0x0d
    $null = Wait-Until 'history preview opens after the first 8 KiB at the output match' {
        $text = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Text)
        $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text) |
            Where-Object { $_.Current.Name.StartsWith([string][char]0x2026) -and $_.Current.Name.Contains('remote result [42]') } |
            Select-Object -First 1
    }
    Click 'Back to search results'
    Wait-PickerEditor $picker 'the reopened window history'
    $null = Wait-Until 'return restores the output query, filter, and order' {
        $field = Find-Element $picker ([System.Windows.Automation.ControlType]::Edit)
        $scope = Find-Element 'Include retained command output' ([System.Windows.Automation.ControlType]::CheckBox)
        $status = Find-Element 'Window command status' ([System.Windows.Automation.ControlType]::ComboBox)
        $order = Find-Element 'Window command order' ([System.Windows.Automation.ControlType]::ComboBox)
        $selection = [System.Windows.Automation.SelectionPattern]::Pattern
        if (!$field -or !$scope -or !$status -or !$order) { return }
        $statuses = @($status.GetCurrentPattern($selection).Current.GetSelection())
        $orders = @($order.GetCurrentPattern($selection).Current.GetSelection())
        $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq 'REMOTE RESULT [42]' -and
            $scope.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -eq 'On' -and
            $statuses.Count -eq 1 -and $statuses[0].Current.Name -ceq 'Failed' -and
            $orders.Count -eq 1 -and $orders[0].Current.Name -ceq 'Slowest first'
    }
    Press-Key 0x1b
    $null = Wait-Until 'Escape closes the restored history search' { !(Find-Element $picker) }
    Wait-Focus $pane

    Run-Palette $find
    Wait-PickerEditor $picker 'the window history input'
    $catalog = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    $ssh = $catalog.profiles.'palette-ssh'
    $dot = ' ' + [char]0xb7 + ' '
    $remoteCommand = 'inspect-history ' + ('s' * 132) + ' window-remote-needle'
    $caption = "palette-ssh${dot}$($ssh.user)@$($ssh.host):$($ssh.port)${dot}Pane $remote${dot}exit 8${dot}$($remoteCommand.Substring(0, 120))"
    $field = Find-Element $picker ([System.Windows.Automation.ControlType]::Edit)
    $value = $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    $value.SetValue('PALETTE-SSH')
    $null = Wait-Until 'the source profile matches its SSH command' { Find-Element $caption }
    $value.SetValue('WINDOW-REMOTE-NEEDLE')
    $null = Wait-Until 'the full SSH command matches past the shortened caption' { Find-Element $caption }
    $value.SetValue('REMOTE HISTORY START')
    $null = Wait-Until 'the captured command directory matches' { Find-Element $caption }
    Press-Key 0x0d
    $selectedRemote = "Selected, $($remoteCommand.Substring(0, 120))${dot}exit 8"
    $null = Wait-Until 'the exact SSH history capture opens' { Find-Element $selectedRemote }
    $null = Wait-Until 'SSH history shows the directory at the command start' { Find-Element "Working directory${dot}/remote history start" }
    Click 'Open command directory'
    Wait-Focus $remote
    $null = Wait-Until 'the original command directory opens in SFTP' {
        (Find-Element '/remote history start') -and (Find-Element 'history-proof' ([System.Windows.Automation.ControlType]::Button))
    }

    $null = Control @('send-text','--pane',"$remote",'--no-paste','neighbor-ok')
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','neighbor-ok')
    $diagnostic = 'while (![IO.File]::Exists($env:W11TERM_HISTORY_INSPECT_GATE)) { Start-Sleep -Milliseconds 25 }; "local-command-finished"'
    $wire = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($diagnostic + [char]13))
    $null = Control @('send-text','--pane',"$pane",'--no-paste','--base64',$wire)
    $running = Wait-Until 'background PowerShell exposes its running command' {
        $m = Control @('inspect','--pane',"$pane")
        if ($m.running_command.command -ceq $diagnostic -and $m.running_command.id -gt 0) {
            $m.running_command
        }
    }
    Wait-Focus $remote
    Run-Palette ('Find running commands' + [char]0x2026)
    Wait-PickerEditor 'Search running commands' 'the running command picker'
    $field = Find-Element 'Search running commands' ([System.Windows.Automation.ControlType]::Edit)
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('HISTORY_INSPECT_GATE')
    $null = Wait-Until 'the background PowerShell command appears with elapsed time' {
        $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $buttons) |
            Where-Object { $_.Current.Name.StartsWith($diagnostic) -and $_.Current.Name -match '    [0-9]+ s$' } |
            Select-Object -First 1
    }
    Click 'Notify when selected command finishes'
    $null = Wait-Until 'the background PowerShell notification is armed' {
        Find-Element 'Cancel selected command notification'
    }
    $beforeFinish = (Control @('events','--pane',"$pane",'--since','0','--timeout','0')).journal.next
    [IO.File]::WriteAllText($env:W11TERM_HISTORY_INSPECT_GATE, 'finish')
    $null = Wait-Until 'PowerShell completes the inspected capture' {
        $m = Control @('inspect','--pane',"$pane")
        $h = Control @('list-commands','--pane',"$pane")
        !$m.running_command -and $h.commands[0].id -eq $running.id -and $h.commands[0].command -ceq $diagnostic
    }
    $null = Wait-Until 'completion removes the result from the open running command picker' {
        Find-Element 'No matching running commands'
    }
    $journal = (Control @('events','--pane',"$pane",'--since',"$beforeFinish",'--timeout','0')).journal
    $finished = @($journal.events | Where-Object kind -CEQ 'command-finished')
    if ($finished.Count -ne 1 -or $journal.lost) { throw 'Expected exactly one PowerShell completion notification' }
    Press-Key 0x1b
    $null = Wait-Until 'Escape closes the running command picker' { !(Find-Element 'Search running commands') }
    Wait-Focus $remote
    Click 'Tab menu'
    Invoke-Button (Wait-Until 'Attention inbox menu item' {
        Find-Element 'Attention inbox' ([System.Windows.Automation.ControlType]::MenuItem)
    })
    $view = "View command $($running.id) for $label, pane $pane"
    Click $view
    $null = Wait-Until 'the PowerShell notification opens the same capture' {
        Find-Element ("Selected, $diagnostic" + ' ' + [char]0xb7 + ' exit 0')
    }
    Click 'Return to pane'
    Wait-Focus $pane
    $loop = 'while ($true) { Start-Sleep -Milliseconds 40 }'
    $wire = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($loop + [char]13))
    $null = Control @('send-text','--pane',"$pane",'--no-paste','--base64',$wire)
    $target = Wait-Until 'the local PowerShell loop starts' {
        $r = (Control @('inspect','--pane',"$pane")).running_command
        if ($r.command -ceq $loop -and $r.start_token -gt 0) { $r }
    }
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    $ack = Control @('interrupt-command','--pane',"$pane",'--start-token',"$($target.start_token)")
    if (!$ack.sent -or $ack.pane -ne $pane) { throw 'Ctrl+C was not confirmed for the background PowerShell pane' }
    $null = Wait-Until 'PowerShell returns from the interrupted command' {
        !(Control @('inspect','--pane',"$pane")).running_command
    }
    Wait-Focus $remote
    $proof = "Write-Output ('interrupt-' + 'recovered')"
    $wire = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($proof + [char]13))
    $null = Control @('send-text','--pane',"$pane",'--no-paste','--base64',$wire)
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','10','interrupt-recovered')

    # Explorer can take the foreground on the hosted desktop. Do the native
    # handoff after the keyboard and picker focus assertions.
    Picker-Query $label
    Press-Key 13
    Wait-Focus $pane
    # The pane's history was opened above and Return to pane leaves that tab
    # available. Select it directly for the Explorer handoff.
    $historyTab = Wait-Until 'existing PowerShell history tab' {
        Find-Element ('History ' + [char]0xb7 + ' ' + $label) ([System.Windows.Automation.ControlType]::TabItem)
    }
    $historyTab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $inputType = [System.Windows.Automation.ControlType]::Edit
    $query = Wait-Until 'local command history search' { Find-Element 'Search command history' $inputType }
    $query.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($line)
    $null = Wait-Until 'the original PowerShell command is selected' { Find-Element $selected }
    $explorer = New-Object -ComObject Shell.Application
    $priorExplorerWindows = @($explorer.Windows() | ForEach-Object { $_.HWND })
    Click 'Open command directory'
    $null = Wait-Until 'the recorded local command directory opens in Explorer' {
        try { $trace = [IO.File]::ReadAllText($env:W11TERM_TEST_EXPLORER_TRACE) | ConvertFrom-Json } catch { return }
        $trace.action -ceq 'explore' -and [IO.File]::Exists((Join-Path $trace.path 'history-folder-proof'))
    }
} finally {
    if ($explorer) {
        try { $views = @($explorer.Windows()) } catch { $views = @() }
        foreach ($view in $views) {
            try {
                $folder = $view.Document.Folder.Self.Path
                if ($view.HWND -notin $priorExplorerWindows -and $folder -and
                    [IO.Path]::GetFullPath($folder) -ieq [IO.Path]::GetFullPath($startFolder)) {
                    $view.Quit()
                }
            } catch { }
        }
    }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
