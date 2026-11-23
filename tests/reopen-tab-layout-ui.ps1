[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--workspace','duplicate','--cols','144','--rows','32') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $original = @(Wait-Until 'focused right PowerShell in the named split' {
        $rows = @(Sessions)
        $a = $rows | Where-Object label -eq 'palette-ssh'
        $b = $rows | Where-Object label -eq 'Palette PowerShell'
        if ($rows.Count -eq 2 -and $a.visible -and $b.active -and $a.tab_title -ceq 'Operations' -and
            $a.tab_id -eq $b.tab_id -and $a.rows -eq $b.rows -and $b.cols -gt $a.cols -and
            [Math]::Abs($a.cols / ($a.cols + $b.cols) - 0.375) -lt 0.03) { $rows }
    })
    $sourceShell = $original | Where-Object label -eq 'Palette PowerShell'
    $sourceSsh = $original | Where-Object label -eq 'palette-ssh'
    Wait-Text $sourceSsh.id 'ready'
    Wait-Text $sourceShell.id 'PS '
    $initial = Identity $sourceShell.id 'OldShell' '$global:DupValue="original"; '
    Send-Remote $sourceSsh.id 'source-before/'

    $neighbor = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $neighbor
    Wait-Text $neighbor 'PS '
    $before = Identity $neighbor 'NeighborBefore' '$global:DupValue="neighbor"; '
    Press-Key 0x77 # F8 selects tab 1.
    Wait-Focus $sourceShell.id
    Press-Key 0x73 # F4 focuses the left SSH pane.
    Wait-Focus $sourceSsh.id
    Run-Palette 'Enable read-only pane input'
    $null = Wait-Until 'source SSH input locked' { ((Sessions) | Where-Object id -eq $sourceSsh.id).read_only }
    Press-Key 0x71 # F2 returns to the right PowerShell before closing.
    Wait-Focus $sourceShell.id
    Press-Key 0x75 # F6 zooms the focused leaf.
    Wait-Zoom $sourceShell.id $sourceSsh.id
    Press-Key 0x79 # F10 closes the selected tab layout.
    $null = Wait-Until 'only the neighboring tab remains' {
        $rows = @(Sessions)
        $rows.Count -eq 1 -and $rows[0].active -and $rows[0].id -eq $neighbor
    }

    Run-Palette 'Reopen last closed tab layout'
    $restored = @(Wait-Until 'fresh named SSH and PowerShell split with restored focus and input locks' {
        $rows = @(Sessions)
        $mixed = @($rows | Where-Object tab_title -ceq 'Operations')
        $ssh = $mixed | Where-Object label -eq 'palette-ssh'
        $shell = $mixed | Where-Object label -eq 'Palette PowerShell'
        if ($rows.Count -eq 3 -and $mixed.Count -eq 2 -and $ssh -and $shell -and
            $rows.id -contains $neighbor -and $ssh.id -notin $original.id -and $shell.id -notin $original.id -and
            $ssh.tab_id -eq $shell.tab_id -and $ssh.tab_id -ne $sourceSsh.tab_id -and
            $shell.active -and !$shell.read_only -and $ssh.read_only) { $mixed }
    })
    $newShell = $restored | Where-Object label -eq 'Palette PowerShell'
    $newSsh = $restored | Where-Object label -eq 'palette-ssh'
    Wait-Zoom $newShell.id $newSsh.id
    Press-Key 0x75
    Wait-Split $newShell.id $newSsh.id
    Wait-Text $newShell.id 'PS '
    Wait-Text $newSsh.id 'ready'
    $fresh = Identity $newShell.id 'NewShell' 'if ($null -eq $global:DupValue) { $global:DupValue="fresh" }; '
    if ($fresh[1].Value -eq $initial[1].Value -or $fresh[2].Value -cne 'fresh') { throw 'Restored PowerShell reused its former process or variables' }
    Press-Key 0x78 # F9 selects the unchanged second tab.
    Wait-Focus $neighbor
    $after = Identity $neighbor 'NeighborAfter' ''
    if ($after[1].Value -ne $before[1].Value -or $after[2].Value -cne 'neighbor') { throw 'Reopening the layout replaced the neighboring shell' }
    Press-Key 0x77 # F8 returns to the restored first tab.
    Wait-Focus $newShell.id
    Press-Key 0x73
    Wait-Focus $newSsh.id
    Type-Text 'locked-input/'
    Run-Palette 'Disable read-only pane input'
    $null = Wait-Until 'restored SSH input unlocked' { !((Sessions) | Where-Object id -eq $newSsh.id).read_only }
    Type-Text 'reopened-after-unlock/'
    Wait-Text $newSsh.id 'reopened-after-unlock/'
    Check-History $newSsh.id 'reopened-after-unlock/' 'source-before/'
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
