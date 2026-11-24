[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_EXPORT_SMOKE_DIR = Join-Path $State 'exports'
$null = [IO.Directory]::CreateDirectory($env:W11TERM_EXPORT_SMOKE_DIR)
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","close_tab":"F6","find_tab":"F10"}}')
$profiles = Join-Path $State 'profiles.json'
$config = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button
$tabItem = [System.Windows.Automation.ControlType]::TabItem
$edit = [System.Windows.Automation.ControlType]::Edit
$comboBox = [System.Windows.Automation.ControlType]::ComboBox
$listItem = [System.Windows.Automation.ControlType]::ListItem
$selectionItem = [System.Windows.Automation.SelectionItemPattern]::Pattern

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $entry = Find-Element $Name $button
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
}
function Field([string]$Name) {
    Wait-Until "editable '$Name'" {
        $entry = Find-Element $Name $edit
        $provider = $null
        if ($entry -and $entry.TryGetCurrentPattern(
                [System.Windows.Automation.ValuePattern]::Pattern, [ref]$provider)) { $provider }
    }
}
function Expect-Field([string]$Name, [string]$Value) {
    $null = Wait-Until "$Name is '$Value'" {
        ((Field $Name).Current.Value -ceq $Value)
    }
}
function Wait-Tab([string]$Name) {
    try {
        $null = Wait-Until "tab '$Name' is selected" {
            $tab = Find-Element $Name $tabItem
            $tab -and $tab.GetCurrentPattern($selectionItem).Current.IsSelected
        }
    } catch {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $scope = [System.Windows.Automation.TreeScope]::Descendants
        $condition = [System.Windows.Automation.AutomationElement]::ControlTypeProperty
        $tabs = $root.FindAll($scope, [System.Windows.Automation.PropertyCondition]::new($condition, $tabItem))
        $texts = $root.FindAll($scope, [System.Windows.Automation.PropertyCondition]::new($condition, [System.Windows.Automation.ControlType]::Text))
        $names = @($tabs | ForEach-Object { $_.Current.Name })
        $status = @($texts | ForEach-Object { $_.Current.Name } | Where-Object { $_ } | Select-Object -Last 15)
        throw "$_ Tabs: $($names -join ' | '). UI text: $($status -join ' | ')"
    }
}
function Select-Tab([string]$Name) {
    $tab = Wait-Until "tab '$Name'" { Find-Element $Name $tabItem }
    $tab.GetCurrentPattern($selectionItem).Select()
    Wait-Tab $Name
}
function Choose-SavedGroup([string]$Name) {
    $combo = Wait-Until 'saved SSH group chooser' { Find-Element 'Saved SSH groups' $comboBox }
    $popup = $combo.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
    $popup.Expand()
    $item = Wait-Until "'$Name' in the group chooser" {
        $entry = Find-Element $Name $listItem
        if ($entry -and !$entry.Current.IsOffscreen) { $entry }
    }
    $item.GetCurrentPattern($selectionItem).Select()
    $null = Wait-Until "'$Name' is selected in the group chooser" {
        $selection = (Find-Element 'Saved SSH groups' $comboBox).GetCurrentPattern(
            [System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
        $selection.Count -eq 1 -and $selection[0].Current.Name -ceq $Name
    }
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}
function Expect-Choice([string]$FieldName, [string]$Choice) {
    $null = Wait-Until "'$FieldName' selects '$Choice'" {
        $field = Find-Element $FieldName $comboBox
        if ($field) {
            $selection = $field.GetCurrentPattern(
                [System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
            $selection.Count -eq 1 -and $selection[0].Current.Name -ceq $Choice
        }
    }
}
function Choose-Option([string]$FieldName, [string]$Choice) {
    $combo = Wait-Until $FieldName { Find-Element $FieldName $comboBox }
    $popup = $combo.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
    $popup.Expand()
    $item = Wait-Until "visible option '$Choice'" {
        $entry = Find-Element $Choice $listItem
        if ($entry -and !$entry.Current.IsOffscreen) { $entry }
    }
    $item.GetCurrentPattern($selectionItem).Select()
    Expect-Choice $FieldName $Choice
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}
function Session([string]$Name) {
    Wait-Until "SSH pane '$Name'" { (Control @('list')).sessions | Where-Object label -ceq $Name }
}
function Target-Label([string]$HostName, [uint64]$Pane) {
    $port = $config.$HostName.port
    "$HostName${dot}test@127.0.0.1:${port}${dot}Pane $Pane"
}
function Submit([string]$Command) {
    (Field 'Group command').SetValue($Command)
    Click 'Send to 1 host'
    $null = Wait-Until "'$Command' was submitted" { Find-Element "Submitted${dot}$Command" }
}
function Open-GroupCommandSearch([string]$Query) {
    # The fixture can exercise the palette through its own UIA providers even
    # when another window owns the desktop foreground.
    Press-Key 0x76
    (Field 'Search commands').SetValue($Query)
}
function Accept-GroupCommandWithEnter([string]$Command) {
    $null = Wait-Until "enabled group command '$Command'" {
        $item = Find-Element $Command $button
        $item -and $item.Current.IsEnabled
    }
    Press-Key 13
    $null = Wait-Until 'Enter closes the command palette' { !(Find-Element 'Search commands' $edit) }
}
function Choose-GroupCommand([string]$Command) {
    Open-GroupCommandSearch $Command
    $null = Wait-Until "invoke group command '$Command'" {
        try {
            $item = Find-Element $Command $button
            if ($item -and $item.Current.IsEnabled) {
                Invoke-Button $item
                return $true
            }
        } catch {
            if ($_.Exception.GetBaseException() -isnot [System.Windows.Automation.ElementNotAvailableException]) { throw }
        }
    }
    $null = Wait-Until 'the group command closes the palette' { !(Find-Element 'Search commands' $edit) }
}
function Select-TabSearchResult([string]$Query, [string]$Title) {
    (Field 'Search tabs').SetValue($Query)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $kind = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $button)
    $null = Wait-Until "the tab picker finds '$Title'" {
        foreach ($result in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $kind)) {
            if ($result.Current.Name.StartsWith("$Title    Tab ")) { return $true }
        }
    }
    Press-Key 13
    $null = Wait-Until 'Enter closes tab search' { !(Find-Element 'Search tabs' $edit) }
}

$argsForApp = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','110','--rows','34')
$app = Start-Process -FilePath $Gui -ArgumentList $argsForApp -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $origin = (Wait-Until 'the active Demo pane' {
        (Control @('list')).sessions | Where-Object active | Select-Object -First 1
    }).id
    $groupReview = "Review SSH group${dot}Fleet Lab"
    Open-GroupCommandSearch 'review fleet lab group'
    Accept-GroupCommandWithEnter $groupReview
    $null = Wait-Until 'the keyboard opens the first page of a large review' {
        (Find-Element $groupReview) -and (Find-Element "0 to reuse${dot}30 to open") -and
            (Find-Element "30 matches${dot}Page 1 of 2")
    }
    if (Find-Element 'Include profile North') { throw 'A second-page profile was shown on the first page' }
    Click 'Next profiles'
    $null = Wait-Until 'the last review page is available' { Find-Element "30 matches${dot}Page 2 of 2" }
    $null = Wait-CheckBox 'Include profile North' On
    (Field 'Find launch profiles').SetValue('CATALOG-ONLY.TEST')
    $null = Wait-Until 'a filter resets the page and preserves launch choices' {
        (Find-Element "28 matches${dot}Page 1 of 2") -and (Find-Element "0 to reuse${dot}30 to open")
    }
    Click 'Include only matches'
    $null = Wait-Until 'the scope includes matching hosts on both pages' {
        Find-Element "0 to reuse${dot}28 to open${dot}2 skipped"
    }
    $eastQuery = "EAST 127.0.0.1:$($config.East.port)"
    (Field 'Find launch profiles').SetValue($eastQuery)
    $null = Wait-CheckBox 'Include profile East' Off
    Click 'Include only matches'
    $null = Wait-Until 'only the matching SSH address is offered' {
        Find-Element "0 to reuse${dot}1 to open${dot}29 skipped"
    }
    Click 'Refresh review'
    Expect-Field 'Find launch profiles' $eastQuery
    $null = Wait-CheckBox 'Include profile East' On
    (Field 'Find launch profiles').SetValue('NORTH')
    $null = Wait-CheckBox 'Include profile North' Off
    Click 'Include matches'
    $null = Wait-Until 'adding matches preserves the hidden East choice' {
        Find-Element "0 to reuse${dot}2 to open${dot}28 skipped"
    }
    Click 'Skip matches'
    $null = Wait-CheckBox 'Include profile North' Off
    (Field 'Find launch profiles').SetValue('no matching fixture address')
    $null = Wait-Until 'an empty search keeps the earlier choice' {
        $only = Find-Element 'Include only matches' $button
        ($only -and !$only.Current.IsEnabled) -and
            (Find-Element 'No SSH profiles match this search.') -and
            (Find-Element "0 to reuse${dot}1 to open${dot}29 skipped")
    }
    $hostFile = Join-Path $env:W11TERM_EXPORT_SMOKE_DIR 'ssh-hosts.txt'
    $hostList = "test@127.0.0.1:$($config.East.port)`n"
    Click ('Export included hosts' + [char]0x2026)
    $null = Wait-Until 'launch host list preview' { Find-Element 'SSH host list preview' }
    Click ('Save host list' + [char]0x2026)
    $dialog = Wait-Until 'launch host list save dialog' { Find-OwnedDialog 'Save SSH host list' }
    $null = Wait-Until 'save all included hosts even under an empty filter' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'only the selected hidden East address was saved' {
        [IO.File]::Exists($hostFile) -and [IO.File]::ReadAllText($hostFile, [Text.UTF8Encoding]::new($false, $true)) -ceq $hostList
    }
    Click 'Back to launch review'
    Expect-Field 'Find launch profiles' 'no matching fixture address'
    Click ('Save as recipient set' + [char]0x2026)
    $null = Wait-Until 'recipient profile preview' { Find-Element 'Recipient set profiles' }
    (Field 'Launch recipient set name').SetValue('Before connect')
    $beforeSaving = [IO.File]::ReadAllBytes($profiles)
    $changed = [Text.Encoding]::UTF8.GetString($beforeSaving) | ConvertFrom-Json
    $changed.profiles.East.port = 1
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click 'Save recipient set'
    $null = Wait-Until 'an edited profile blocks saving the reviewed set' {
        Find-Element 'SSH profiles changed. Refresh the review before continuing.'
    }
    if ((Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_recipient_sets) {
        throw 'Saving a stale launch set changed the catalog'
    }
    [IO.File]::WriteAllBytes($profiles, $beforeSaving)
    Click 'Save recipient set'
    $null = Wait-Until 'the launch recipient set is saved' { Find-Element 'Saved recipient set Before connect' }
    Expect-Field 'Find launch profiles' 'no matching fixture address'
    Expect-Choice 'Launch recipient set' 'Before connect'
    $sets = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_recipient_sets
    if (($sets.'Fleet Lab'.'Before connect' -join ',') -cne 'East') {
        throw 'The saved set did not include exactly the hidden East profile'
    }
    if (@((Control @('list')).sessions).Count -ne 1) { throw 'Opening the group review started an SSH session' }
    Click 'Cancel'
    Wait-Focus $origin
    if (Find-Element "SSH group${dot}Fleet Lab" $tabItem) { throw 'Cancelling left a draft group tab' }
    $savedLaunch = "Review SSH recipient set${dot}Fleet Lab${dot}Before connect"
    Open-GroupCommandSearch 'review fleet lab before connect'
    Accept-GroupCommandWithEnter $savedLaunch
    $null = Wait-Until 'the new preset reopens with just East selected' {
        Find-Element "0 to reuse${dot}1 to open${dot}29 skipped"
    }
    Expect-Choice 'Launch recipient set' 'Before connect'
    if (@((Control @('list')).sessions).Count -ne 1) { throw 'Reopening the new recipient set started an SSH session' }
    $updateSet = 'Update recipient set' + [char]0x2026
    Click $updateSet
    Expect-Choice 'Existing recipient set' 'Before connect'
    $null = Wait-Until 'an unchanged recipient set needs no save' {
        $save = Find-Element 'Update recipient set' $button
        ($save -and !$save.Current.IsEnabled) -and
            (Find-Element 'The included profiles already match this set.')
    }
    Click 'Back to launch review'
    (Field 'Find launch profiles').SetValue('NORTH')
    $null = Wait-CheckBox 'Include profile North' Off
    Click 'Include only matches'
    $null = Wait-CheckBox 'Include profile North' On
    Click $updateSet
    Choose-Option 'Existing recipient set' 'Before connect'
    $null = Wait-Until 'the update previews both membership changes' {
        (Find-Element 'Add North') -and (Find-Element 'Remove East')
    }
    $beforeUpdate = [IO.File]::ReadAllBytes($profiles)
    $changed = [Text.Encoding]::UTF8.GetString($beforeUpdate) | ConvertFrom-Json
    $changed.ssh_recipient_sets.'Fleet Lab'.'Before connect' = @('Lab 01')
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click 'Update recipient set'
    $null = Wait-Until 'a concurrent update cannot be replaced by the stale preview' {
        Find-Element 'This name is already in use or the recipient set changed. Reopen recipient sets.'
    }
    $sets = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_recipient_sets
    if (($sets.'Fleet Lab'.'Before connect' -join ',') -cne 'Lab 01') { throw 'A stale update replaced the recipient set' }
    [IO.File]::WriteAllBytes($profiles, $beforeUpdate)
    Click 'Update recipient set'
    $null = Wait-Until 'the reviewed change is saved' { Find-Element 'Updated recipient set Before connect' }
    Expect-Choice 'Launch recipient set' 'Before connect'
    Expect-Field 'Find launch profiles' 'NORTH'
    $sets = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_recipient_sets
    if (($sets.'Fleet Lab'.'Before connect' -join ',') -cne 'North' -or @((Control @('list')).sessions).Count -ne 1) {
        throw 'Updating the preset saved the wrong hosts or opened an SSH session'
    }
    $manageSets = 'Manage sets' + [char]0x2026
    Click $manageSets
    Expect-Choice 'Existing recipient set' 'Before connect'
    (Field 'New recipient set name').SetValue('North pilot')
    Click 'Rename recipient set'
    $null = Wait-Until 'the reviewed set is renamed' { Find-Element 'Renamed recipient set Before connect to North pilot' }
    Expect-Choice 'Launch recipient set' 'North pilot'
    Expect-Field 'Find launch profiles' 'NORTH'
    $null = Wait-CheckBox 'Include profile North' On
    $sets = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_recipient_sets
    if ($sets.'Fleet Lab'.'Before connect' -or ($sets.'Fleet Lab'.'North pilot' -join ',') -cne 'North') {
        throw 'Renaming the recipient set altered its members or left the old name'
    }
    Click 'Cancel'
    Wait-Focus $origin
    Open-GroupCommandSearch 'review fleet lab north pilot'
    Accept-GroupCommandWithEnter "Review SSH recipient set${dot}Fleet Lab${dot}North pilot"
    Expect-Choice 'Launch recipient set' 'North pilot'
    (Field 'Find launch profiles').SetValue('NORTH')
    $null = Wait-CheckBox 'Include profile North' On
    Click $manageSets
    Click 'Delete recipient set'
    $sets = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_recipient_sets
    if (!$sets.'Fleet Lab'.'North pilot') { throw 'The set was deleted before confirmation' }
    Click 'Confirm delete recipient set'
    $null = Wait-Until 'the selected set is deleted' { Find-Element 'Deleted recipient set North pilot' }
    $null = Wait-CheckBox 'Include profile North' On
    $sets = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_recipient_sets
    if ($sets.'Fleet Lab'.'North pilot' -or @((Control @('list')).sessions).Count -ne 1) {
        throw 'Deleting the preset left the saved name or opened an SSH session'
    }
    Click 'Cancel'
    Wait-Focus $origin

    $eastTab = "SSH group${dot}Fleet East"
    $westTab = "SSH group${dot}Fleet West"
    Choose-GroupCommand "Open SSH group${dot}Fleet East"
    Wait-Tab $eastTab
    $east = (Session 'East').id
    $eastName = Target-Label 'East' $east
    $null = Wait-CheckBox $eastName On
    $null = Wait-Until 'East is ready' { Find-Element "Pane $east${dot}Connected${dot}Ready" }
    Submit 'east-probe'
    $null = Wait-Until 'East command stays running across the tab switch' { Find-Element "$eastName${dot}Running" }
    (Field 'Group command').SetValue('east-only draft')
    (Field 'Host filter').SetValue('east')
    Expect-Field 'Group command' 'east-only draft'
    Expect-Field 'Host filter' 'east'

    Choose-SavedGroup 'Fleet West'
    Click 'Open in new tab'
    Wait-Tab $westTab
    $west = (Session 'West').id
    $westName = Target-Label 'West' $west
    $null = Wait-CheckBox $westName On
    if (Find-Element "Focus pane $east" $button) { throw 'East leaked into the West roster' }
    Expect-Field 'Group command' ''
    Expect-Field 'Host filter' ''
    $null = Wait-Until 'West is ready' { Find-Element "Pane $west${dot}Connected${dot}Ready" }
    Submit 'west-probe'
    $null = Wait-Until 'only the West submission result is shown' {
        (Find-Element "$westName${dot}Exit 0") -and !(Find-Element "Submitted${dot}east-probe")
    }
    Click ('Recent commands' + [char]0x2026)
    $null = Wait-Until 'West recent commands excludes the East submission' {
        (Find-Element 'west-probe' $button) -and !(Find-Element 'east-probe' $button)
    }
    Press-Key 27
    $null = Wait-Until 'West recent command picker closes' {
        !(Find-Element 'Search recent group commands' $edit)
    }

    $findHost = 'Find SSH host in group' + [char]0x2026
    Choose-GroupCommand $findHost
    (Field 'Find SSH host in group').SetValue('East')
    $null = Wait-Until 'the West host picker excludes the other group' {
        (Find-Element 'No matching SSH hosts in this roster') -and @(Get-PickerRows).Count -eq 0
    }
    Press-Key 27
    $null = Wait-Until 'Escape returns to the West roster' {
        !(Find-Element 'Find SSH host in group' $edit) -and (Find-Element 'Host filter' $edit)
    }
    Wait-Tab $westTab
    (Wait-CheckBox 'Show all window SSH panes' Off).Toggle()
    $null = Wait-Until 'the window-wide roster includes the running East pane' {
        Find-Element "Pane $east${dot}Connected${dot}Waiting for a fresh prompt"
    }
    Choose-Option 'Filter host state' 'Ready'
    Choose-GroupCommand $findHost
    (Field 'Find SSH host in group').SetValue('East')
    $null = Wait-Until 'the Ready filter keeps the running East pane out of the picker' {
        (Find-Element 'No matching SSH hosts in this roster') -and @(Get-PickerRows).Count -eq 0
    }
    [IO.File]::WriteAllText((Join-Path $State 'release-east'), '')
    $null = Control @('wait-for-text','--pane',"$east",'--timeout','15','East finished east-probe')
    $null = Wait-Until 'East appears in the open picker when it becomes ready' {
        $results = @(Get-PickerRows)
        $results.Count -eq 1 -and $results[0].Contains("test@127.0.0.1:$($config.East.port)")
    }
    $address = "test@127.0.0.1:$($config.West.port)"
    (Field 'Find SSH host in group').SetValue($address)
    $null = Wait-Until 'only West matches its SSH address' {
        $results = @(Get-PickerRows)
        $results.Count -eq 1 -and $results[0].Contains($address)
    }
    Press-Key 13
    Wait-Focus $west
    Choose-GroupCommand 'SSH host group'
    Wait-Tab $westTab
    Expect-Choice 'Filter host state' 'Ready'
    Choose-Option 'Filter host state' 'All host states'
    (Wait-CheckBox 'Show all window SSH panes' On).Toggle()

    Click "Focus result pane $west"
    Wait-Focus $west
    Choose-GroupCommand 'SSH host group'
    Wait-Tab $westTab
    (Wait-CheckBox 'Show all window SSH panes' Off).Toggle()
    (Wait-CheckBox $eastName Off).Toggle()
    $null = Wait-CheckBox $eastName On
    (Wait-CheckBox 'Show all window SSH panes' On).Toggle()
    $null = Wait-Until 'the East pane leaves the West scope' { !(Find-Element "Focus pane $east" $button) }
    (Wait-CheckBox 'Show all window SSH panes' Off).Toggle()
    $null = Wait-CheckBox $eastName Off
    $null = Wait-CheckBox $westName On
    (Wait-CheckBox 'Show all window SSH panes' On).Toggle()

    Select-Tab $eastTab
    Expect-Field 'Group command' 'east-only draft'
    Expect-Field 'Host filter' 'east'
    $null = Wait-CheckBox $eastName On
    $null = Wait-Until 'East completed while the tabs remained independent' {
        (Find-Element "Submitted${dot}east-probe") -and (Find-Element "$eastName${dot}Exit 0") -and
            !(Find-Element "Submitted${dot}west-probe")
    }

    Choose-GroupCommand 'New SSH group tab'
    Wait-Tab 'SSH host group'
    $null = Wait-CheckBox $eastName Off
    $null = Wait-CheckBox $westName Off
    Expect-Field 'Group command' ''
    if (@((Control @('list')).sessions).Count -ne 3) { throw 'Changing group tabs started or closed an SSH pane' }
    Press-Key 0x75
    $null = Wait-Until 'the finished window-wide roster closes' { !(Find-Element 'SSH host group' $tabItem) }
    Select-Tab $westTab
    $null = Wait-CheckBox $westName On
    $null = Wait-Until 'West retains its own results and narrow scope' {
        (Find-Element "Submitted${dot}west-probe") -and !(Find-Element "Focus pane $east" $button)
    }

    Select-Tab $eastTab
    (Field 'Host filter').SetValue('')
    $manage = 'Manage live panes' + [char]0x2026
    Click $manage
    $null = Wait-CheckBox "Include $eastName" On
    $null = Wait-CheckBox "Include $westName" Off
    (Field 'Find SSH panes').SetValue("WEST $($config.West.port)")
    $null = Wait-Until 'membership search isolates West' {
        (Find-Element "Include $westName") -and !(Find-Element "Include $eastName")
    }
    (Wait-CheckBox "Include $westName" Off).Toggle()
    $null = Wait-CheckBox "Include $westName" On
    Click 'Cancel'
    $null = Wait-Until 'cancel leaves the committed East roster intact' {
        (Find-Element $manage $button) -and !(Find-Element "Focus pane $west" $button)
    }
    Click $manage
    (Wait-CheckBox "Include $westName" Off).Toggle()
    $null = Wait-CheckBox "Include $westName" On
    Click 'Apply membership'
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName Off
    Expect-Field 'Group command' 'east-only draft'
    $null = Wait-Until 'adding a member preserves the previous submission' { Find-Element "Submitted${dot}east-probe" }
    (Wait-CheckBox $eastName On).Toggle()
    $null = Wait-CheckBox $eastName Off
    (Wait-CheckBox $westName Off).Toggle()
    $null = Wait-CheckBox $westName On
    Submit 'shared-probe'
    $null = Wait-Until 'West receives the intentionally selected command' { Find-Element "$westName${dot}Exit 0" }

    Click $manage
    (Wait-CheckBox "Include $westName" On).Toggle()
    $null = Wait-CheckBox "Include $westName" Off
    Click 'Apply membership'
    $null = Wait-Until 'removal hides West on the named East roster' {
        (Find-Element $manage $button) -and !(Find-Element "Focus pane $west" $button)
    }
    (Wait-CheckBox 'Show all window SSH panes' Off).Toggle()
    $null = Wait-CheckBox $westName Off
    Select-Tab $westTab
    $null = Wait-CheckBox $westName On
    $null = Wait-Until 'the other group retains its selection and own result' {
        (Find-Element "Submitted${dot}west-probe") -and !(Find-Element "Submitted${dot}shared-probe")
    }
    $current = (Session 'West')
    if ($current.id -ne $west -or @((Control @('list')).sessions).Count -ne 3) {
        throw 'Editing membership replaced a running SSH session'
    }

    Choose-SavedGroup 'Fleet Mixed'
    $reuse = 'Review group launch' + [char]0x2026
    Click $reuse
    $null = Wait-Until 'review offers to reuse East and open North' {
        (Find-Element "Review SSH group${dot}Fleet Mixed") -and
            (Find-Element "1 to reuse${dot}1 to open")
    }
    Expect-Choice 'Connection for East' "Reuse${dot}$eastName"
    Expect-Choice 'Connection for North' 'Open a new SSH pane'
    (Field 'Find launch profiles').SetValue('NORTH')
    $null = Wait-Until 'North has no reusable connection and East is hidden' {
        $bulk = Find-Element 'Reuse unique matches' $button
        ($bulk -and !$bulk.Current.IsEnabled) -and
            !(Find-Element 'Connection for East' $comboBox)
    }
    (Field 'Find launch profiles').SetValue('EAST')
    $null = Wait-CheckBox 'Include profile East' On
    Click 'Use new panes for matches'
    $null = Wait-Until 'bulk fresh choice leaves both hosts included' {
        Find-Element "0 to reuse${dot}2 to open"
    }
    Expect-Choice 'Connection for East' 'Open a new SSH pane'
    Click 'Refresh review'
    Expect-Field 'Find launch profiles' 'EAST'
    Expect-Choice 'Connection for East' 'Open a new SSH pane'
    Click 'Reuse unique matches'
    $null = Wait-Until 'the bulk reuse choice restores the unique match' {
        Find-Element "1 to reuse${dot}1 to open"
    }
    Expect-Choice 'Connection for East' "Reuse${dot}$eastName"
    (Field 'Find launch profiles').SetValue('')
    Click 'Clear included hosts'
    $null = Wait-CheckBox 'Include profile East' Off
    (Wait-CheckBox 'Include profile North' Off).Toggle()
    $null = Wait-Until 'the launch can include North alone' {
        Find-Element "0 to reuse${dot}1 to open${dot}1 skipped"
    }
    Click 'Refresh review'
    $null = Wait-CheckBox 'Include profile East' Off
    $null = Wait-CheckBox 'Include profile North' On
    Click 'Clear included hosts'
    $null = Wait-Until 'an empty launch is disabled' {
        $open = Find-Element 'Open reviewed group' $button
        $open -and !$open.Current.IsEnabled
    }
    Click 'Include all available'
    $null = Wait-Until 'both available hosts are included again' { Find-Element "1 to reuse${dot}1 to open" }
    Click 'Cancel'
    $null = Wait-Until 'cancelling the launch review returns to the group' { Find-Element $reuse $button }
    if (@((Control @('list')).sessions).Count -ne 3) { throw 'Cancelling the review opened a session' }

    Click $reuse
    Expect-Choice 'Connection for East' "Reuse${dot}$eastName"
    $original = [IO.File]::ReadAllBytes($profiles)
    $changed = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    $changed.profiles.East.host = 'localhost'
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click 'Open reviewed group'
    $null = Wait-Until 'changed settings require another review' {
        Find-Element 'SSH profiles changed. Refresh the review before continuing.'
    }
    if (@((Control @('list')).sessions).Count -ne 3) { throw 'A stale review opened a session' }
    [IO.File]::WriteAllBytes($profiles, $original)
    Click 'Refresh review'
    $null = Wait-Until 'refresh clears the stale review message' {
        !(Find-Element 'SSH profiles changed. Refresh the review before continuing.')
    }
    Expect-Choice 'Connection for East' "Reuse${dot}$eastName"
    $generation = (Session 'East').connection_generation
    Click 'Open reviewed group'
    Wait-Tab "SSH group${dot}Fleet Mixed"
    $north = (Session 'North').id
    $northName = Target-Label 'North' $north
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $northName On
    $null = Wait-Until 'the new group reports its mixed launch' {
        Find-Element "Reused 1${dot}Opened 1 for Fleet Mixed"
    }
    $eastStill = (Session 'East')
    if ($eastStill.id -ne $east -or $eastStill.connection_generation -ne $generation -or
            @((Control @('list')).sessions).Count -ne 4) {
        throw 'The launch created a second East SSH session'
    }
    (Wait-CheckBox $northName On).Toggle()
    $null = Wait-CheckBox $northName Off
    Submit 'reuse-probe'
    $null = Wait-Until 'the reviewed East session accepts the new group command' {
        Find-Element "$eastName${dot}Exit 0"
    }
    Press-Key 0x75
    $null = Wait-Until 'the finished mixed roster closes' { !(Find-Element "SSH group${dot}Fleet Mixed" $tabItem) }
    Select-Tab $westTab
    $null = Wait-Until 'the source group retains its earlier result' { Find-Element "Submitted${dot}west-probe" }

    Select-Tab $eastTab
    Choose-SavedGroup 'Fleet Mixed'
    Click $manage
    (Wait-CheckBox "Include $westName" Off).Toggle()
    $null = Wait-CheckBox "Include $westName" On
    Click 'Apply membership'
    $null = Wait-CheckBox $westName Off
    (Field 'Group command').SetValue('retained roster draft')
    (Field 'Host filter').SetValue('east')
    Expect-Field 'Host filter' 'east'
    $update = 'Update saved group' + [char]0x2026
    $original = [IO.File]::ReadAllBytes($profiles)
    Click $update
    $null = Wait-Until 'the roster review targets its source group and includes unchecked, hidden West' {
        (Find-Element "Update saved SSH group${dot}Fleet East") -and (Find-Element 'Add profile West')
    }
    Click 'Cancel'
    $null = Wait-Until 'cancelling the roster review returns to the tab' { Find-Element $update $button }
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($profiles)) -cne
            [Convert]::ToBase64String($original)) { throw 'Cancelling the roster review edited the catalog' }

    Click $update
    $null = Wait-Until 'West is available at the start of the review' { Find-Element 'Add profile West' }
    $changed = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    $changed.profiles.PSObject.Properties.Remove('West')
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click 'Save roster changes'
    $null = Wait-Until 'the save refreshes the preview when a roster profile disappears' {
        (Find-Element 'Live panes changed. Review the new membership before saving.') -and
            (Find-Element 'Panes without an available saved SSH profile: 1') -and
            !(Find-Element 'Add profile West')
    }
    [IO.File]::WriteAllBytes($profiles, $original)
    Click 'Cancel'
    $null = Wait-Until 'the updated preview closes' { Find-Element $update $button }

    Click $update
    $null = Wait-Until 'the roster review proposes West' { Find-Element 'Add profile West' }
    $changed = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    $changed.ssh_host_groups.'Fleet East' = @('East', 'North')
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click 'Save roster changes'
    $null = Wait-Until 'an external roster edit rejects the reviewed save' {
        Find-Element 'The SSH group changed on disk. Reopen it before saving.'
    }
    $groups = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_host_groups
    if (($groups.'Fleet East' -join ',') -cne 'East,North') { throw 'A stale review replaced the saved group' }
    [IO.File]::WriteAllBytes($profiles, $original)
    Click 'Save roster changes'
    $null = Wait-Until 'the confirmed roster is saved' { Find-Element 'Saved live membership for Fleet East' }
    $groups = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_host_groups
    if (($groups.'Fleet East' -join ',') -cne 'East,West' -or
            ($groups.'Fleet Mixed' -join ',') -cne 'East,North' -or
            ($groups.'Fleet West' -join ',') -cne 'West') { throw 'The wrong saved group was updated' }
    Expect-Field 'Group command' 'retained roster draft'
    Expect-Field 'Host filter' 'east'
    $null = Wait-Until 'the last command remains in the group' { Find-Element "Submitted${dot}shared-probe" }

    $setsButton = 'Recipient sets' + [char]0x2026
    (Wait-CheckBox $eastName Off).Toggle()
    $null = Wait-CheckBox $eastName On
    Click $setsButton
    $null = Wait-Until 'recipient sets belong to the tab source group' {
        (Find-Element "Recipient sets${dot}Fleet East") -and (Find-Element "Current selection${dot}East")
    }
    (Field 'Recipient set name').SetValue('Canaries')
    Click 'Save recipient set'
    $null = Wait-Until 'the canary set is saved' { Find-Element 'Saved recipient set Canaries' }
    Click 'Back to SSH group'
    (Field 'Host filter').SetValue('')
    (Wait-CheckBox $westName Off).Toggle()
    $null = Wait-CheckBox $westName On
    Click $setsButton
    (Field 'Recipient set name').SetValue('Fleet')
    Click 'Save recipient set'
    $null = Wait-Until 'the fleet set is saved' { Find-Element 'Saved recipient set Fleet' }
    $sets = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_recipient_sets.'Fleet East'
    if (($sets.Canaries -join ',') -cne 'East' -or ($sets.Fleet -join ',') -cne 'East,West') {
        throw 'Saved sets do not match the reviewed profile selections'
    }
    Click 'Back to SSH group'

    $null = Wait-CheckBox 'Show all window SSH panes' On
    Select-Tab $westTab
    Open-GroupCommandSearch 'apply recipient canaries'
    $null = Wait-Until 'the West group does not offer the East group canaries' {
        Find-Element 'No matching commands'
    }
    Press-Key 27
    $null = Wait-Until 'Esc closes the command palette' { !(Find-Element 'Search commands' $edit) }
    Select-Tab $eastTab
    (Field 'Host filter').SetValue('west')
    Expect-Field 'Host filter' 'west'
    Open-GroupCommandSearch 'apply canaries recipient'
    $applyCanaries = "Apply recipient set${dot}Canaries"
    $null = Wait-Until 'the palette has the reviewed Canaries set' { Find-Element $applyCanaries $button }
    $beforePresetEdit = [IO.File]::ReadAllBytes($profiles)
    $changed = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    $changed.ssh_recipient_sets.'Fleet East'.Canaries = @('West')
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Accept-GroupCommandWithEnter $applyCanaries
    $null = Wait-Until 'a changed preset cannot be applied from an old palette row' {
        Find-Element 'The recipient set changed on disk. Open it again before applying.'
    }
    Expect-Field 'Host filter' 'west'
    $null = Wait-CheckBox $westName On
    [IO.File]::WriteAllBytes($profiles, $beforePresetEdit)
    Open-GroupCommandSearch 'apply CANARIES recipient'
    Accept-GroupCommandWithEnter $applyCanaries
    $null = Wait-CheckBox 'Show all window SSH panes' Off
    Expect-Field 'Host filter' ''
    Expect-Field 'Group command' 'retained roster draft'
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName Off
    Open-GroupCommandSearch "Apply recipient set${dot}Fleet"
    Accept-GroupCommandWithEnter "Apply recipient set${dot}Fleet"
    $null = Wait-CheckBox $eastName On
    $null = Wait-CheckBox $westName On

    Select-Tab $westTab
    Choose-SavedGroup 'Fleet East'
    Click $reuse
    Choose-Option 'Connection for West' 'Open a new SSH pane'
    Choose-Option 'Launch recipient set' 'Canaries'
    $null = Wait-CheckBox 'Include profile East' On
    $null = Wait-CheckBox 'Include profile West' Off
    $null = Wait-Until 'only the canary connection is offered' {
        Find-Element "1 to reuse${dot}0 to open${dot}1 skipped"
    }
    $beforePresetLaunch = [IO.File]::ReadAllBytes($profiles)
    $changed = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    $changed.ssh_recipient_sets.'Fleet East'.Canaries = @('West')
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click 'Open reviewed group'
    $null = Wait-Until 'stale launch preset requires another review' {
        Find-Element 'Recipient set Canaries changed. Refresh the review before continuing.'
    }
    if (@((Control @('list')).sessions).Count -ne 4) { throw 'A stale recipient set opened a new SSH pane' }
    [IO.File]::WriteAllBytes($profiles, $beforePresetLaunch)
    Click 'Refresh review'
    $null = Wait-Until 'refreshed preset is ready' {
        !(Find-Element 'Recipient set Canaries changed. Refresh the review before continuing.')
    }
    Expect-Choice 'Launch recipient set' 'Canaries'
    Click 'Open reviewed group'
    Wait-Tab "SSH group${dot}Fleet East (2)"
    $null = Wait-CheckBox $eastName On
    $null = Wait-Until 'only the canary is in the new roster' {
        (Find-Element "Reused 1${dot}Opened 0 for Fleet East${dot}Skipped 1") -and
            !(Find-Element "Focus pane $west" $button) -and !(Find-Element $update $button)
    }
    if (@((Control @('list')).sessions).Count -ne 4 -or
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($profiles)) -cne
            [Convert]::ToBase64String($beforePresetLaunch)) {
        throw 'The canary launch created another connection or edited the saved group'
    }
    Select-Tab $westTab
    Click "Focus pane $west"
    Wait-Focus $west
    $canaryReview = "Review SSH recipient set${dot}Fleet East${dot}Canaries"
    Open-GroupCommandSearch 'review canaries fleet east'
    $null = Wait-Until 'the terminal command palette offers the saved set' { Find-Element $canaryReview $button }
    $changed = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    $changed.ssh_recipient_sets.'Fleet East'.Canaries = @('West')
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Accept-GroupCommandWithEnter $canaryReview
    $staleError = 'Recipient set Canaries changed. Open the command palette again.  ' + [char]0xb7 + '  Dismiss'
    Click $staleError
    Wait-Focus $west
    $keyboardCanaryTab = "SSH group${dot}Fleet East (3)"
    if ((Find-Element $keyboardCanaryTab $tabItem) -or @((Control @('list')).sessions).Count -ne 4) {
        throw 'A stale palette offer opened a review or a connection'
    }

    [IO.File]::WriteAllBytes($profiles, $beforePresetLaunch)
    Open-GroupCommandSearch 'review canaries fleet east'
    Accept-GroupCommandWithEnter $canaryReview
    Wait-Tab $keyboardCanaryTab
    Expect-Choice 'Launch recipient set' 'Canaries'
    $null = Wait-CheckBox 'Include profile East' On
    $null = Wait-CheckBox 'Include profile West' Off
    $null = Wait-Until 'keyboard review offers the existing East connection only' {
        Find-Element "1 to reuse${dot}0 to open${dot}1 skipped"
    }
    Click 'Open reviewed group'
    Wait-Tab $keyboardCanaryTab
    $null = Wait-CheckBox $eastName On
    $null = Wait-Until 'the keyboard review became the canary roster' {
        (Find-Element "Reused 1${dot}Opened 0 for Fleet East${dot}Skipped 1") -and
            !(Find-Element "Focus pane $west" $button)
    }
    if ((Find-Element "SSH group${dot}Fleet East (4)" $tabItem) -or
            @((Control @('list')).sessions).Count -ne 4) {
        throw 'Approving the keyboard review created an extra tab or SSH connection'
    }

    Choose-GroupCommand 'New SSH group tab'
    $null = Wait-Until 'the window-wide roster replaces the canary report' {
        !(Find-Element "Reused 1${dot}Opened 0 for Fleet East${dot}Skipped 1") -and (Find-Element 'Group command' $edit)
    }
    (Field 'Group command').SetValue('first roster draft')
    Expect-Field 'Group command' 'first roster draft'
    Choose-GroupCommand 'New SSH group tab'
    Expect-Field 'Group command' ''
    (Field 'Group command').SetValue('overflow roster draft')
    Expect-Field 'Group command' 'overflow roster draft'
    Press-Key 0x79
    Select-TabSearchResult 'WEST' 'West'
    Wait-Focus $west
    Press-Key 0x79
    (Field 'Search tabs').SetValue('a tab that does not exist')
    $null = Wait-Until 'the tab search reports an empty result' { Find-Element 'No matching tabs' }
    Press-Key 27
    $null = Wait-Until 'Escape closes tab search' { !(Find-Element 'Search tabs' $edit) }
    Wait-Focus $west

    Choose-GroupCommand ("Find tab$([char]0x2026)    F10")
    Select-TabSearchResult 'TAB 10' 'SSH host group (2)'
    Expect-Field 'Group command' 'overflow roster draft'
    if (@((Control @('list')).sessions).Count -ne 4) { throw 'Navigating the tab picker changed the SSH sessions' }
    Press-Key 0x79
    Select-TabSearchResult $eastTab $eastTab
    Expect-Field 'Group command' 'retained roster draft'
    Click $manage
    (Wait-CheckBox "Include $westName" On).Toggle()
    $null = Wait-CheckBox "Include $westName" Off
    Click 'Apply membership'
    Choose-GroupCommand ('Manage recipient sets' + [char]0x2026)
    Click 'Apply recipient set Fleet'
    $null = Wait-Until 'a missing roster member blocks the whole preset' {
        Find-Element 'West has no matching live pane in this roster.'
    }
    Click 'Back to SSH group'
    $null = Wait-CheckBox $eastName On
    Click $update
    $null = Wait-Until 'a second review uses the just-saved roster' { Find-Element 'Remove profile West' }
    Click 'Save roster changes'
    $null = Wait-Until 'the second update is saved' { Find-Element 'Saved live membership for Fleet East' }
    $groups = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_host_groups
    if (($groups.'Fleet East' -join ',') -cne 'East') { throw 'The second roster change did not persist' }
    Click $setsButton
    Click 'Delete recipient set Fleet'
    Click 'Confirm delete recipient set Fleet'
    $null = Wait-Until 'the invalidated set can be removed' { Find-Element 'Deleted recipient set Fleet' }
    $sets = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_recipient_sets.'Fleet East'
    if ($sets.Fleet -or ($sets.Canaries -join ',') -cne 'East') { throw 'Deleting a set edited another set' }
    Click 'Back to SSH group'
    if (@((Control @('list')).sessions).Count -ne 4) { throw 'Roster and recipient edits changed the SSH sessions' }

    $beforeAdd = @((Control @('list')).sessions)
    (Field 'Host filter').SetValue('east')
    Expect-Field 'Host filter' 'east'
    $addProfiles = 'Add saved profiles' + [char]0x2026
    Click $addProfiles
    (Field 'Find saved SSH profiles').SetValue('WEST')
    $null = Wait-CheckBox 'Add SSH profile West' Off
    Click 'Check matches'
    $null = Wait-CheckBox 'Add SSH profile West' On
    (Field 'Find saved SSH profiles').SetValue('NORTH')
    (Wait-CheckBox 'Add SSH profile North' Off).Toggle()
    $null = Wait-CheckBox 'Add SSH profile North' On
    Click 'Review selected profiles'
    $westLabel = "West${dot}test@127.0.0.1:$($config.West.port)"
    Choose-Option "Connection for $westLabel" "Reuse${dot}$westName"
    $beforeStale = [IO.File]::ReadAllBytes($profiles)
    $changed = [Text.Encoding]::UTF8.GetString($beforeStale) | ConvertFrom-Json
    $changed.profiles.North.port = 1
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click 'Use 2 checked hosts'
    $null = Wait-Until 'a changed saved profile returns to the picker' {
        Find-Element 'SSH profile North changed. Refresh saved profiles and review again.'
    }
    if (@((Control @('list')).sessions).Count -ne $beforeAdd.Count) { throw 'A stale profile review opened a pane' }
    [IO.File]::WriteAllBytes($profiles, $beforeStale)
    Click 'Refresh saved profiles'
    Expect-Field 'Find saved SSH profiles' 'NORTH'
    $null = Wait-CheckBox 'Add SSH profile North' On
    Click 'Review selected profiles'
    Choose-Option "Connection for $westLabel" "Reuse${dot}$westName"
    Click 'Use 2 checked hosts'
    $null = Wait-Until 'the selected profiles are added to the named roster' {
        Find-Element "Reused 1${dot}Opened 1 from saved profiles"
    }
    Expect-Field 'Group command' 'retained roster draft'
    Expect-Field 'Host filter' 'east'
    $null = Wait-CheckBox $eastName On
    $newNorth = Wait-Until 'exactly one fresh North pane is opened' {
        $current = @((Control @('list')).sessions)
        if ($current.Count -eq ($beforeAdd.Count + 1)) {
            $current | Where-Object { $_.label -ceq 'North' -and $_.id -notin $beforeAdd.id }
        }
    }
    (Field 'Host filter').SetValue('')
    $null = Wait-CheckBox $westName Off
    $null = Wait-CheckBox (Target-Label 'North' $newNorth.id) Off
    if (((Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).ssh_host_groups.'Fleet East' -join ',') -cne 'East') {
        throw 'Adding profiles changed the saved group membership'
    }
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
