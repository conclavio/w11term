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
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    Run-Palette "Open SSH group${dot}Fleet" -Enter
    $origin = Wait-Until 'the origin group starts two shells' {
        $current = @((Control @('list')).sessions)
        if ($current.Count -eq 3 -and @($current.window_id | Sort-Object -Unique).Count -eq 1) { ,$current }
    }
    $originId = $origin[0].window_id
    foreach ($pane in ($origin | Where-Object label -in 'East','West')) {
        $null = Control @('wait-for-text','--pane',"$($pane.id)",'--timeout','15','ready')
    }
    Invoke-Button (Wait-Until 'the saved group window action' {
        Find-Element 'Open group in new window' ([System.Windows.Automation.ControlType]::Button)
    })
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
