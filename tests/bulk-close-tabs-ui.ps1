param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$tab = [System.Windows.Automation.ControlType]::TabItem
$menu = [System.Windows.Automation.ControlType]::MenuItem

function New-LocalPane([int]$Existing) {
    Run-Palette ('New tab with Palette PowerShell ' + [char]0xb7 + ' Local')
    $id = Wait-Until 'new active PowerShell tab' {
        $pane = Sessions | Where-Object { $_.active -and $_.label -eq 'Palette PowerShell' -and $_.id -ne $Existing }
        if ($pane) { $pane.id }
    }
    Wait-Text $id 'PS '
    return $id
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','120','--rows','28') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $original = @(Sessions)[0]
    $ssh = $original.id
    Wait-Text $ssh 'ready'
    Send-Remote $ssh 'before/'
    Run-Palette 'Pin tab'
    $null = Wait-Until 'pinned SSH tab' { Find-Element 'Pinned: palette-ssh' $tab }

    $left = New-LocalPane 0
    $before = Identity $left 'BeforeCloseRight' '$global:DupValue="original"; '
    Run-Palette 'Settings'
    $null = Wait-Until 'open Settings tab' { Find-Element 'Settings' $tab }
    $right = New-LocalPane $left
    $closing = Identity $right 'OriginalRight' '$global:DupValue="closing"; '

    Press-Key 0x78 # F9 selects the second tab, beside the pinned SSH tab
    Wait-Focus $left
    Run-Palette 'Close tabs to the right'
    $null = Wait-Until 'only pinned SSH and the selected PowerShell remain' {
        $panes = @(Sessions)
        $panes.Count -eq 2 -and ($panes | Where-Object { $_.id -eq $left -and $_.active }) -and
            ($panes | Where-Object id -eq $ssh) -and !(Find-Element 'Settings' $tab)
    }
    $after = Identity $left 'AfterCloseRight' ''
    if ($after[1].Value -cne $before[1].Value -or $after[2].Value -cne 'original') {
        throw 'Close tabs to the right replaced the selected PowerShell shell'
    }

    Run-Palette 'Reopen last closed tab layout'
    $reopened = Wait-Until 'fresh PowerShell restored from close history' {
        $pane = Sessions | Where-Object { $_.active -and $_.label -eq 'Palette PowerShell' -and $_.id -ne $left -and $_.id -ne $right }
        if ($pane) { $pane.id }
    }
    Wait-Text $reopened 'PS '
    $fresh = Identity $reopened 'ReopenedRight' 'if ($null -eq $global:DupValue) { $global:DupValue="fresh" }; '
    if ($fresh[1].Value -ceq $closing[1].Value -or $fresh[2].Value -cne 'fresh') {
        throw 'Close history did not start a fresh shell for the closed tab'
    }

    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' $button })
    Invoke-Button (Wait-Until 'Close other tabs' { Find-Element 'Close other tabs' $menu })
    $null = Wait-Until 'only pinned SSH and the reopened PowerShell remain' {
        $panes = @(Sessions)
        $panes.Count -eq 2 -and ($panes | Where-Object { $_.id -eq $reopened -and $_.active }) -and
            ($panes | Where-Object { $_.id -eq $ssh -and $_.connection_generation -eq $original.connection_generation })
    }
    $after = Identity $reopened 'AfterCloseOther' ''
    if ($after[1].Value -cne $fresh[1].Value -or $after[2].Value -cne 'fresh') {
        throw 'Close other tabs replaced the selected PowerShell shell'
    }
    Send-Remote $ssh 'after/'
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
