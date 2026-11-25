[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$snow = [char]0x96ea
$target = "zz-review-$snow.log"
$folder = 'zz-review-archive/'
$preview = 'Preview text' + [char]0x2026

function Click([string]$Label) {
    Invoke-Button (Wait-Until "enabled button $Label" {
        $item = Find-Element $Label $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Sort-Radio([string]$Label) {
    $item = Wait-Until "sort radio $Label" {
        $radio = Find-Element $Label ([System.Windows.Automation.ControlType]::RadioButton)
        if ($radio -and $radio.Current.IsEnabled) { $radio }
    }
    return $item.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
}
function Set-Sort([string]$Label) {
    (Sort-Radio $Label).Select()
    $null = Wait-Until "$Label selected" { (Sort-Radio $Label).Current.IsSelected }
}
function Size-Cell($Row) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-entry-size')
    return $Row.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
}
function Expect-Size([string]$Name, [int]$Bytes) {
    $null = Wait-Until "size cell for $Name has $Bytes bytes" {
        $row = Find-Element $Name $button
        if (!$row) { return }
        $size = Size-Cell $row
        if (!$size -or $size.Current.Name -cne "Size $Bytes bytes") { return }
        $bounds = $row.Current.BoundingRectangle
        $cell = $size.Current.BoundingRectangle
        !$cell.IsEmpty -and $cell.Width -ge 10 -and
            $cell.Left -gt $bounds.Left -and $cell.Right -le $bounds.Right + 1
    }
}
function Wait-Order([string[]]$Expected) {
    $observed = @{ entries = @() }
    $buttons = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $button)
    try {
        $null = Wait-Until "visible SFTP order $($Expected -join ', ')" {
            $list = Find-Element 'SFTP entries'
            if (!$list) { return $false }
            $observed.entries = @($list.FindAll([System.Windows.Automation.TreeScope]::Descendants,$buttons) |
                ForEach-Object { $_.Current.Name } | Where-Object { $_ -like '*.log' -or $_ -ceq $folder })
            if ($observed.entries.Count -lt $Expected.Count) { return $false }
            for ($index = 0; $index -lt $Expected.Count; $index++) {
                if ($observed.entries[$index] -cne $Expected[$index]) { return $false }
            }
            return $true
        }
    } catch { throw "$($_.Exception.Message). Actual entries: $($observed.entries -join ', ')" }
}
function Shell-Pid([string]$Tag) {
    $line = "'$Tag=' + " + '$PID'
    foreach ($c in $line.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $pattern = "$Tag=([0-9]+)"
    $frame = Control @('wait-for-text','--pane',"$localPane",'--regex','--timeout','15',$pattern)
    return [int]([regex]::Match($frame.text,$pattern).Groups[1].Value)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    $ssh = (Control @('list')).sessions | Where-Object id -eq $remote
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','20','PS ')
    $shellPid = Shell-Pid 'sort-before'
    $null = Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    Run-Palette 'Open files here'
    $null = Wait-Until 'initial listing' { (Find-Element '/shell directory') -and (Find-Element '1 / 2') }
    if (!(Sort-Radio 'Sort by name').Current.IsSelected -or (Sort-Radio 'Sort newest first').Current.IsSelected -or
        (Sort-Radio 'Sort largest first').Current.IsSelected) {
        throw 'The default sort is not visibly set to Name'
    }
    Wait-Order @($folder,'aa-review.log','batch-00.log','batch-01.log')
    Expect-Size 'aa-review.log' 18
    Expect-Size 'batch-00.log' 1536
    Expect-Size 'batch-01.log' 0
    if (Size-Cell (Find-Element $folder $button)) { throw 'A directory displayed a file size' }
    if (Find-Element $target $button) { throw 'Expected the Unicode target on the second page of the name sort' }
    Click 'Next'
    Click $target
    $null = Wait-Until 'target selected on page two' { (Find-Element '2 / 2') -and (Find-Element 'Selected entry actions' $button).Current.IsEnabled }

    # A concurrent edit makes the selected file the oldest on disk. Resorting
    # this existing listing must still use its original READDIR attributes.
    $fixtureDirectory = [IO.File]::ReadAllText((Join-Path $State 'fixture-directory'))
    [IO.File]::SetLastWriteTimeUtc((Join-Path $fixtureDirectory $target),[datetime]::new(2000,1,1,0,0,0,[DateTimeKind]::Utc))
    Set-Sort 'Sort newest first'
    $null = Wait-Until 'page follows selected file' { Find-Element '1 / 2' }
    Wait-Order @($folder,$target,'yy-review.log','batch-34.log','batch-33.log')
    if ((Sort-Radio 'Sort by name').Current.IsSelected) { throw 'Both sort orders remained selected' }
    Invoke-SftpEntryAction $preview # No row reselected after the reorder.
    $null = Wait-Until 'preview for the same selected Unicode file' {
        $body = Find-Element 'File contents'
        $body -and $body.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1) -ceq "selected Unicode file: $snow"
    }
    Click 'Close preview'
    Click ('Filter filenames' + [char]0x2026)
    $filter = Wait-Until 'filter edit' { Find-Element 'Filter filenames' ([System.Windows.Automation.ControlType]::Edit) }
    $filter.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('review')
    $null = Wait-Until 'four cached matches' { Find-Element '4 matches' }
    Click 'Done'
    Wait-Order @($folder,$target,'yy-review.log','aa-review.log')
    # Grow a different file after listing. Largest first must still use cached sizes.
    [IO.File]::WriteAllText((Join-Path $fixtureDirectory 'aa-review.log'), ('z' * 8192))
    Set-Sort 'Sort largest first'
    Wait-Order @($folder,$target,'aa-review.log','yy-review.log')
    Expect-Size 'aa-review.log' 18
    if ((Sort-Radio 'Sort by name').Current.IsSelected -or (Sort-Radio 'Sort newest first').Current.IsSelected) {
        throw 'Largest first left another sort order selected'
    }
    if (!(Find-Element '4 matches') -or !(Find-Element 'Selected entry actions' $button).Current.IsEnabled) {
        throw 'Largest first discarded the filter or selected file'
    }
    Set-Sort 'Sort by name'
    Wait-Order @($folder,'aa-review.log','yy-review.log',$target)
    if (!(Find-Element '4 matches') -or !(Find-Element 'Selected entry actions' $button).Current.IsEnabled) {
        throw 'Sorting discarded the filter or selected file'
    }
    Run-Palette 'SFTP files'
    $null = Wait-Until 'drawer closed' { !(Find-Element 'SFTP entries') }
    Wait-Focus $remote
    foreach ($c in 'ssh-sort-still-live'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ssh-sort-still-live')
    $current = (Control @('list')).sessions | Where-Object id -eq $remote
    if ($current.tab_id -ne $ssh.tab_id -or $current.connection_generation -ne $ssh.connection_generation) { throw 'Sorting replaced the SSH pane' }
    $null = Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $localPane
    if ((Shell-Pid 'sort-after') -ne $shellPid) { throw 'Sorting interrupted the neighboring PowerShell process' }
} catch {
    throw "$($_.Exception.Message) at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
