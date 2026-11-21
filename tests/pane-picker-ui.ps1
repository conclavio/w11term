# Invoked by the native SFTP/SSH integration suite with two live loopback shells.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class PickerMessages {
    [DllImport("user32.dll", SetLastError=true)] public static extern bool PostMessage(
        IntPtr hwnd, uint msg, UIntPtr key, IntPtr detail);
}
'@

$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--workspace', 'picker') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')

function Wait-Until([string]$Description, [scriptblock]$Probe) {
    $until = [DateTime]::UtcNow.AddSeconds(15)
    do {
        if ($app.HasExited) { throw "w11term exited while waiting for $Description" }
        $answer = & $Probe
        if ($answer) { return $answer }
        Start-Sleep -Milliseconds 80
    } while ([DateTime]::UtcNow -lt $until)
    throw "Timed out waiting for $Description"
}
function Control([string[]]$Command) {
    $reply = & $Cli --pid $app.Id @Command | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0 -or -not $reply.ok) { throw "Control request failed: $Command $($reply.error)" }
    return $reply.result
}
function Find-Element([string]$Name) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $found = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    if ($found) { return $found }
    # WinUI may expose a flyout as a sibling of the main HWND.
    $processCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $app.Id)
    $popups = [System.Windows.Automation.AutomationElement]::RootElement.FindAll(
        [System.Windows.Automation.TreeScope]::Children, $processCondition)
    foreach ($popup in $popups) {
        $found = $popup.FindFirst([System.Windows.Automation.TreeScope]::Subtree, $condition)
        if ($found) { return $found }
    }
}
function Invoke-Button($Element) {
    $Element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
}
function Picker-Query([string]$Query) {
    Write-Output "Searching for $Query"
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    Invoke-Button (Wait-Until 'Find pane menu item' { Find-Element ('Find pane' + [char]0x2026) })
    $edit = Wait-Until 'keyboard focus in the pane search field' {
        $field = Find-Element 'Find pane'
        if ($field -and $field.Current.HasKeyboardFocus) { return $field }
    }
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Query)
    $null = Wait-Until "a single rendered result matching '$Query'" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $buttons = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Button)
        $rows = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $buttons) |
            ForEach-Object { $_.Current.Name } | Where-Object { $_ -match 'Tab [0-9]+ / pane [0-9]+' })
        $rows.Count -eq 1 -and $rows[0].IndexOf($Query, [StringComparison]::OrdinalIgnoreCase) -ge 0
    }
}
function Post-Key([uint32]$Message, [uint32]$Key) {
    # Address this process's window. This never types into the foreground application.
    if (-not [PickerMessages]::PostMessage($window, $Message, [UIntPtr]::new($Key), [IntPtr]::Zero)) {
        throw "Could not post key event to the fixture window"
    }
}
function Press-Key([uint32]$Key) {
    Post-Key 0x100 $Key
    Post-Key 0x101 $Key
}
function Wait-Focus([int]$Pane) {
    try {
        $null = Wait-Until "pane $Pane to receive focus" {
            $active = @((Control @('list')).sessions | Where-Object active)
            $active.Count -eq 1 -and $active[0].id -eq $Pane
        }
    } catch {
        throw "$_ Current panes: $((Control @('list')).sessions | Select-Object id,label,title,active,visible | ConvertTo-Json -Compress)"
    }
}
function Assert-Input([int]$Pane, [string]$Marker, [int[]]$OtherPanes) {
    foreach ($letter in $Marker.ToCharArray()) { Post-Key 0x102 ([uint32]$letter) }
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','5',$Marker)
    foreach ($other in $OtherPanes) {
        if ((Control @('get-text','--pane',"$other")).text.Contains($Marker)) {
            throw "Keystrokes intended for pane $Pane also reached $other"
        }
    }
}

try {
    $window = Wait-Until 'the fixture window' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
    $manifest = Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json"
    $null = Wait-Until 'the fixture control pipe' { Test-Path -LiteralPath $manifest }
    $panes = (Control @('list')).sessions
    $left = ($panes | Where-Object label -eq 'dev1').id
    $right = ($panes | Where-Object label -eq 'dev2').id
    $local = ($panes | Where-Object label -eq 'Windows PowerShell').id
    if ($panes.Count -ne 3 -or !$left -or !$right -or !$local) { throw "Unexpected workspace: $($panes | ConvertTo-Json -Compress)" }
    Wait-Focus $left
    foreach ($pane in $left,$right) { $null = Control @('wait-for-text','--pane',"$pane",'--timeout','10','ready') }

    # Give dev1 a remote title unrelated to its saved profile or address.
    $osc = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("$([char]27)]2;build-output$([char]7)"))
    $null = Control @('send-text','--pane',"$left",'--no-paste','--base64',$osc)
    $null = Wait-Until 'remote window title' { (Control @('get-text','--pane',"$left")).title -eq 'build-output' }

    Picker-Query 'DEV2'
    Press-Key 13
    Wait-Focus $right
    Assert-Input $right 'after-enter' @($left,$local)

    Picker-Query 'build-output'
    Press-Key 27
    $null = Wait-Until 'picker dismissal' { -not (Find-Element 'Find pane') }
    Wait-Focus $right
    Assert-Input $right 'after-escape' @($left,$local)

    Picker-Query 'build-output'
    Press-Key 13
    Wait-Focus $left
    Picker-Query 'navigator@127.0.0.1'
    Press-Key 13
    Wait-Focus $right
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
