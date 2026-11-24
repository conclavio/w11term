[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
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
    $null = Wait-Until "tab '$Name' is selected" {
        $tab = Find-Element $Name $tabItem
        $tab -and $tab.GetCurrentPattern($selectionItem).Current.IsSelected
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

$argsForApp = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','110','--rows','34')
$app = Start-Process -FilePath $Gui -ArgumentList $argsForApp -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
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

    [IO.File]::WriteAllText((Join-Path $State 'release-east'), '')
    $null = Control @('wait-for-text','--pane',"$east",'--timeout','15','East finished east-probe')
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
    $reuse = 'Review connection reuse' + [char]0x2026
    Click $reuse
    $null = Wait-Until 'review offers to reuse East and open North' {
        (Find-Element "Review SSH group${dot}Fleet Mixed") -and
            (Find-Element "1 to reuse${dot}1 to open")
    }
    Expect-Choice 'Connection for East' "Reuse${dot}$eastName"
    Expect-Choice 'Connection for North' 'Open a new SSH pane'
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
        Find-Element 'SSH profiles changed. Refresh the review before opening this group.'
    }
    if (@((Control @('list')).sessions).Count -ne 3) { throw 'A stale review opened a session' }
    [IO.File]::WriteAllBytes($profiles, $original)
    Click 'Refresh review'
    $null = Wait-Until 'refresh clears the stale review message' {
        !(Find-Element 'SSH profiles changed. Refresh the review before opening this group.')
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
    Open-GroupCommandSearch 'recipient canaries'
    $null = Wait-Until 'the West group does not offer the East group canaries' {
        Find-Element 'No matching commands'
    }
    Press-Key 27
    $null = Wait-Until 'Esc closes the command palette' { !(Find-Element 'Search commands' $edit) }
    Select-Tab $eastTab
    (Field 'Host filter').SetValue('west')
    Expect-Field 'Host filter' 'west'
    Open-GroupCommandSearch 'canaries recipient'
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
    Open-GroupCommandSearch 'CANARIES recipient'
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
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
