[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$verifyClipboard = $env:GITHUB_ACTIONS -ceq 'true' -and $env:RUNNER_ENVIRONMENT -ceq 'github-hosted'
if ($verifyClipboard) {
    . (Join-Path $PSScriptRoot 'support/clipboard.ps1')
} else {
    $env:W11TERM_TEST_COPY_FILE = Join-Path $State 'copied-path.txt'
}
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$picker = Join-Path $State 'picker'
$env:W11TERM_SFTP_PICKER_SMOKE_DIR = $picker
$null = New-Item -ItemType Directory -Path (Join-Path $picker 'received')
$peerRoot = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$readGate = Join-Path $peerRoot 'hold-transfer-read'
$refreshGate = Join-Path $peerRoot 'hold-opendir'
$profiles = Join-Path $State 'profiles.json'
$account = (Get-Content $profiles -Raw | ConvertFrom-Json).profiles.'palette-ssh'
$parent = '/shell directory'
# Windows PowerShell 5.1 also reads this fixture on non-UTF-8 code pages.
$word = ([char]0xc5).ToString() + 'ngstr' + [char]0xf6 + 'm'
$query = '[' + [char]0xe5 + 'NGSTR' + [char]0xd6 + 'M]'
$target = "zz-[$word].txt"
$decoy = "zz-$word.txt"
$added = "zz-[$word]-updated.txt"
$download = 'Download' + [char]0x2026
$rename = 'Rename' + [char]0x2026
$filterAction = 'Filter filenames' + [char]0x2026

function Button([string]$Name) {
    Find-Element $Name ([System.Windows.Automation.ControlType]::Button)
}
function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $b = Button $Name
        if ($b -and $b.Current.IsEnabled) { $b }
    })
}
function Filter-Edit {
    Find-Element 'Filter filenames' ([System.Windows.Automation.ControlType]::Edit)
}
function Wait-FilenameSearchFocus {
    Wait-Until 'focused recursive filename field' {
        $query = Find-Element 'Filename contains' ([System.Windows.Automation.ControlType]::Edit)
        if ($query -and $query.Current.HasKeyboardFocus) { $query }
    }
}
function Focus-FilenameSearch {
    $query = Find-Element 'Filename contains' ([System.Windows.Automation.ControlType]::Edit)
    $query.SetFocus()
    $null = Wait-FilenameSearchFocus
}
function Get-AdvancedSearchSummary {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-search-filters-summary')
    $summary = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if ($summary) { $summary.Current.Name }
}
function Set-AdvancedSearchField([string]$Label, [string]$Value, [string]$ExpectedSummary) {
    if (Button 'More filters') { Click 'More filters' }
    $edit = Wait-Until "advanced search field '$Label'" {
        Find-Element $Label ([System.Windows.Automation.ControlType]::Edit)
    }
    $edit.SetFocus()
    $null = Wait-Until "'$Label' owns keyboard focus" { $edit.Current.HasKeyboardFocus }
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value)
    Click 'Hide filters'
    $null = Wait-Until "collapsed search shows '$ExpectedSummary'" {
        $summary = Get-AdvancedSearchSummary
        $summary -and $summary.Contains($ExpectedSummary)
    }
}
function Set-SearchChoice([string]$Control, [string]$Name) {
    $controlType = [System.Windows.Automation.ControlType]::ComboBox
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $selector = Wait-Until "enabled '$Control' selector" {
        $box = Find-Element $Control $controlType
        if ($box -and $box.Current.IsEnabled) { $box }
    }
    $selector.GetCurrentPattern($expand).Expand()
    $choice = Wait-Until "'$Control' option '$Name'" {
        Find-Element $Name ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $popup = Wait-Until "'$Control' with '$Name' selected" {
        $box = Find-Element $Control $controlType
        if ($box) {
            $selected = $box.GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
            if ($selected.Count -eq 1 -and $selected[0].Current.Name -ceq $Name) {
                $box.GetCurrentPattern($expand)
            }
        }
    }
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}
function Check-PathCopy([string]$ButtonName, [string]$Expected) {
    $control = Wait-Until "available '$ButtonName' command" {
        $b = Button $ButtonName
        if ($b -and $b.Current.IsEnabled) { $b }
    }
    if ($verifyClipboard) {
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Invoke-Button $control
        $null = Wait-Until "exact clipboard path from '$ButtonName'" {
            [FixtureClipboard]::ContainsNewText($before, $Expected)
        }
    } else {
        [IO.File]::Delete($env:W11TERM_TEST_COPY_FILE)
        Invoke-Button $control
        $null = Wait-Until "exact fixture clipboard text from '$ButtonName'" {
            (Read-ClipboardFixture) -ceq $Expected
        }
    }
}
function Filter-Value {
    (Filter-Edit).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
}
function Open-Filter {
    $existing = Filter-Edit
    if (!$existing) { Click $filterAction }
    elseif (!$existing.Current.HasKeyboardFocus) { $existing.SetFocus() }
    Wait-Until 'focused filename editor' {
        $field = Filter-Edit
        if ($field -and $field.Current.HasKeyboardFocus) { $field }
    }
}
function Set-Filter([string]$Text) {
    (Open-Filter).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Text)
}
function Save-SearchResult([string]$Result, [string]$Destination) {
    Click $Result
    Set-DownloadDestination $Destination
}
function Set-DownloadDestination([string]$Destination) {
    $dialog = Wait-Until 'owned download save dialog from the result' { Find-OwnedDialog 'Save As' }
    $null = Wait-Until 'enter download destination' {
        [FixtureDialog]::SetFilename($dialog, $Destination, 'nested-content.txt')
    }
    $null = Wait-Until 'accept download destination' { [FixtureDialog]::ClickButton($dialog, 1) }
}
function Save-MarkedResults([string]$Destination) {
    Click 'Download marked results'
    $dialog = Wait-Until 'destination picker for marked search results' {
        Find-OwnedDialog 'Download marked search results'
    }
    $null = Wait-Until 'choose the new download folder name' {
        [FixtureDialog]::SetFilename($dialog, $Destination, 'Search results')
    }
    $null = Wait-Until 'start marked search download' { [FixtureDialog]::ClickButton($dialog, 1) }
}
function Read-SearchProperties([string]$Path, [string]$Type) {
    Click "Properties $Path"
    $field = Wait-Until 'fresh Properties from the search result' { Find-Element 'File properties' }
    $text = $field.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1)
    if ($text -cnotlike "*Path  $Path*" -or $text -cnotlike "*Type  $Type*") {
        throw "Unexpected properties for $Path. $text"
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    $null = Wait-Until 'first page of the shell directory' { (Find-Element $parent) -and (Button 'build-00.log') -and (Find-Element '1 / 2') }
    if (Button $target) { throw 'The Unicode target must begin outside page one' }
    $null = Wait-Until 'Copy selected path disabled without a selection' {
        $b = Button 'Copy selected path'
        $b -and !$b.Current.IsEnabled
    }
    Check-PathCopy 'Copy directory path' $parent
    Click 'Next'
    Click $target
    $null = Wait-Until 'file selected on page two' { (Find-Element '2 / 2') -and (Button $download).Current.IsEnabled }

    $null = Open-Filter
    $inputHwnd = Press-FocusedKey 0x54
    try {
        $null = Wait-Until 'typed T reaches the focused native filter' { (Filter-Value) -ieq 't' }
    } catch {
        $screen = (Control @('get-text','--pane',"$remote")).text
        throw "$_ Input HWND $inputHwnd, owner $window, editor value '$(Filter-Value)', shell: $screen"
    }
    Set-Filter $query
    $null = Wait-Until 'case-insensitive literal Unicode match from page two' {
        (Find-Element '1 match') -and (Button $target) -and !(Button $decoy) -and (Button $download).Current.IsEnabled
    }
    Click 'Done'
    $null = Wait-Until 'Done retains the filtered rows and returns keyboard input to SSH' {
        !(Filter-Edit) -and (Find-Element '1 match') -and (Button $target)
    }
    foreach ($c in 'after-filter-done'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','after-filter-done')
    Check-PathCopy 'Copy selected path' "$parent/$target"
    Check-PathCopy 'Copy directory path' $parent
    Set-Filter 'nothing-matches-this'
    $null = Wait-Until 'no matches and no actions on the old selected file' {
        (Find-Element 'No matching files or folders') -and !(Button $target) -and
            !(Button $download).Current.IsEnabled -and !(Button $rename).Current.IsEnabled -and
            !(Button 'Copy selected path').Current.IsEnabled
    }
    Click 'Clear filter'
    $null = Wait-Until 'unfiltered first page without a selection' {
        (Button 'build-00.log') -and (Find-Element '1 / 2') -and !(Button $download).Current.IsEnabled
    }
    Set-Filter 'build-'
    $null = Wait-Until 'all 38 matching rows counted' { Find-Element '38 matches' }
    Click 'Next'
    $null = Wait-Until 'second page of filtered results' { (Find-Element '2 / 2') -and (Button 'build-37.log') -and !(Button 'build-00.log') }
    Set-Filter $query
    $null = Wait-Until 'new query starts on its first page' { (Find-Element '1 match') -and (Button $target) }

    # Another actor adds a matching file. Its arrival in the UI proves the
    # refresh completed before we assert the same-directory filtering policy.
    $upload = Join-Path $State 'updated.txt'
    [IO.File]::WriteAllBytes($upload, [byte[]](23))
    $null = Control @('sftp','--pane',"$remote",'put',$upload,"$parent/$added")
    Click 'Refresh'
    $null = Wait-Until 'refreshed listing still uses the same query' {
        (Find-Element '2 matches') -and (Button $added) -and !(Button $decoy)
    }
    $null = Open-Filter
    if ((Filter-Value) -cne $query) { throw 'Refresh changed the query' }
    Set-Filter 'nested'
    Click 'nested/'
    Click 'Open folder'
    $null = Wait-Until 'child directory begins unfiltered' {
        (Find-Element "$parent/nested") -and (Button 'nested-content.txt') -and !(Button 'Open folder').Current.IsEnabled
    }
    Check-PathCopy 'Copy directory path' "$parent/nested"
    $null = Open-Filter
    if ((Filter-Value) -cne '') { throw 'Child directory retained a filter from its parent' }
    Click 'Done'
    Click 'Back'
    $null = Wait-Until 'parent directory restored' { Find-Element $parent }
    Click ('Find files' + [char]0x2026)
    $field = Wait-FilenameSearchFocus
    Set-SearchChoice 'Search result type' 'Folders'
    $null = Wait-Until 'type-only search enabled' {
        $run = Button 'Search'
        $run -and $run.Current.IsEnabled
    }
    Focus-FilenameSearch
    $null = Press-FocusedKey 13
    $null = Wait-Until 'folder-only search omits files and still sees nested content' {
        (Find-Element '1 matches in 2 folders') -and (Button "Reveal $parent/nested") -and
            !(Button "Reveal $parent/nested/nested-content.txt")
    }
    $value = [System.Windows.Automation.ValuePattern]::Pattern
    $excludeLabel = 'Exclude folder paths (regex)'
    if (Find-Element $excludeLabel ([System.Windows.Automation.ControlType]::Edit)) {
        throw 'Advanced search filters started expanded'
    }
    Click 'More filters'
    $excludeField = Wait-Until 'folder exclusion field is visible after expanding' {
        Find-Element $excludeLabel ([System.Windows.Automation.ControlType]::Edit)
    }
    $excludeField.GetCurrentPattern($value).SetValue('^nested$')
    $null = Wait-Until 'exclusion edit invalidates the previous results' {
        (Button 'Search') -and !(Button "Reveal $parent/nested")
    }
    $excludeField.SetFocus()
    $null = Wait-Until 'folder exclusion field has keyboard focus' { $excludeField.Current.HasKeyboardFocus }
    $null = Press-FocusedKey 13
    $null = Wait-Until 'Enter searches without opening or listing the excluded folder' {
        (Find-Element '0 matches in 1 folders. 1 folder excluded') -and !(Button "Reveal $parent/nested")
    }
    $excludeField = Find-Element $excludeLabel ([System.Windows.Automation.ControlType]::Edit)
    $excludeField.GetCurrentPattern($value).SetValue('')
    Set-SearchChoice 'Search result type' 'All types'
    $contentLabel = 'File text contains'
    $fixtureText = Join-Path $peerRoot 'shell directory/nested/nested-content.txt'
    $fixtureContents = "header`r`n" + ("unrelated line`r`n" * 40) +
        "Context before`r`nFixture needle value`r`nContext after`r`n" +
        ("later unrelated line`r`n" * 31) +
        "second fixture needle`r`ncontext in between`r`nthird fixture needle`r`nlast line`r`n"
    [IO.File]::WriteAllText($fixtureText, $fixtureContents)
    $contentField = Find-Element $contentLabel ([System.Windows.Automation.ControlType]::Edit)
    $contentField.GetCurrentPattern($value).SetValue('FIXTURE NEEDLE')
    $null = Wait-Until 'a content query enables search without a filename' {
        $run = Button 'Search'
        $run -and $run.Current.IsEnabled
    }
    $contentField.SetFocus()
    $null = Wait-Until 'file text field has keyboard focus' { $contentField.Current.HasKeyboardFocus }
    $null = Press-FocusedKey 13
    $excerpt = 'Line 43 ' + [char]0xb7 + ' Fixture needle value'
    $null = Wait-Until 'content search displays the matching line and scan totals' {
        (Find-Element '1 matches in 2 folders. 42 text files inspected') -and
            (Find-Element $excerpt) -and (Button "Reveal $parent/nested/nested-content.txt")
    }
    $previewMatch = "Preview match $parent/nested/nested-content.txt"
    [IO.File]::WriteAllText($fixtureText, "new first line`r`n$fixtureContents")
    Click $previewMatch
    $matchOne = 'Match 1 of 3 ' + [char]0xb7 + ' Line 44'
    $matchTwo = 'Match 2 of 3 ' + [char]0xb7 + ' Line 77'
    $matchThree = 'Match 3 of 3 ' + [char]0xb7 + ' Line 79'
    $null = Wait-Until 'preview starts at the current matching line with context' {
        $previous = Button 'Previous match'
        $next = Button 'Next match'
        (Find-Element $matchOne -Visible) -and
            (Find-Element 'Fixture needle value' -Visible) -and
            (Find-Element 'Context before' -Visible) -and
            (Find-Element 'Context after' -Visible) -and !(Find-Element 'File contents') -and
            $previous -and !$previous.Current.IsEnabled -and $next -and $next.Current.IsEnabled
    }
    Click 'Next match'
    $null = Wait-Until 'the second matching line is visible' { Find-Element $matchTwo -Visible }
    Click 'Next match'
    $null = Wait-Until 'the last matching line disables Next match' {
        $next = Button 'Next match'
        (Find-Element $matchThree -Visible) -and $next -and !$next.Current.IsEnabled
    }
    Click 'Previous match'
    $null = Wait-Until 'Previous match returns to the second line' { Find-Element $matchTwo -Visible }
    Click 'Show full file'
    $fullText = Wait-Until 'the full file is available without a second search' {
        Find-Element 'File contents'
    }
    $actualText = $fullText.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1)
    if (!$actualText.StartsWith('new first line') -or !$actualText.Contains('last line')) {
        throw 'The full preview omitted text outside the match context'
    }
    Click 'Back to match'
    $null = Wait-Until 'the selected match is visible again' { Find-Element $matchTwo -Visible }
    Click 'Close preview'
    $null = Wait-Until 'the match preview closes' { !(Button 'Close preview') }
    [IO.File]::WriteAllText($fixtureText, 'The file was changed.')
    Click $previewMatch
    $null = Wait-Until 'the stale search hit opens the current file with an explanation' {
        (Find-Element 'The current file no longer matches the search.' -Visible) -and
            (Find-Element 'File contents') -and !(Button 'Show full file')
    }
    Click 'Close preview'
    $contentField = Wait-Until 'the changed file preview closes and restores the search field' {
        $field = Find-Element $contentLabel ([System.Windows.Automation.ControlType]::Edit)
        if (!(Button 'Close preview') -and $field -and $field.Current.IsEnabled) { $field }
    }
    $contentField.GetCurrentPattern($value).SetValue('')
    $null = Wait-Until 'clearing the content query disables the empty search' {
        $run = Button 'Search'
        $run -and !$run.Current.IsEnabled
    }
    [IO.File]::WriteAllText($fixtureText, ('f' * 70000))
    $minSizeLabel = 'Minimum file size'
    $maxSizeLabel = 'Maximum file size'
    $fileOnPageTwo = "$parent/nested/nested-content.txt"
    $minimumSize = Find-Element $minSizeLabel ([System.Windows.Automation.ControlType]::Edit)
    $maximumSize = Find-Element $maxSizeLabel ([System.Windows.Automation.ControlType]::Edit)
    $minimumSize.GetCurrentPattern($value).SetValue('1 KiB')
    $maximumSize.GetCurrentPattern($value).SetValue('70 KiB')
    $null = Wait-Until 'file size bounds allow a search without a name' {
        $run = Button 'Search'
        $run -and $run.Current.IsEnabled
    }
    $maximumSize.SetFocus()
    $null = Wait-Until 'maximum file size has keyboard focus' { $maximumSize.Current.HasKeyboardFocus }
    $null = Press-FocusedKey 13
    $null = Wait-Until 'only the file within the size range is returned' {
        (Find-Element '1 matches in 2 folders') -and (Button "Reveal $fileOnPageTwo") -and
            !(Button "Reveal $parent/build-00.log") -and !(Button "Reveal $parent/nested")
    }
    $minimumSize = Find-Element $minSizeLabel ([System.Windows.Automation.ControlType]::Edit)
    $maximumSize = Find-Element $maxSizeLabel ([System.Windows.Automation.ControlType]::Edit)
    $minimumSize.GetCurrentPattern($value).SetValue('')
    $maximumSize.GetCurrentPattern($value).SetValue('')
    $null = Wait-Until 'clearing both sizes disables the empty search' {
        $run = Button 'Search'
        $run -and !$run.Current.IsEnabled
    }
    $fromLabel = 'Modified from (UTC)'
    $throughLabel = 'Modified through (UTC)'
    $fromField = Find-Element $fromLabel ([System.Windows.Automation.ControlType]::Edit)
    $throughField = Find-Element $throughLabel ([System.Windows.Automation.ControlType]::Edit)
    $fromField.GetCurrentPattern($value).SetValue('2037-01-01')
    $throughField.GetCurrentPattern($value).SetValue('2037-01-01')
    $null = Wait-Until 'date bounds allow a search without a name' {
        $run = Button 'Search'
        $run -and $run.Current.IsEnabled
    }
    $throughField.SetFocus()
    $null = Wait-Until 'modified through has keyboard focus' { $throughField.Current.HasKeyboardFocus }
    $null = Press-FocusedKey 13
    $null = Wait-Until 'date-only search returns the file for the selected UTC day' {
        (Find-Element '1 matches in 2 folders') -and (Button "Reveal $parent/build-37.log") -and
            !(Button "Reveal $parent/build-00.log") -and !(Button "Reveal $parent/nested")
    }
    $fromField = Find-Element $fromLabel ([System.Windows.Automation.ControlType]::Edit)
    $throughField = Find-Element $throughLabel ([System.Windows.Automation.ControlType]::Edit)
    $fromField.GetCurrentPattern($value).SetValue('')
    $throughField.GetCurrentPattern($value).SetValue('')
    $null = Wait-Until 'clearing both dates disables the empty search' {
        $run = Button 'Search'
        $run -and !$run.Current.IsEnabled
    }
    $on = [System.Windows.Automation.ToggleState]::On
    $off = [System.Windows.Automation.ToggleState]::Off
    (Wait-CheckBox 'Regex' $off).Toggle()
    (Wait-CheckBox 'Match case' $off).Toggle()
    (Wait-CheckBox 'Match path' $off).Toggle()
    $null = Wait-CheckBox 'Regex' $on
    $null = Wait-CheckBox 'Match case' $on
    $null = Wait-CheckBox 'Match path' $on
    $field = Wait-Until 'relative path search field' {
        Find-Element 'Relative path contains' ([System.Windows.Automation.ControlType]::Edit)
    }
    $newest = "$parent/build-37.log"
    $largeFilesPattern = '^build-[0-9]+\.log$|^nested/nested-content\.txt$'
    $savedExclusion = '^cache$'
    $savedThrough = '2099-12-31'
    $field.GetCurrentPattern($value).SetValue($largeFilesPattern)
    $null = Wait-Until 'relative-path query is ready before adding exclusions' {
        $run = Button 'Search'
        $run -and $run.Current.IsEnabled
    }
    Set-AdvancedSearchField $excludeLabel $savedExclusion "Exclude folders $savedExclusion"
    Set-AdvancedSearchField $maxSizeLabel '1 MiB' 'Max size 1 MiB'
    Set-AdvancedSearchField $throughLabel $savedThrough "Modified through $savedThrough UTC"
    Click 'Search'
    $null = Wait-Until 'folder-sorted recursive results span two pages' {
        (Find-Element '1 / 2') -and (Button "Reveal $parent/build-00.log") -and
            !(Button "Reveal $fileOnPageTwo") -and !(Button "Reveal $newest")
    }
    Click ('Select downloads' + [char]0x2026)
    Click 'Next results'
    $null = Wait-Until 'recursive results page two' {
        (Find-Element '2 / 2') -and (Button "Reveal $fileOnPageTwo")
    }
    (Wait-CheckBox "Mark $fileOnPageTwo" $off).Toggle()
    $null = Wait-CheckBox "Mark $fileOnPageTwo" $on
    Set-SearchChoice 'Sort search results' 'Largest first'
    $null = Wait-Until 'largest file moves to the first page with its reported size' {
        (Find-Element '1 / 2') -and (Button "Reveal $fileOnPageTwo") -and
            (Find-Element '68.4 KiB') -and !(Button "Reveal $newest")
    }
    $null = Wait-CheckBox "Mark $fileOnPageTwo" $on
    $savedSearchesButton = 'Saved searches' + [char]0x2026
    Click $savedSearchesButton
    $presetName = Wait-Until 'saved search name field' {
        Find-Element 'Saved search name' ([System.Windows.Automation.ControlType]::Edit)
    }
    $presetName.GetCurrentPattern($value).SetValue('Large build files')
    Click 'Save current search'
    try {
        $null = Wait-Until 'saved preset is written to the SSH profile' {
            $data = (Get-Content $profiles -Raw | ConvertFrom-Json).profiles.'palette-ssh'.sftp_searches
            if ($data) {
                $entry = $data[0].searches.'Large build files'
                $entry.root -ceq $parent -and $entry.query -ceq $largeFilesPattern -and
                    $entry.filter.regex -and $entry.filter.match_case -and $entry.filter.match_path -and
                    $entry.filter.exclude_dir_regex -ceq $savedExclusion -and
                    $entry.filter.file_size.maximum -eq 1048576 -and
                    $entry.filter.modified_dates.through -ceq $savedThrough -and
                    $entry.sort -ceq 'largest'
            }
        }
    } catch {
        $data = (Get-Content $profiles -Raw | ConvertFrom-Json).profiles.'palette-ssh'.sftp_searches
        $saved = $data | ConvertTo-Json -Depth 8 -Compress
        throw "$_ Observed saved searches: $saved"
    }
    Click 'Hide saved searches'
    Set-SearchChoice 'Sort search results' 'Newest first'
    $null = Wait-Until 'newest file moves onto the first page' {
        (Find-Element '1 / 2') -and (Button "Reveal $newest") -and
            (Find-Element '2037-01-01 00:00:00 UTC')
    }
    Set-SearchChoice 'Sort search results' 'Sort by folder'
    $null = Wait-Until 'original first page restored without rescanning' {
        (Find-Element '1 / 2') -and (Button "Reveal $parent/build-00.log") -and
            !(Button "Reveal $fileOnPageTwo")
    }
    Click 'Back to files'
    Click 'nested/'
    Click 'Open folder'
    $null = Wait-Until 'the drawer has moved away from the saved search root' {
        (Find-Element "$parent/nested") -and (Button 'nested-content.txt')
    }
    Click ('Find files' + [char]0x2026)
    $null = Wait-FilenameSearchFocus
    Click $savedSearchesButton
    Click 'Run saved search'
    $null = Wait-Until 'saved search uses its original root, criteria, and sort' {
        (Find-Element "Find files in $parent") -and
            (Find-Element '39 matches in 2 folders') -and
            (Button "Reveal $fileOnPageTwo") -and (Find-Element '68.4 KiB')
    }
    $null = Wait-CheckBox 'Regex' $on
    $null = Wait-CheckBox 'Match case' $on
    $null = Wait-CheckBox 'Match path' $on
    $null = Wait-Until 'saved advanced filters shown while collapsed' {
        $summary = Get-AdvancedSearchSummary
        $summary -and $summary.Contains("Exclude folders $savedExclusion") -and
            $summary.Contains('Max size 1048576') -and $summary.Contains("Modified through $savedThrough UTC")
    }
    Click 'More filters'
    $excludeField = Wait-Until 'saved advanced fields are visible after expanding' {
        Find-Element $excludeLabel ([System.Windows.Automation.ControlType]::Edit)
    }
    if ($excludeField.GetCurrentPattern($value).Current.Value -cne $savedExclusion) {
        throw 'Saved search did not restore its folder exclusion'
    }
    $maximumSize = Find-Element $maxSizeLabel ([System.Windows.Automation.ControlType]::Edit)
    if ($maximumSize.GetCurrentPattern($value).Current.Value -cne '1048576') {
        throw 'Saved search did not restore its maximum size'
    }
    $throughField = Find-Element $throughLabel ([System.Windows.Automation.ControlType]::Edit)
    if ($throughField.GetCurrentPattern($value).Current.Value -cne $savedThrough) {
        throw 'Saved search did not restore its modified-through date'
    }
    Click $savedSearchesButton
    Click 'Remove saved search'
    $null = Wait-Until 'removed preset disappears from picker and profile' {
        $profile = (Get-Content $profiles -Raw | ConvertFrom-Json).profiles.'palette-ssh'
        (Find-Element 'No saved searches') -and !$profile.PSObject.Properties['sftp_searches']
    }
    Click 'Hide saved searches'
    Set-SearchChoice 'Sort search results' 'Sort by folder'
    $excludeField = Find-Element $excludeLabel ([System.Windows.Automation.ControlType]::Edit)
    $excludeField.GetCurrentPattern($value).SetValue('')
    $maximumSize = Find-Element $maxSizeLabel ([System.Windows.Automation.ControlType]::Edit)
    $maximumSize.GetCurrentPattern($value).SetValue('')
    $throughField = Find-Element $throughLabel ([System.Windows.Automation.ControlType]::Edit)
    $throughField.GetCurrentPattern($value).SetValue('')
    (Wait-CheckBox 'Match path' $on).Toggle()
    $null = Wait-CheckBox 'Match path' $off
    $field = Wait-Until 'filename search field restored' {
        Find-Element 'Filename contains' ([System.Windows.Automation.ControlType]::Edit)
    }
    $field.GetCurrentPattern($value).SetValue('')
    $null = Wait-Until 'cleared filename query disables Search' {
        $search = Button 'Search'
        $search -and !$search.Current.IsEnabled
    }
    $field.GetCurrentPattern($value).SetValue('(')
    Click 'Search'
    $searchView = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $searchStatus = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-search-status')
    try {
        $null = Wait-Until 'invalid filename regex reported' {
            $status = $searchView.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $searchStatus)
            $status -and $status.Current.Name.Contains('Invalid filename regex')
        }
    } catch {
        $status = $searchView.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $searchStatus)
        $field = Find-Element 'Filename contains' ([System.Windows.Automation.ControlType]::Edit)
        $query = if ($field) { $field.GetCurrentPattern($value).Current.Value }
        throw "$_ Observed query '$query' and status '$($status.Current.Name)'."
    }
    $pattern = '^nested(?:-(?:added|content|renamed)\.txt)?$|^BUILD-00\.log$'
    $field = Find-Element 'Filename contains' ([System.Windows.Automation.ControlType]::Edit)
    $field.GetCurrentPattern($value).SetValue($pattern)
    Click 'Search'
    Read-SearchProperties "$parent/nested" 'Directory'
    Click 'Close properties'
    $fileResult = "Download $parent/nested/nested-content.txt"
    $destination = Join-Path $picker 'from-search.txt'
    [IO.File]::WriteAllBytes($readGate, [byte[]]@())
    Save-SearchResult $fileResult $destination
    $null = Wait-Until 'search panel shows active download progress' {
        (Find-Element '32768 / 70000 bytes') -and (Button 'Cancel transfer').Current.IsEnabled
    }
    Click 'Cancel transfer'
    $null = Wait-Until 'search panel shows cancellation in progress' {
        $cancel = Button 'Cancel transfer'
        $cancel -and !$cancel.Current.IsEnabled
    }
    [IO.File]::Delete($readGate)
    $null = Wait-Until 'cancelled search download keeps results and leaves no destination' {
        !(Button 'Cancel transfer') -and ![IO.File]::Exists($destination) -and
            (Button $fileResult).Current.IsEnabled
    }
    $contents = 'f' * 70000
    $hasher = [Security.Cryptography.SHA256]::Create()
    try { $expectedDigest = -join ($hasher.ComputeHash([Text.Encoding]::UTF8.GetBytes($contents)) | ForEach-Object { $_.ToString('x2') }) }
    finally { $hasher.Dispose() }
    Read-SearchProperties "$parent/nested/nested-content.txt" 'Regular file'
    Click 'Calculate SHA-256'
    $digest = Wait-Until 'SHA-256 from search result Properties' { Find-Element 'SHA-256 digest' }
    if ($digest.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1).Trim() -cne $expectedDigest) {
        throw 'Properties hashed the wrong search result'
    }
    Click 'Use for verified download'
    $field = Wait-Until 'prefilled digest' { Find-Element 'Expected SHA-256' }
    if ($field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -cne $expectedDigest) {
        throw 'Verified download did not reuse the search result digest'
    }
    Click 'Choose destination'
    Set-DownloadDestination $destination
    $null = Wait-Until 'verified file downloaded with search results still shown' {
        [IO.File]::Exists($destination) -and
            [IO.File]::ReadAllText($destination) -ceq $contents -and
            (Button "Reveal $parent/nested/nested-content.txt") -and (Button $fileResult).Current.IsEnabled
    }
    $null = Wait-Until 'verified search transfer recorded' {
        $events = (Control @('events','--pane',"$remote",'--since','0','--timeout','0')).journal.events
        $events | Where-Object { $_.kind -ceq 'sftp-transfer' -and
            $_.data.outcome -ceq 'completed' -and $_.data.message -clike '*SHA-256 verified*' }
    }
    Click "Download $parent/nested"
    $dialog = Wait-Until 'owned folder picker from the result' {
        Find-OwnedDialog 'Choose a download destination folder'
    }
    Select-DialogItem $dialog 'received'
    $null = Wait-Until 'accept the folder download destination' { [FixtureDialog]::ClickButton($dialog, 1) }
    $folderItem = Join-Path $picker 'received/nested/nested-content.txt'
    $null = Wait-Until 'folder downloaded with search results still shown' {
        [IO.File]::Exists($folderItem) -and
            [IO.File]::ReadAllText($folderItem) -ceq $contents -and
            (Button "Reveal $parent/nested/nested-content.txt").Current.IsEnabled
    }
    Click ('Select downloads' + [char]0x2026)
    (Wait-CheckBox "Mark $parent/nested/nested-content.txt" $off).Toggle()
    (Wait-CheckBox "Mark $parent/nested" $off).Toggle()
    Click "Reveal $parent/nested/nested-content.txt"
    $null = Wait-Until 'search result shown and selected in its containing folder' {
        (Find-Element "$parent/nested") -and (Button 'nested-content.txt') -and (Button $download).Current.IsEnabled
    }
    Click 'Back to search results'
    $null = Wait-FilenameSearchFocus
    $field = Wait-Until 'original search restored with both matches' {
        $edit = Find-Element 'Filename contains' ([System.Windows.Automation.ControlType]::Edit)
        if ($edit -and (Button "Reveal $parent/nested") -and (Button "Reveal $parent/nested/nested-content.txt")) {
            $edit
        }
    }
    if ($field.GetCurrentPattern($value).Current.Value -cne $pattern) {
        throw 'Returning to results changed the search query'
    }
    $null = Wait-CheckBox 'Regex' $on
    $null = Wait-CheckBox 'Match case' $on
    $null = Wait-CheckBox "Mark $parent/nested/nested-content.txt" $on
    $null = Wait-CheckBox "Mark $parent/nested" $on
    Check-PathCopy "Copy path $parent/nested/nested-content.txt" "$parent/nested/nested-content.txt"
    Check-PathCopy 'Copy all paths' "$parent/nested`r`n$parent/nested/nested-content.txt"
    $null = Wait-Until 'result summary and copy confirmation share the panel' {
        (Find-Element '2 matches in 2 folders') -and (Find-Element 'Copied 2 paths')
    }
    [IO.File]::WriteAllText((Join-Path $peerRoot 'shell directory/nested-added.txt'), 'added after search')
    $nestedPath = "$parent/nested"
    [IO.File]::WriteAllText($refreshGate, $nestedPath)
    Click 'Refresh results'
    $progressId = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-search-progress')
    $null = Wait-Until 'scan progress and previous results are visible together' {
        $downloadMarked = Button 'Download marked results'
        $progress = $searchView.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $progressId)
        (Test-Path -LiteralPath (Join-Path $peerRoot 'opendir-paused')) -and
            $progress -and $progress.Current.Name.Contains("Scanning $nestedPath") -and
            $progress.Current.Name.Contains('43 entries scanned') -and
            $progress.Current.Name.Contains('1 folder complete') -and $progress.Current.Name.Contains('2 matches') -and
            (Button "Reveal $parent/nested/nested-content.txt") -and
            $downloadMarked -and !$downloadMarked.Current.IsEnabled
    }
    Click 'Cancel search'
    $null = Wait-Until 'refresh cancellation requested' {
        $cancel = Button 'Cancel search'
        $cancel -and !$cancel.Current.IsEnabled
    }
    [IO.File]::Delete($refreshGate)
    $null = Wait-Until 'cancelled refresh keeps the original results' {
        (Find-Element 'Refresh cancelled. Showing previous results.') -and
            !$searchView.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $progressId) -and
            (Button "Reveal $parent/nested/nested-content.txt").Current.IsEnabled -and
            !(Button "Reveal $parent/nested-added.txt")
    }
    $null = Wait-CheckBox "Mark $parent/nested/nested-content.txt" $on
    Click 'Refresh results'
    $null = Wait-Until 'refresh includes the new unmarked result' {
        (Find-Element '3 matches in 2 folders') -and (Button "Reveal $parent/nested-added.txt")
    }
    $null = Wait-CheckBox "Mark $parent/nested-added.txt" $off
    $null = Wait-CheckBox "Mark $parent/nested/nested-content.txt" $on
    $null = Wait-CheckBox "Mark $parent/nested" $on
    Click "Rename $parent/nested-added.txt"
    $newName = Wait-Until 'rename dialog for the search result' {
        $edit = Find-Element 'New name' ([System.Windows.Automation.ControlType]::Edit)
        if ($edit -and (Find-Element 'Remote rename source')) { $edit }
    }
    if ($newName.GetCurrentPattern($value).Current.Value -cne 'nested-added.txt') {
        throw 'Rename did not prefill the selected search result'
    }
    $newName.GetCurrentPattern($value).SetValue('nested-renamed.txt')
    Click 'Rename'
    $null = Wait-Until 'same search refreshes after rename' {
        (Find-Element 'Renamed nested-added.txt to nested-renamed.txt. 3 matches in 2 folders') -and
            (Button "Reveal $parent/nested-renamed.txt") -and !(Button "Reveal $parent/nested-added.txt")
    }
    $renamedFile = Join-Path $peerRoot 'shell directory/nested-renamed.txt'
    if ([IO.File]::ReadAllText($renamedFile) -cne 'added after search') {
        throw 'Rename changed the bytes of the search result'
    }
    $null = Wait-CheckBox "Mark $parent/nested/nested-content.txt" $on
    $null = Wait-CheckBox "Mark $parent/nested" $on
    $deleteTarget = "$parent/nested-renamed.txt"
    (Wait-CheckBox "Mark $deleteTarget" $off).Toggle()
    $null = Wait-CheckBox "Mark $deleteTarget" $on
    foreach ($confirm in @('Cancel', 'Delete')) {
        Click "Delete $deleteTarget"
        $null = Wait-Until 'exact host and path in the search result deletion confirmation' {
            (Find-Element 'Delete remote file?') -and (Find-Element $deleteTarget) -and
                (Find-Element "$($account.user)@$($account.host):$($account.port)") -and
                (Button 'Delete') -and (Button 'Cancel')
        }
        Click $confirm
        if ($confirm -ceq 'Cancel') {
            $null = Wait-Until 'cancelling deletion preserves the file and its search result' {
                !(Find-Element 'Delete remote file?') -and
                    (Button "Reveal $deleteTarget") -and [IO.File]::Exists($renamedFile)
            }
            $null = Wait-CheckBox "Mark $deleteTarget" $on
        }
    }
    $null = Wait-Until 'same search refreshes after deletion without the deleted row' {
        (Find-Element 'Deleted nested-renamed.txt. 2 matches in 2 folders') -and
            !(Button "Reveal $deleteTarget") -and ![IO.File]::Exists($renamedFile)
    }
    $null = Wait-CheckBox "Mark $parent/nested/nested-content.txt" $on
    $null = Wait-CheckBox "Mark $parent/nested" $on
    $cancelled = Join-Path $picker 'cancelled search batch'
    [IO.File]::WriteAllBytes($readGate, [byte[]]@())
    Save-MarkedResults $cancelled
    $null = Wait-Until 'marked search download stages bytes before publication' {
        (Find-Element '32768 bytes') -and (Button 'Cancel transfer').Current.IsEnabled
    }
    if ([IO.Directory]::Exists($cancelled)) { throw 'Batch published before its transfer finished' }
    Click 'Cancel transfer'
    $null = Wait-Until 'marked search download is cancelling' {
        $cancel = Button 'Cancel transfer'
        $cancel -and !$cancel.Current.IsEnabled
    }
    [IO.File]::Delete($readGate)
    $null = Wait-Until 'cancelled search batch cleans up its own staging directory' {
        !(Button 'Cancel transfer') -and ![IO.Directory]::Exists($cancelled) -and
            [IO.Directory]::GetDirectories($picker, '.w11term-download-*').Count -eq 0
    }
    $null = Wait-CheckBox "Mark $parent/nested" $on
    $batch = Join-Path $picker 'marked search batch'
    Save-MarkedResults $batch
    $batchFile = Join-Path $batch 'nested/nested-content.txt'
    $null = Wait-Until 'marked search download publishes the selected tree' {
        [IO.File]::Exists($batchFile) -and [IO.File]::ReadAllText($batchFile) -ceq $contents -and
            (Button 'Download marked results').Current.IsEnabled
    }
    if ([IO.Directory]::GetFileSystemEntries($batch).Count -ne 1) {
        throw 'Batch downloaded something outside the marked folder'
    }
    Focus-FilenameSearch
    $null = Press-FocusedKey 27
    $null = Wait-Until 'listing remains at the revealed file after leaving the results' {
        (Find-Element "$parent/nested") -and (Button 'nested-content.txt') -and (Button $download).Current.IsEnabled
    }
    $move = 'Move to folder' + [char]0x2026
    $movedFile = Join-Path $peerRoot 'shell directory/nested-content.txt'
    Click $move
    $destination = Wait-Until 'the listed move shows the source and destination editor' {
        $field = Find-Element 'Destination folder' ([System.Windows.Automation.ControlType]::Edit)
        if ($field -and (Find-Element 'Remote move source')) { $field }
    }
    $destination.GetCurrentPattern($value).SetValue($parent)
    Click 'Move'
    $null = Wait-Until 'the source listing refreshes without the moved entry' {
        (Find-Element "Moved nested-content.txt to $parent") -and !(Button 'nested-content.txt') -and
            [IO.File]::Exists($movedFile) -and ![IO.File]::Exists($fixtureText) -and
            (Button 'Up').Current.IsEnabled
    }
    Click ('Go to folder' + [char]0x2026)
    $folderPath = Wait-Until 'the folder navigation dialog accepts a path' {
        Find-Element 'Remote folder path' ([System.Windows.Automation.ControlType]::Edit)
    }
    $folderPath.GetCurrentPattern($value).SetValue($parent)
    Click 'Open'
    $null = Wait-Until 'the destination listing is ready' {
        (Find-Element $parent) -and (Button $filterAction).Current.IsEnabled
    }
    Click $filterAction
    $listingFilter = Wait-Until 'the destination filename filter is available' { Filter-Edit }
    $listingFilter.GetCurrentPattern($value).SetValue('nested-content.txt')
    $null = Wait-Until 'the destination listing contains the moved entry on a later page' { Button 'nested-content.txt' }
    Click 'Clear filter'
    Click ('Find files' + [char]0x2026)
    $field = Wait-FilenameSearchFocus
    $field.GetCurrentPattern($value).SetValue('nested-content.txt')
    Click 'Search'
    Click "Move to folder $parent/nested-content.txt"
    $destination = Wait-Until 'the search result offers its own destination editor' {
        Find-Element 'Destination folder' ([System.Windows.Automation.ControlType]::Edit)
    }
    $destination.GetCurrentPattern($value).SetValue('nested')
    Click 'Move'
    $null = Wait-Until 'the running search follows the entry to its new path' {
        (Find-Element "Moved nested-content.txt to $parent/nested. 1 matches in 2 folders") -and
            (Button "Reveal $parent/nested/nested-content.txt") -and !(Button "Reveal $parent/nested-content.txt") -and
            [IO.File]::Exists($fixtureText) -and ![IO.File]::Exists($movedFile)
    }
    if ([IO.File]::ReadAllText($fixtureText) -cne $contents) {
        throw 'Moving the entry changed its contents'
    }
    Run-Palette 'SFTP files'
    $null = Wait-Until 'drawer closed' { !(Button $filterAction) }
    foreach ($c in 'filter-ssh-ready'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','filter-ssh-ready')
} catch {
    throw "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
} finally {
    [IO.File]::Delete($readGate)
    [IO.File]::Delete($refreshGate)
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
