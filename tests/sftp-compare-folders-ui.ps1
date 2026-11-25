[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_EXPORT_SMOKE_DIR = Join-Path $State 'exports'
[void][IO.Directory]::CreateDirectory($env:W11TERM_EXPORT_SMOKE_DIR)
$profiles = Join-Path $State 'profiles.json'
$fixture = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$combo = [System.Windows.Automation.ControlType]::ComboBox
$listItem = [System.Windows.Automation.ControlType]::ListItem
$value = [System.Windows.Automation.ValuePattern]::Pattern
$expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
$selection = [System.Windows.Automation.SelectionItemPattern]::Pattern
$dot = [char]0xb7
$dash = [char]0x2013
$reference = '/shell directory/archive'
$current = '/shell directory/live'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Wait-Range([int]$First, [int]$Last, [int]$Total) {
    $range = if ($Total -eq 1) { '1 difference' } else { "Showing $First$dash$Last of $Total differences" }
    $null = Wait-Until $range { Find-Element $range }
}

function Set-Query([string]$Query) {
    $field = Wait-Until 'relative path filter' { Find-Element 'Filter relative paths' $edit }
    $field.GetCurrentPattern($value).SetValue($Query)
}

function Select-DifferenceType([string]$Name) {
    $field = Wait-Until 'difference type filter' { Find-Element 'Folder difference type' $combo }
    $field.GetCurrentPattern($expand).Expand()
    $choice = Wait-Until "difference type '$Name'" { Find-Element $Name $listItem -Visible }
    $choice.GetCurrentPattern($selection).Select()
    $popup = (Find-Element 'Folder difference type' $combo).GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}

function Test-ComparisonExport {
    $save = 'Save comparison as JSON' + [char]0x2026
    $output = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'sftp-folder-comparison.json'
    $null = Wait-Until 'the comparison export button is visible' { Find-Element $save $button -Visible }
    Click $save
    $dialog = Wait-Until 'owned comparison Save dialog' { Find-OwnedDialog 'Save folder comparison as JSON' }
    $null = Wait-Until 'cancel the comparison save' { [FixtureDialog]::ClickButton($dialog, 2) }
    $null = Wait-Until 'the comparison is ready to export again' {
        $item = Find-Element $save $button
        $item -and $item.Current.IsEnabled
    }
    if ([IO.File]::Exists($output)) { throw 'Cancel created a comparison export' }
    Click $save
    $dialog = Wait-Until 'owned comparison Save dialog reopened' { Find-OwnedDialog 'Save folder comparison as JSON' }
    $null = Wait-Until 'save the comparison JSON' { [FixtureDialog]::ClickButton($dialog, 1) }
    $prefix = 'Saved folder comparison to '
    $published = Wait-Until 'an export status inside the comparison dialog confirms publication' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $byId = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-folder-comparison-export-status')
        $status = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $byId)
        if ($status -and $status.Current.Name.StartsWith('Could not save folder comparison.')) {
            throw $status.Current.Name
        }
        if ($status -and $status.Current.Name.StartsWith($prefix)) {
            $status.Current.Name.Substring($prefix.Length)
        }
    }
    # The shell picker can expand a short temporary-directory path to its long spelling.
    if (![IO.File]::Exists($output) -or ![IO.File]::Exists($published)) {
        throw "Expected the saved export at '$output'. The dialog reported '$published'"
    }
    $encoding = [Text.UTF8Encoding]::new($false, $true)
    $json = [IO.File]::ReadAllText($output, $encoding)
    if ([IO.File]::ReadAllText($published, $encoding) -cne $json) {
        throw "The export status refers to a different file: '$published'"
    }
    $report = $json | ConvertFrom-Json
    $modified = $report.differences | Where-Object path -ceq 'nested/data.txt'
    $later = $report.differences | Where-Object path -ceq 'bulk-239.txt'
    if ($report.schema_version -ne 1 -or $report.ssh.host -cne '127.0.0.1' -or $report.ssh.port -le 0 -or
        $report.ssh.user -cne 'test' -or $report.reference.path -cne $reference -or $report.reference.origin -cne 'remote' -or
        $report.current.path -cne $current -or $report.current.origin -cne 'remote' -or $report.differences.Count -ne 244 -or
        $report.counts.current_only -ne 241 -or $report.counts.needs_inspection -ne 1 -or
        $modified.change -cne 'metadata_changed' -or $modified.reference.size_bytes -ne 3 -or $modified.current.size_bytes -ne 8 -or
        $later.change -cne 'current_only' -or $null -ne $later.reference -or $later.current.kind -cne 'file') {
        throw 'Export did not preserve the complete comparison and the pinned metadata'
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $paneId = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$paneId",'--timeout','15','src/trace.rs')
    $connection = ((Control @('list')).sessions | Where-Object id -eq $paneId).connection_generation
    Run-Palette 'Open files here'
    Click 'archive/'
    Click 'Open folder'
    Click ('Set folder as comparison reference' + [char]0x2026)
    $null = Wait-Until 'the reference inventory is captured' {
        Find-Element "Captured folder reference $dot 4 entries"
    }
    Click 'Close folder reference'
    $null = Wait-Until 'the folder reference is visible in the drawer' {
        (Find-Element "Folder reference  $reference") -and !(Find-Element 'Close folder reference' $button)
    }
    [IO.File]::WriteAllText((Join-Path $fixture 'shell directory/archive/nested/data.txt'), 'current!', [Text.UTF8Encoding]::new($false))

    Click 'Back'
    Click 'live/'
    Click 'Open folder'
    Click ('Compare folder with reference' + [char]0x2026)
    $null = Wait-Until 'a comparison identifies both folders and the expected counts' {
        (Find-Element "Reference folder  $reference") -and (Find-Element $current) -and
            (Find-Element 'Only in reference  1') -and (Find-Element 'Only in current folder  241') -and
            (Find-Element 'File metadata changed  1') -and (Find-Element 'Needs inspection  1')
    }
    Wait-Range 1 40 244
    Click 'Next differences'
    Wait-Range 41 80 244
    Click 'Previous differences'
    Wait-Range 1 40 244
    Set-Query 'NESTED/DATA.TXT'
    Wait-Range 1 1 1
    $null = Wait-Until 'the changed nested file shows the pinned and current sizes' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $text = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Text)
        $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text) | Where-Object {
            $name = $_.Current.Name
            $name.StartsWith("File metadata changed $dot nested/data.txt") -and
                $name.Contains('Reference  File  3 bytes') -and $name.Contains('Current  File  8 bytes')
        }
    }
    Select-DifferenceType 'Needs inspection'
    $null = Wait-Until 'the path and change filters are intersected' {
        Find-Element 'No differences match these filters.'
    }
    Set-Query ''
    Wait-Range 1 1 1
    Test-ComparisonExport
    Wait-Range 1 1 1
    $hold = Join-Path $fixture 'hold-opendir'
    [IO.File]::WriteAllText($hold, "$current/nested")
    try {
        Click 'Compare again'
        $null = Wait-Until 'the refreshed comparison is scanning the directory' {
            Test-Path -LiteralPath (Join-Path $fixture 'opendir-paused')
        }
    } finally {
        [IO.File]::Delete($hold)
    }
    Wait-Range 1 1 1
    Select-DifferenceType 'All differences'
    Wait-Range 1 40 244
    Click 'Close folder comparison'
    $null = Wait-Until 'the comparison dialog is closed' { !(Find-Element 'Close folder comparison' $button) }
    Click 'Clear folder reference'
    $null = Wait-Until 'comparison actions are removed' {
        !(Find-Element "Folder reference  $reference") -and
            !(Find-Element ('Compare folder with reference' + [char]0x2026) $button)
    }
    $session = (Control @('list')).sessions[0]
    if ($session.id -ne $paneId -or $session.connection_generation -ne $connection) {
        throw 'Comparing folders changed the SSH connection'
    }
} catch {
    throw "$($_.Exception.Message)$([char]10)$($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
