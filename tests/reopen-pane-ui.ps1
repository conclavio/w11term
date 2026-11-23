[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

function Tab-MenuItem([string]$Name, [switch]$Panes) {
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' $button })
    $menu = [System.Windows.Automation.ControlType]::MenuItem
    if ($Panes) {
        $item = Wait-Until 'Panes submenu' { Find-Element 'Panes' $menu }
        $item.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    }
    Wait-Until "menu item '$Name'" { Find-Element $Name $menu }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--workspace','duplicate','--cols','144','--rows','32') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $original = @(Wait-Until 'separate SSH and PowerShell in their original split' {
        $rows = @(Sessions)
        $ssh = $rows | Where-Object label -eq 'palette-ssh'
        $local = $rows | Where-Object label -eq 'Palette PowerShell'
        if ($rows.Count -eq 2 -and $ssh.visible -and $local.active -and $ssh.tab_title -ceq 'Operations' -and
            $ssh.tab_id -eq $local.tab_id -and $ssh.rows -eq $local.rows -and
            [Math]::Abs($ssh.cols / ($ssh.cols + $local.cols) - 0.375) -lt 0.03) { $rows }
    })
    $sourceShell = $original | Where-Object label -eq 'Palette PowerShell'
    $sourceSsh = $original | Where-Object label -eq 'palette-ssh'
    Wait-Text $sourceSsh.id 'ready'
    Wait-Text $sourceShell.id 'PS '
    $initial = Identity $sourceShell.id 'OriginalPane' '$global:DupValue="original"; '
    Send-Remote $sourceSsh.id 'before-local-close/'

    Invoke-Button (Tab-MenuItem ('Rename focused pane' + [char]0x2026) -Panes)
    $field = Wait-Until 'pane-name field' { Find-Element 'Pane name' }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Local build')
    Invoke-Button (Wait-Until 'enabled Save button' {
        $save = Find-Element 'Save' $button
        if ($save -and $save.Current.IsEnabled) { $save }
    })
    $null = Wait-Until 'original pane named' { ((Sessions) | Where-Object id -eq $sourceShell.id).pane_name -ceq 'Local build' }
    $null = Wait-Until 'named PowerShell pane ready for input' {
        $document = Find-Element ('Terminal ' + [char]0xb7 + ' Local build') ([System.Windows.Automation.ControlType]::Document)
        $document -and $document.Current.AutomationId -ceq "w11term-pane-$($sourceShell.id)"
    }
    Invoke-Button (Tab-MenuItem 'Enable read-only pane input' -Panes)
    $null = Wait-Until 'original pane locked' { ((Sessions) | Where-Object id -eq $sourceShell.id).read_only }
    Invoke-Button (Tab-MenuItem 'Close pane    Ctrl+Alt+Shift+W' -Panes)
    $null = Wait-Until 'only the original SSH shell remains' {
        $rows = @(Sessions)
        $rows.Count -eq 1 -and $rows[0].id -eq $sourceSsh.id -and $rows[0].active -and $rows[0].cols -gt $sourceSsh.cols
    }
    Invoke-Button (Tab-MenuItem 'Reopen last closed pane')
    $restored = Wait-Until 'fresh named and locked PowerShell in the original tab' {
        $rows = @(Sessions)
        $ssh = $rows | Where-Object id -eq $sourceSsh.id
        $local = $rows | Where-Object pane_name -ceq 'Local build'
        if ($rows.Count -eq 2 -and $local.active -and $local.read_only -and $local.id -ne $sourceShell.id -and
            $local.tab_id -eq $sourceSsh.tab_id -and $local.tab_title -ceq 'Operations' -and
            $ssh.connection_generation -eq $sourceSsh.connection_generation) { $local }
    }
    Wait-Split $restored.id $sourceSsh.id
    Wait-Text $restored.id 'PS '
    Invoke-Button (Tab-MenuItem 'Disable read-only pane input' -Panes)
    $null = Wait-Until 'restored input unlocked' { !((Sessions) | Where-Object id -eq $restored.id).read_only }
    $fresh = Identity $restored.id 'ReopenedPane' 'if ($null -eq $global:DupValue) { $global:DupValue="fresh" }; '
    if ($fresh[1].Value -eq $initial[1].Value -or $fresh[2].Value -cne 'fresh') { throw 'PowerShell reused the original PID or variables' }
    Send-Remote $sourceSsh.id 'after-local-reopen/'
    Check-History $sourceSsh.id 'before-local-close/' 'ReopenedPane='
    if ((Tab-MenuItem 'Reopen last closed pane').Current.IsEnabled) { throw 'Reopen slot was not consumed' }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
