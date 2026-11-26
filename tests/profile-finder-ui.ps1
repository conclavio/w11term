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
function Select-Search([string]$Query, [string]$Result) {
    $field = Wait-PickerEditor 'Search profiles' 'the current profile finder'
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
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
