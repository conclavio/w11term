Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class FixtureMessages {
    [DllImport("user32.dll", SetLastError=true)] public static extern bool PostMessage(
        IntPtr hwnd, uint msg, UIntPtr key, IntPtr detail);
}
'@

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
function Find-Element([string]$Name, [System.Windows.Automation.ControlType]$Type) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    if ($Type) {
        $byType = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $Type)
        $condition = [System.Windows.Automation.AndCondition]::new($condition, $byType)
    }
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
function Post-Key([uint32]$Message, [uint32]$Key) {
    # Address this process's window. This never types into the foreground application.
    if (-not [FixtureMessages]::PostMessage($window, $Message, [UIntPtr]::new($Key), [IntPtr]::Zero)) {
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
function Picker-Query([string]$Query) {
    Write-Output "Searching for $Query"
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    Invoke-Button (Wait-Until 'Find pane menu item' { Find-Element ('Find pane' + [char]0x2026 + '    Ctrl+Shift+P') })
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
