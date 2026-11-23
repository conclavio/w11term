[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
$action = 'Duplicate tab layout'

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--workspace','duplicate','--cols','144','--rows','32') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $all = Wait-Until 'named mixed split with the saved asymmetric divider' {
        $rows = @(Sessions)
        $a = $rows | Where-Object label -eq 'palette-ssh'
        $b = $rows | Where-Object label -eq 'Palette PowerShell'
        if ($rows.Count -eq 2 -and $a.visible -and $b.active -and $a.tab_id -eq $b.tab_id -and
            $a.tab_title -ceq 'Operations' -and $b.tab_title -ceq 'Operations' -and
            $a.rows -eq $b.rows -and $b.cols -gt $a.cols -and
            [Math]::Abs($a.cols / ($a.cols + $b.cols) - 0.375) -lt 0.03) { $rows }
    }
    $sourceSsh = $all | Where-Object label -eq 'palette-ssh'
    $sourceShell = $all | Where-Object label -eq 'Palette PowerShell'
    Wait-Text $sourceSsh.id 'ready'
    Wait-Text $sourceShell.id 'PS '
    $original = Identity $sourceShell.id 'Original' '$global:DupValue="source"; '
    if ($original[2].Value -cne 'source') { throw 'Original PowerShell variable was not set' }
    $sourcePid = $original[1].Value
    Send-Remote $sourceSsh.id 'source-before/'
    Wait-Focus $sourceShell.id
    Press-Key 0x75 # F6 zooms the right pane before cloning the group
    Wait-Zoom $sourceShell.id $sourceSsh.id

    Invoke-Button (Wait-Until 'caret' { Find-Element 'Tab menu' $button })
    Invoke-Button (Wait-Until 'enabled duplicate action in the native menu' {
        $entry = Find-Element $action ([System.Windows.Automation.ControlType]::MenuItem)
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
    $copied = Wait-Until 'four distinct panes in two terminal tabs in the original window' {
        $rows = @(Sessions)
        $new = @($rows | Where-Object tab_id -ne $sourceSsh.tab_id)
        if ($rows.Count -eq 4 -and @($rows.id | Sort-Object -Unique).Count -eq 4 -and
            $new.Count -eq 2 -and @($new.tab_id | Sort-Object -Unique).Count -eq 1 -and
            @($rows | Where-Object { $_.window_id -ne $sourceSsh.window_id -or $_.tab_title -cne 'Operations' }).Count -eq 0) { $new }
    }
    $copySsh = $copied | Where-Object label -eq 'palette-ssh'
    $copyShell = $copied | Where-Object label -eq 'Palette PowerShell'
    if (!$copySsh -or !$copyShell) { throw 'Duplicated the wrong profiles' }
    Wait-Zoom $copyShell.id $copySsh.id
    Wait-Text $copySsh.id 'ready'
    Wait-Text $copyShell.id 'PS '
    $fresh = Identity $copyShell.id 'Fresh' 'if ($null -eq $global:DupValue) { $global:DupValue="copy" }; '
    if ($fresh[1].Value -eq $sourcePid -or $fresh[2].Value -cne 'copy') { throw 'Duplicate reused PowerShell runtime state' }
    $foreground = [FixtureMessages]::GetForegroundWindow()
    Send-Remote $copySsh.id 'duplicate-background/'
    Wait-Focus $copyShell.id
    if ($foreground -ne $window -and [FixtureMessages]::GetForegroundWindow() -eq $window) {
        throw 'Background SSH control input activated the fixture window'
    }

    Press-Key 0x75 # F6 restores only the duplicate split
    Wait-Split $copyShell.id $copySsh.id
    Press-Key 0x73 # F4 focuses the left neighbor, proving the two leaves kept their order
    Wait-Focus $copySsh.id
    Type-Text 'duplicate-focused/'
    Wait-Text $copySsh.id 'duplicate-focused/'
    Check-History $copySsh.id 'duplicate-background/' 'source-before/'
    Check-History $sourceSsh.id 'source-before/' 'duplicate-focused/'
    Check-History $copyShell.id "Fresh=$($fresh[1].Value)/copy" "Original=$sourcePid/source"
    Check-History $sourceShell.id "Original=$sourcePid/source" "Fresh=$($fresh[1].Value)/copy"
    Press-Key 0x77 # First tab remains the original, still zoomed
    Wait-Zoom $sourceShell.id $sourceSsh.id
    Press-Key 0x78 # Second tab is the copy, still split, with its left pane focused
    Wait-Focus $copySsh.id
    Wait-Split $copyShell.id $copySsh.id
    Press-Key 0x79 # F10 closes the selected duplicate group
    $null = Wait-Until 'only original pane IDs remain registered' {
        $rows = @(Sessions)
        $rows.Count -eq 2 -and $rows.id -contains $sourceShell.id -and $rows.id -contains $sourceSsh.id
    }
    Wait-Zoom $sourceShell.id $sourceSsh.id
    $retained = Identity $sourceShell.id 'Retained' ''
    if ($retained[1].Value -ne $sourcePid -or $retained[2].Value -cne 'source') { throw 'Closing the copy affected the original PowerShell' }
    Press-Key 0x75
    Wait-Split $sourceShell.id $sourceSsh.id
    Press-Key 0x73
    Wait-Focus $sourceSsh.id
    Type-Text 'source-after/'
    Wait-Text $sourceSsh.id 'source-after/'
    Check-History $sourceSsh.id 'source-before/' 'duplicate-focused/'

    # Stage a recipe only. The peer has no exec capability; the negative case
    # must not open a third connection or send any command to the active shell.
    Run-Palette ('Workspace startup' + [char]0x2026)
    $edit = Wait-Until 'startup command editor' { Find-Element 'Remote startup command' ([System.Windows.Automation.ControlType]::Edit) }
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('echo must-not-run')
    $null = Wait-Until 'startup command preview' { Find-Element ('On reopen ' + [char]0xb7 + ' Run echo must-not-run') }
    Invoke-Button (Wait-Until 'apply to pane' { Find-Element 'Apply to pane' $button })
    $tab = Wait-Until 'original terminal tab' { Find-Element 'Operations' ([System.Windows.Automation.ControlType]::TabItem) }
    $tab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Wait-Focus $sourceSsh.id
    $edit = Open-Palette $action
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($action)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty,$button)
    $null = Wait-Until 'duplicate disabled with the staged SSH startup reason' {
        foreach ($entry in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$condition)) {
            if ($entry.Current.Name -like 'Duplicate tab layout:*workspace startup*' -and !$entry.Current.IsEnabled) { return $true }
        }
    }
    Press-Key 27
    Wait-Focus $sourceSsh.id
    if (@(Sessions).Count -ne 2) { throw 'The disabled action created a session' }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
