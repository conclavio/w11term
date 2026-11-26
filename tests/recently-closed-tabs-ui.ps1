[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
$keymap.shortcuts.find_closed_tab = 'F3'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, ($keymap | ConvertTo-Json))
$value = [System.Windows.Automation.ValuePattern]::Pattern
$edit = [System.Windows.Automation.ControlType]::Edit
$dot = [char]0x00b7
$historyCommand = 'Recently closed tabs' + [char]0x2026 + '    F3'
$operations = "Operations    2 panes $dot Tab 1"
$scratch = "Scratch    1 pane $dot Tab 3"

function Find-RecentRows {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $button)
    $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition) |
        ForEach-Object { $_.Current.Name } | Where-Object { $_ -in $operations, $scratch }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--workspace',
    'duplicate', '--cols', '144', '--rows', '32') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $initial = @(Wait-Until 'both panes from Operations' {
        $all = @(Sessions)
        if ($all.Count -eq 2 -and @($all | Where-Object tab_title -eq 'Operations').Count -eq 2) { $all }
    })
    $ssh = $initial | Where-Object label -eq 'palette-ssh'
    $shell = $initial | Where-Object label -eq 'Palette PowerShell'
    Wait-Focus $shell.id
    Wait-Text $ssh.id 'ready'
    Wait-Text $shell.id 'PS '
    $oldShell = Identity $shell.id 'OriginalShell' '$global:DupValue="original"; '
    Send-Remote $ssh.id 'before-close/'

    $neighbor = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $neighbor
    Wait-Text $neighbor 'PS '
    $liveShell = Identity $neighbor 'LiveShell' '$global:DupValue="neighbor"; '
    $third = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $third
    Run-Palette ('Rename tab' + [char]0x2026) -Enter
    $name = Wait-Until 'Scratch tab name field' { Find-Element 'Tab name' $edit -Visible }
    $name.GetCurrentPattern($value).SetValue('Scratch')
    Invoke-Button (Wait-Until 'enabled tab name Save' {
        $item = Find-Element 'Save' $button -Visible
        if ($item -and $item.Current.IsEnabled) { $item }
    })
    $null = Wait-Until 'Scratch title committed' { (Sessions | Where-Object id -eq $third).tab_title -ceq 'Scratch' }
    $null = Wait-Until 'title editor closed' { !(Find-Element 'Tab name' $edit) }
    Press-Key 0x78 # F9 selects the tab that will survive the bulk close.
    Wait-Focus $neighbor
    Run-Palette 'Close other tabs' -Enter
    $null = Wait-Until 'bulk close leaves only the neighboring shell' {
        $all = @(Sessions)
        $all.Count -eq 1 -and $all[0].id -eq $neighbor -and $all[0].active
    }

    Run-Palette $historyCommand -Enter
    $field = Wait-PickerEditor 'Search closed tabs' 'the palette history command'
    $null = Wait-Until 'both history results in reverse close order' {
        $rows = @(Find-RecentRows)
        $rows.Count -eq 2 -and $rows[0] -ceq $scratch -and $rows[1] -ceq $operations
    }
    $field.GetCurrentPattern($value).SetValue('unsupported title')
    $null = Wait-Until 'no layouts match the query' { Find-Element 'No matching closed tabs' }
    Press-Key 27
    $null = Wait-Until 'Escape dismisses history' { !(Find-Element 'Search closed tabs' $edit) }
    Wait-Focus $neighbor

    Press-Key 0x72 # F3 opens the same picker directly.
    $field = Wait-PickerEditor 'Search closed tabs' 'the remapped history key'
    $field.GetCurrentPattern($value).SetValue('OPERATIONS test@127.0.0.1')
    $null = Wait-Until 'only the older mixed split matches the title and address' {
        $rows = @(Find-RecentRows)
        $rows.Count -eq 1 -and $rows[0] -ceq $operations
    }
    Press-Key 13
    $restored = @(Wait-Until 'the selected Operations split returns as two fresh sessions' {
        $all = @(Sessions)
        $split = @($all | Where-Object tab_title -eq 'Operations')
        if ($all.Count -eq 3 -and $split.Count -eq 2 -and
            !($split | Where-Object { $_.id -in $initial.id }) -and @($split.tab_id | Select-Object -Unique).Count -eq 1) { $split }
    })
    $newSsh = $restored | Where-Object label -eq 'palette-ssh'
    $newShell = $restored | Where-Object label -eq 'Palette PowerShell'
    Wait-Focus $newShell.id
    Wait-Text $newSsh.id 'ready'
    Wait-Text $newShell.id 'PS '
    $fresh = Identity $newShell.id 'FreshShell' 'if ($null -eq $global:DupValue) { $global:DupValue="fresh" }; '
    if ($fresh[1].Value -eq $oldShell[1].Value -or $fresh[2].Value -cne 'fresh') { throw 'The older layout reused its former shell' }
    Send-Remote $newSsh.id 'after-reopen/'
    Press-Key 0x78 # The surviving shell still occupies tab 2.
    Wait-Focus $neighbor
    $neighborAfter = Identity $neighbor 'NeighborAfter' ''
    if ($neighborAfter[1].Value -ne $liveShell[1].Value -or $neighborAfter[2].Value -cne 'neighbor') {
        throw 'Reopening the older tab replaced the surviving shell'
    }

    Run-Palette 'Reopen last closed tab layout' -Enter
    $null = Wait-Until 'the newer Scratch layout remains available to the last-closed action' {
        $all = @(Sessions)
        $all.Count -eq 4 -and ($all | Where-Object { $_.tab_title -ceq 'Scratch' -and $_.id -ne $third -and $_.active })
    }
    Press-Key 0x72
    $null = Wait-PickerEditor 'Search closed tabs' 'empty history after both layouts reopened'
    $null = Wait-Until 'the history is exhausted' { Find-Element 'No recently closed tab layouts' }
} finally {
    Close-FixtureApp $app
}
