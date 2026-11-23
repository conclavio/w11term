function Click([string]$Label) {
    Invoke-Button (Wait-Until "enabled '$Label'" {
        $item = Find-Element $Label $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Wait-Status([string]$Pattern) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $text = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    try {
        $null = Wait-Until "drawer message matching '$Pattern'" {
            foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text)) {
                if ($item.Current.Name -match $Pattern) { return $true }
            }
        }
    } catch {
        $labels = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text) | ForEach-Object { $_.Current.Name }
        throw "$_; visible text: $($labels -join ' | ')"
    }
}

function Type-Line([string]$Text) {
    foreach ($c in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
}

function Open-TransferFixture {
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    $ssh = (Control @('list')).sessions | Where-Object id -eq $remote
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'local neighboring pane' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','PS ')
    Type-Line '$global:folderTransferPid = $PID; "seeded-" + "local"'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','seeded-local')
    $null = Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    Run-Palette 'Open files here'
    $null = Wait-Until 'original SSH shell directory' { (Find-Element '/shell directory') -and (Find-Element 'readme.txt' $button) }
    return [pscustomobject]@{ Ssh = $ssh; LocalPane = $localPane }
}

function Confirm-TransferShells($Fixture, [string]$Marker) {
    $ssh = $Fixture.Ssh
    $remote = $ssh.id
    $localPane = $Fixture.LocalPane
    Run-Palette 'SFTP files'
    foreach ($c in $Marker.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15',$Marker)
    $panes = (Control @('list')).sessions
    $active = $panes | Where-Object id -eq $remote
    if ($panes.Count -ne 2 -or !$active.active -or $active.connection_generation -ne $ssh.connection_generation -or $active.tab_id -ne $ssh.tab_id) {
        throw 'The upload changed the original SSH pane, connection or tab'
    }
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $localPane
    Type-Line 'if ($folderTransferPid -eq $PID) { "local-still-" + "alive" }'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','local-still-alive')
}
