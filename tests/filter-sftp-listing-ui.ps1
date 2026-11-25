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
$readGate = Join-Path ([IO.File]::ReadAllText((Join-Path $State 'fixture-root'))) 'hold-transfer-read'
$profiles = Join-Path $State 'profiles.json'
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
    $field = Wait-Until 'recursive filename search field' {
        Find-Element 'Filename contains' ([System.Windows.Automation.ControlType]::Edit)
    }
    $on = [System.Windows.Automation.ToggleState]::On
    $off = [System.Windows.Automation.ToggleState]::Off
    (Wait-CheckBox 'Regex' $off).Toggle()
    (Wait-CheckBox 'Match case' $off).Toggle()
    $null = Wait-CheckBox 'Regex' $on
    $null = Wait-CheckBox 'Match case' $on
    $value = [System.Windows.Automation.ValuePattern]::Pattern
    $field = Find-Element 'Filename contains' ([System.Windows.Automation.ControlType]::Edit)
    $field.GetCurrentPattern($value).SetValue('(')
    Click 'Search'
    $searchView = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $searchStatus = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-search-status')
    $null = Wait-Until 'invalid filename regex reported' {
        $status = $searchView.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $searchStatus)
        $status -and $status.Current.Name.Contains('Invalid filename regex')
    }
    $pattern = '^nested(?:-content\.txt)?$|^BUILD-00\.log$'
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
    Click "Reveal $parent/nested/nested-content.txt"
    $null = Wait-Until 'search result shown and selected in its containing folder' {
        (Find-Element "$parent/nested") -and (Button 'nested-content.txt') -and (Button $download).Current.IsEnabled
    }
    Click 'Back to search results'
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
    Check-PathCopy "Copy path $parent/nested/nested-content.txt" "$parent/nested/nested-content.txt"
    Check-PathCopy 'Copy all paths' "$parent/nested`r`n$parent/nested/nested-content.txt"
    $null = Wait-Until 'result summary and copy confirmation share the panel' {
        (Find-Element '2 matches in 2 folders') -and (Find-Element 'Copied 2 paths')
    }
    Click 'Back to files'
    $null = Wait-Until 'listing remains at the revealed file after leaving the results' {
        (Find-Element "$parent/nested") -and (Button 'nested-content.txt') -and (Button $download).Current.IsEnabled
    }
    Run-Palette 'SFTP files'
    $null = Wait-Until 'drawer closed' { !(Button $filterAction) }
    foreach ($c in 'filter-ssh-ready'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','filter-ssh-ready')
} finally {
    [IO.File]::Delete($readGate)
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
