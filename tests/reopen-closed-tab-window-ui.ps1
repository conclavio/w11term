[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$keymap.shortcuts.find_closed_tab = 'F3'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, ($keymap | ConvertTo-Json))
$value = [System.Windows.Automation.ValuePattern]::Pattern
$action = 'Reopen in new window'

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'),
    '--workspace', 'duplicate') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $origin = $window
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $initial = @(Wait-Until 'initial Operations split' {
        $panes = @(Sessions)
        if ($panes.Count -eq 2 -and @($panes | Where-Object tab_title -eq 'Operations').Count -eq 2) { $panes }
    })
    $ssh = $initial | Where-Object label -eq 'palette-ssh'
    $shell = $initial | Where-Object label -eq 'Palette PowerShell'
    Wait-Text $ssh.id 'ready'
    Wait-Text $shell.id 'PS '
    Wait-Focus $shell.id
    $oldShell = Identity $shell.id 'BeforeClose' '$global:DupValue="original"; '
    Send-Remote $ssh.id 'original-window/'

    $neighbor = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $neighbor
    Wait-Text $neighbor 'PS '
    $neighborState = Identity $neighbor 'BeforeReopen' '$global:DupValue="neighbor"; '
    Press-Key 0x77 # F8 selects Operations.
    Wait-Focus $shell.id
    Press-Key 0x79 # F10 closes Operations and records its layout.
    Wait-Focus $neighbor
    $null = Wait-Until 'only the neighboring pane remains in the original window' {
        $panes = @(Sessions)
        $panes.Count -eq 1 -and $panes[0].id -eq $neighbor -and $panes[0].window_id -eq $ssh.window_id
    }

    Press-Key 0x72 # F3 opens Recently closed tabs.
    $field = Wait-PickerEditor 'Search closed tabs' 'the remapped history key'
    $field.GetCurrentPattern($value).SetValue('missing entry')
    $null = Wait-Until 'no window action without a matching layout' {
        (Find-Element 'No matching closed tabs') -and !(Find-Element $action $button)
    }
    $field.GetCurrentPattern($value).SetValue('operations test@127.0.0.1')
    Invoke-Button (Wait-Until 'the selected layout can reopen in a window' {
        $item = Find-Element $action $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
    $window = Wait-Until 'a second foreground HWND in the same w11term process' {
        $candidate = [FixtureMessages]::GetForegroundWindow()
        if ($candidate -ne [IntPtr]::Zero -and $candidate -ne $origin) {
            $element = [System.Windows.Automation.AutomationElement]::FromHandle($candidate)
            if ($element -and $element.Current.ProcessId -eq $app.Id -and
                $element.Current.Name -ceq ('w11term ' + [char]0x00b7 + ' Operations')) { $candidate }
        }
    }
    $restored = @(Wait-Until 'two fresh panes in the new window and the surviving source pane' {
        $panes = @(Sessions)
        $copy = @($panes | Where-Object tab_title -eq 'Operations')
        $source = $panes | Where-Object id -eq $neighbor
        if ($panes.Count -eq 3 -and $copy.Count -eq 2 -and
            $copy[0].window_id -ne $ssh.window_id -and $copy[0].window_id -eq $copy[1].window_id -and
            $copy[0].tab_id -eq $copy[1].tab_id -and !($copy | Where-Object { $_.id -in $initial.id }) -and
            $source.visible -and $source.window_id -eq $ssh.window_id) { $copy }
    })
    $newSsh = $restored | Where-Object label -eq 'palette-ssh'
    $newShell = $restored | Where-Object label -eq 'Palette PowerShell'
    Wait-Text $newSsh.id 'ready'
    Wait-Text $newShell.id 'PS '
    Wait-Focus $newShell.id
    try {
        $fresh = Identity $newShell.id 'AfterReopen' 'if (!$global:DupValue) { $global:DupValue="fresh" }; '
    } catch {
        $view = Control @('get-text','--pane',"$($newShell.id)")
        throw "$_ Foreground HWND $([FixtureMessages]::GetForegroundWindow()), target $window. PowerShell screen $($view.text)"
    }
    if ($fresh[1].Value -eq $oldShell[1].Value -or $fresh[2].Value -cne 'fresh') {
        throw 'History reused a closed PowerShell process'
    }
    Send-Remote $newSsh.id 'history-window/'
    Check-History $newSsh.id 'history-window/' 'original-window/'

    if (![FixtureMessages]::PostMessage($window,0x10,[UIntPtr]::Zero,[IntPtr]::Zero)) {
        throw 'Could not close the new history window'
    }
    $null = Wait-Until 'the source pane outlives its reopened neighbor' {
        $panes = @(Sessions)
        $panes.Count -eq 1 -and $panes[0].id -eq $neighbor
    }
    $window = $origin
    Wait-Focus $neighbor
    $retained = Identity $neighbor 'SourceAfterReopen' ''
    if ($retained[1].Value -ne $neighborState[1].Value -or $retained[2].Value -cne 'neighbor') {
        throw 'Opening the history window changed the source process'
    }
    Press-Key 0x72
    $null = Wait-PickerEditor 'Search closed tabs' 'the history after reopening'
    $null = Wait-Until 'the reopened layout is consumed exactly once' {
        Find-Element 'No recently closed tab layouts'
    }
} finally {
    Close-FixtureApp $app
}
