[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$portByName = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles
$dot = ' ' + [char]0xb7 + ' '
$button = [System.Windows.Automation.ControlType]::Button

function Click-TestButton([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Close-TestWindow([IntPtr]$Hwnd) {
    if ([FixtureDialog]::Windows($app.Id) -notcontains $Hwnd -or
        [FixtureDialog]::GetWindow($Hwnd, 4) -ne [IntPtr]::Zero) {
        throw "Refusing to close a window outside the fixture"
    }
    if (![FixtureMessages]::PostMessage($Hwnd, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero)) {
        throw "Failed to close the fixture window"
    }
}

$launch = @('--profiles',('"' + $profiles + '"'),'--demo','--cols','95','--rows','32')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $originHwnd = $window
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    Run-Palette "Open SSH group${dot}Fleet" -Enter
    $origin = Wait-Until 'the origin group starts two shells' {
        $current = @((Control @('list')).sessions)
        if ($current.Count -eq 3 -and @($current.window_id | Sort-Object -Unique).Count -eq 1) { ,$current }
    }
    $originId = $origin[0].window_id
    foreach ($pane in ($origin | Where-Object label -in 'East','West')) {
        $null = Control @('wait-for-text','--pane',"$($pane.id)",'--timeout','15','ready')
    }
    Invoke-NativeMenuAction 'Saved group actions' 'Open group in new window'
    $childHwnd = Wait-Until 'a second top-level window in the same process' {
        [FixtureDialog]::Windows($app.Id) | Where-Object {
            $_ -ne $originHwnd -and [FixtureDialog]::GetWindow($_, 4) -eq [IntPtr]::Zero
        } | Select-Object -First 1
    }
    $child = Wait-Until 'the two new panes have their own window ID' {
        $all = @((Control @('list')).sessions)
        $children = @($all | Where-Object window_id -ne $originId)
        if ($all.Count -eq 5 -and $children.Count -eq 2 -and
            @($children.window_id | Sort-Object -Unique).Count -eq 1) { ,$children }
    }
    if (($child.label | Sort-Object) -join ',' -cne 'East,West') { throw 'The child opened unexpected profiles' }
    $window = $childHwnd
    foreach ($pane in $child) {
        $port = $portByName.($pane.label).port
        $null = Wait-CheckBox "$($pane.label)${dot}test@127.0.0.1:${port}${dot}Pane $($pane.id)" On
        $null = Control @('wait-for-text','--pane',"$($pane.id)",'--timeout','15','ready')
    }
    $null = Wait-Until 'the child shows its saved readiness setting when collapsed' {
        Find-Element 'All recipients must be ready' -Visible
    }
    Set-NativeExpansion 'Dispatch settings' $true
    $null = Wait-Until 'the child roster uses the saved readiness default' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($childHwnd)
        $name = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::NameProperty, 'Require all recipients ready')
        $check = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $name)
        $pattern = $null
        $check -and $check.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$pattern) -and
            $pattern.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On
    }
    Close-TestWindow $childHwnd
    $window = $originHwnd
    $null = Wait-Until 'only the original pane IDs remain' {
        $after = @((Control @('list')).sessions)
        ($after.id | Sort-Object) -join ',' -ceq (($origin.id | Sort-Object) -join ',') -and
            @($after | Where-Object window_id -ne $originId).Count -eq 0
    }
    $east = ($origin | Where-Object label -ceq 'East').id
    $null = Control @('send-text','--pane',"$east",'--no-paste','origin-still-live')
    $null = Control @('wait-for-text','--pane',"$east",'--timeout','15','origin-still-live')

    Run-Palette "Review SSH recipient set${dot}Fleet${dot}Canary" -Enter
    $openSubset = 'Open reviewed group in new window'
    $null = Wait-Until 'the review keeps the live connection choice' {
        $item = Find-Element $openSubset $button
        $item -and !$item.Current.IsEnabled -and (Find-Element "1 to reuse${dot}0 to open${dot}1 skipped")
    }
    Click-TestButton 'Use new panes for matches'
    $null = Wait-Until 'the Canary review is ready to open one fresh pane' {
        Find-Element "0 to reuse${dot}1 to open${dot}1 skipped"
    }
    $before = [IO.File]::ReadAllBytes($profiles)
    $changed = [Text.Encoding]::UTF8.GetString($before) | ConvertFrom-Json
    $changed.ssh_recipient_sets.Fleet.Canary = @('West')
    [IO.File]::WriteAllText($profiles, ($changed | ConvertTo-Json -Depth 30))
    Click-TestButton $openSubset
    $null = Wait-Until 'a stale recipient set is refused before opening a window' {
        Find-Element 'Recipient set Canary changed. Refresh the review before continuing.'
    }
    if (@((Control @('list')).sessions).Count -ne 3) { throw 'The stale review opened a session' }
    [IO.File]::WriteAllBytes($profiles, $before)
    Click-TestButton $openSubset
    $subset = Wait-Until 'only the included profile opens in a separate window' {
        $all = @((Control @('list')).sessions)
        $extra = @($all | Where-Object window_id -ne $originId)
        if ($all.Count -eq 4 -and $extra.Count -eq 1 -and $extra[0].label -ceq 'East') { $extra[0] }
    }
    $null = Control @('wait-for-text','--pane',"$($subset.id)",'--timeout','15','ready')
    $subsetHwnd = Wait-Until 'the reviewed window has its own native handle' {
        [FixtureDialog]::Windows($app.Id) | Where-Object {
            $_ -ne $originHwnd -and [FixtureDialog]::GetWindow($_, 4) -eq [IntPtr]::Zero
        } | Select-Object -First 1
    }
    $window = $subsetHwnd
    $null = Wait-CheckBox "East${dot}test@127.0.0.1:$($portByName.East.port)${dot}Pane $($subset.id)" On
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($subsetHwnd)
    $update = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, ('Update saved group' + [char]0x2026))
    if ($root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $update)) {
        throw 'The partial roster offered to replace saved group membership'
    }
    Close-TestWindow $subsetHwnd
    $window = $originHwnd
    $null = Wait-Until 'closing the reviewed window restores the unchanged source roster' {
        (Find-Element 'Saved group actions' $button) -and
            !(Find-Element 'Find launch profiles' ([System.Windows.Automation.ControlType]::Edit)) -and
            (@((Control @('list')).sessions).id | Sort-Object) -join ',' -ceq (($origin.id | Sort-Object) -join ',')
    }
} catch {
    throw ($_.Exception.Message + [Environment]::NewLine + $_.ScriptStackTrace)
} finally {
    if (!$app.HasExited) {
        foreach ($hwnd in [FixtureDialog]::Windows($app.Id)) {
            if ([FixtureDialog]::GetWindow($hwnd, 4) -eq [IntPtr]::Zero) { Close-TestWindow $hwnd }
        }
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
