[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$peer = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$trace = Join-Path $peer 'delete-trace.jsonl'
$gate = Join-Path $peer 'hold-delete-reply'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$on = [System.Windows.Automation.ToggleState]::On
$off = [System.Windows.Automation.ToggleState]::Off
$ellipsis = [char]0x2026

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Get-UiText([string]$Id) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $item = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if ($item) { $item.Current.Name }
}

function Review-Delete([string[]]$Paths) {
    Click "Delete marked entries$ellipsis"
    $null = Wait-Until 'review shows every marked remote path' {
        $review = Get-UiText 'sftp-delete-review-paths'
        if (!$review) { return $false }
        foreach ($path in $Paths) {
            if (!$review.Contains($path)) { return $false }
        }
        return $true
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Click "Select entries$ellipsis"
    foreach ($name in 'a-empty/','b-occupied/','c-queued.txt') {
        (Wait-CheckBox "Mark $name" $off).Toggle()
        $null = Wait-CheckBox "Mark $name" $on
    }
    $paths = @('/shell directory/a-empty','/shell directory/b-occupied','/shell directory/c-queued.txt')
    Review-Delete $paths
    Click 'Cancel'
    $null = Wait-Until 'delete review closes' { !(Get-UiText 'sftp-delete-review-paths') }
    if ((Get-Item -LiteralPath $trace).Length -ne 0) { throw 'Cancelling the review sent a deletion' }
    Review-Delete $paths
    Click 'Delete entries'
    $null = Wait-Until 'nonempty folder stops the listing batch' {
        (Get-UiText 'sftp-listing-status') -like 'Deleted 1 of 3 marked entries. Stopped at /shell directory/b-occupied.*'
    }
    $null = Wait-CheckBox 'Mark b-occupied/' $on
    $null = Wait-CheckBox 'Mark c-queued.txt' $on
    Click 'Exit selection'

    Click "Find files$ellipsis"
    $query = Wait-Until 'search query' { Find-Element 'Filename contains' $edit }
    $query.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('search-batch.txt')
    Click 'Search'
    Click "Select results$ellipsis"
    Click 'Mark all results'
    $later = '/shell directory/nested/search-batch.txt'
    $null = Wait-CheckBox "Mark $later" $on
    Review-Delete @('/shell directory/search-batch.txt',$later)
    [IO.File]::WriteAllBytes($gate, [byte[]]@())
    Click 'Delete entries'
    $null = Wait-Until 'first delete is waiting for a reply' {
        ![IO.File]::Exists((Join-Path $peer 'shell directory/search-batch.txt'))
    }
    Click 'Stop after current delete'
    $null = Wait-Until 'queued delete is stopped' {
        $item = Find-Element "Stopping after current delete$ellipsis" $button
        $item -and !$item.Current.IsEnabled
    }
    [IO.File]::Delete($gate)
    $null = Wait-Until 'search reports the acknowledged delete and refreshes' {
        (Get-UiText 'sftp-search-status') -like 'Deleted 1 of 2 marked entries. Stopped after the current delete*'
    }
    $null = Wait-CheckBox "Mark $later" $on
} catch {
    $status = if ($window -and !$app.HasExited) {
        Get-UiText 'sftp-listing-status'
        Get-UiText 'sftp-search-status'
    }
    throw "$($_.Exception.Message)`nDrawer status: $status`n$($_.ScriptStackTrace)"
} finally {
    [IO.File]::Delete($gate)
    Close-FixtureApp $app
}
