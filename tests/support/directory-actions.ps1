function Check-DirectoryActions([bool]$Enabled) {
    $field = Open-Palette 'current directory'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('current directory')
    $null = Wait-Until "current-directory commands enabled=$Enabled" {
        foreach ($name in 'Split right in current directory','Split below in current directory','New tab in current directory') {
            $row = Find-Element $name ([System.Windows.Automation.ControlType]::Button)
            if (!$row -or $row.Current.IsEnabled -ne $Enabled) { return $false }
        }
        return $true
    }
    Press-Key 27
    $null = Wait-Until 'command palette closed' { !(Find-Element 'Search commands') }
}

function Wait-DirectoryChild($Origin, [int]$PaneCount, [string]$Destination) {
    Wait-Until "new active shell in the reported directory ($Destination)" {
        $sessions = @((Control @('list')).sessions)
        $source = $sessions | Where-Object id -eq $Origin.id
        $child = $sessions | Where-Object active
        if ($sessions.Count -ne $PaneCount -or !$source -or !$child.visible -or $source.id -eq $child.id -or
            $source.tab_id -ne $Origin.tab_id -or $source.connection_generation -ne $Origin.connection_generation -or
            $child.label -ne $Origin.label) { return }
        if ($Destination -eq 'tab') {
            if (!$source.visible -and $child.tab_id -ne $Origin.tab_id -and
                @($sessions | Where-Object tab_id -eq $child.tab_id).Count -eq 1) { $child }
            return
        }
        if (!$source.visible -or $child.tab_id -ne $Origin.tab_id) { return }
        if ($Destination -eq 'right') {
            if ($source.cols -lt 0.8 * $Origin.cols -and $child.cols -lt 0.8 * $Origin.cols -and
                [Math]::Abs($child.rows - $source.rows) -le 1) { $child }
        } elseif ($source.rows -lt 0.8 * $Origin.rows -and $child.rows -lt 0.8 * $Origin.rows -and
            [Math]::Abs($child.cols - $source.cols) -le 1) { $child }
    }
}
