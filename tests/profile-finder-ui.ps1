[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$original = [IO.File]::ReadAllBytes($profiles)
$catalog = [Text.Encoding]::UTF8.GetString($original) | ConvertFrom-Json
$port = $catalog.profiles.'Fleet North'.port
$newPort = if ($port -eq 65535) { $port - 1 } else { $port + 1 }
$dot = ' ' + [char]0xb7 + ' '
$findProfile = 'Find profile' + [char]0x2026
$findRight = 'Find profile to split right' + [char]0x2026
$findBelow = 'Find profile to split below' + [char]0x2026
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$north = "Fleet North${dot}test@127.0.0.1:$port"

function Open-ProfileFinder {
    $entry = Get-NativeMenuAction 'New tab' $findProfile
    # A live window request invalidates the title bar while the native menu is open.
    $null = Control @('list-saved-ssh-groups','--window',"$windowId")
    Invoke-Button $entry
    Wait-PickerEditor 'Search profiles' 'the New tab profile finder'
}
function Select-Search([string]$Query, [string]$Result, [string]$Editor = 'Search profiles') {
    $field = Wait-PickerEditor $Editor 'the current profile finder'
    $field.GetCurrentPattern($value).SetValue($Query)
    $null = Wait-Until "profile result '$Result'" { Find-Element $Result $button -Visible }
    Press-Key 13
}

$launch = @('--profiles', ('"' + $profiles + '"'), '--demo', '--cols', '110', '--rows', '28')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control manifest' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $initial = (Control @('list')).sessions[0]
    $origin = $initial.id
    $windowId = $initial.window_id
    $field = Open-ProfileFinder
    $null = Wait-Until 'the initial profile list is paged' { Find-Element 'Next profiles' $button }
    $field.GetCurrentPattern($value).SetValue('no matches here')
    $null = Wait-Until 'profile search has an empty state' { Find-Element 'No matching profiles' }
    Press-Key 27
    $null = Wait-Until 'Escape closes the profile finder' { !(Find-Element 'Search profiles' $edit) }
    Wait-Focus $origin
    if (@((Control @('list')).sessions).Count -ne 1) { throw 'Canceling the profile finder created a pane' }

    $field = Open-ProfileFinder
    $field.GetCurrentPattern($value).SetValue('fleet north')
    $null = Wait-Until 'the currently offered SSH endpoint' { Find-Element $north $button -Visible }
    $catalog.profiles.'Fleet North'.port = $newPort
    [IO.File]::WriteAllText($profiles, ($catalog | ConvertTo-Json -Depth 40))
    Press-Key 13
    $null = Wait-Until 'a changed endpoint is displayed before it can be accepted' {
        (Find-Element "Fleet North${dot}test@127.0.0.1:$newPort" $button -Visible) -and
            !(Find-Element $north $button -Visible)
    }
    Wait-Focus $origin
    if (@((Control @('list')).sessions).Count -ne 1) { throw 'A stale profile row opened a pane' }
    [IO.File]::WriteAllBytes($profiles, $original)
    Press-Key 13
    $null = Wait-Until 'the original endpoint is offered again after revalidation' {
        Find-Element $north $button -Visible
    }
    Select-Search "$port NORTH TEST@127.0.0.1" $north
    $remote = Wait-Until 'the matching SSH profile opens in a new pane' {
        ((Control @('list')).sessions | Where-Object { $_.label -ceq 'Fleet North' -and $_.active }).id
    }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')

    Run-Palette $findProfile
    Select-Search 'local PALETTE' "Palette PowerShell${dot}Local"
    $local = Wait-Until 'the selected local profile opens in another pane' {
        ((Control @('list')).sessions | Where-Object { $_.label -ceq 'Palette PowerShell' -and $_.active }).id
    }
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    $ids = @((Control @('list')).sessions.id)
    if ($ids.Count -ne 3 -or $origin -notin $ids -or $remote -notin $ids) {
        throw 'The profile finder changed the previous live panes'
    }

    $tabId = ((Control @('list')).sessions | Where-Object id -eq $local).tab_id
    $full = Wait-Until 'measured unsplit PowerShell' {
        $area = Control @('inspect','--pane',"$local")
        if ($area.pixel_width -gt 0 -and $area.pixel_height -gt 0) { $area }
    }
    Invoke-NativeMenuAction 'Pane layout' $findRight
    $field = Wait-PickerEditor 'Search profiles to split right' 'Pane layout'
    $field.GetCurrentPattern($value).SetValue('Fleet North')
    $null = Wait-Until 'the right split offers the SSH profile' { Find-Element $north $button -Visible }
    Press-Key 27
    $null = Wait-Until 'Escape closes the split finder' { !(Find-Element 'Search profiles to split right' $edit) }
    Wait-Focus $local
    if (@((Control @('list')).sessions).Count -ne $ids.Count) { throw 'Canceling the split created a pane' }

    Invoke-NativeMenuAction 'Pane layout' $findRight
    Select-Search "$port NORTH TEST@127.0.0.1" $north 'Search profiles to split right'
    $right = Wait-Until 'SSH opens on the PowerShell tab' {
        ((Control @('list')).sessions | Where-Object {
            $_.label -ceq 'Fleet North' -and $_.active -and $_.tab_id -eq $tabId
        }).id
    }
    $null = Control @('wait-for-text','--pane',"$right",'--timeout','15','ready')
    $rightArea = Wait-Until 'the right split divides the tab horizontally' {
        $left = Control @('inspect','--pane',"$local")
        $rightSize = Control @('inspect','--pane',"$right")
        if ($left.pixel_width -gt 0 -and $rightSize.pixel_width -gt 0 -and
            $left.pixel_width -lt $full.pixel_width -and $rightSize.pixel_width -lt $full.pixel_width -and
            $left.pixel_height -eq $rightSize.pixel_height) { $rightSize }
    }

    Run-Palette $findBelow
    Select-Search 'local PALETTE' "Palette PowerShell${dot}Local" 'Search profiles to split below'
    $below = Wait-Until 'another PowerShell opens below the SSH pane on the same tab' {
        ((Control @('list')).sessions | Where-Object {
            $_.label -ceq 'Palette PowerShell' -and $_.active -and $_.tab_id -eq $tabId -and $_.id -ne $local
        }).id
    }
    $null = Control @('wait-for-text','--pane',"$below",'--timeout','15','PS ')
    $null = Wait-Until 'the below split divides the right column vertically' {
        $upper = Control @('inspect','--pane',"$right")
        $lower = Control @('inspect','--pane',"$below")
        $upper.pixel_height -gt 0 -and $lower.pixel_height -gt 0 -and
            $upper.pixel_height -lt $rightArea.pixel_height -and $lower.pixel_height -lt $rightArea.pixel_height -and
            $upper.pixel_width -eq $lower.pixel_width
    }
    $ids += @($right, $below)

    Run-Palette 'Settings'
    $filter = Wait-Until 'profile filter in Settings' { Find-Element 'Filter profiles' $edit -Visible }
    $filter.SetFocus()
    $filter.GetCurrentPattern($value).SetValue('no matches here')
    $null = Wait-Until 'Settings reports no matching profiles' {
        (Find-Element 'No profiles match this filter.') -and !(Find-Element 'Edit profile Fleet North' $button)
    }
    $filter.GetCurrentPattern($value).SetValue('local PALETTE')
    $null = Wait-Until 'Settings filters local profiles by name and kind' {
        (Find-Element 'Edit local profile Palette PowerShell' $button) -and
            !(Find-Element 'Edit profile Fleet North' $button)
    }
    $filter.GetCurrentPattern($value).SetValue("$port NORTH TEST@127.0.0.1")
    Invoke-Button (Wait-Until 'only the matching SSH profile can be edited' {
        $editProfile = Find-Element 'Edit profile Fleet North' $button
        if ($editProfile -and !(Find-Element 'Edit local profile Palette PowerShell' $button) -and
            !(Find-Element 'Edit profile Fleet 01' $button)) { $editProfile }
    })
    $null = Wait-Until 'the existing SSH profile opens with its saved values' {
        $name = Find-Element 'Profile name' $edit
        $hostField = Find-Element 'Host' $edit
        $portField = Find-Element 'Port' $edit
        $name -and $hostField -and $portField -and
            $name.GetCurrentPattern($value).Current.Value -ceq 'Fleet North' -and
            $hostField.GetCurrentPattern($value).Current.Value -ceq '127.0.0.1' -and
            $portField.GetCurrentPattern($value).Current.Value -ceq "$port"
    }
    $afterEdit = @((Control @('list')).sessions.id | Sort-Object)
    if (($afterEdit -join ',') -cne (($ids | Sort-Object) -join ',')) {
        throw 'Editing from the filtered list changed the running sessions'
    }
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
