# Native Find in a loopback SSH / PowerShell split. All keys target the fixture HWND.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'

function Find-Field {
    Find-Element 'Find in scrollback' ([System.Windows.Automation.ControlType]::Edit)
}
function Open-Find {
    param([switch]$Palette)
    if ($Palette) { Run-Palette 'Find in scrollback    F6' } else { Press-Key 0x75 }
    $null = Wait-Until 'focused Find edit field' {
        $edit = Find-Field
        $edit -and $edit.Current.HasKeyboardFocus
    }
}
function Set-FindBinding([string]$Label, [string]$Before, [string]$After) {
    Invoke-Button (Wait-Until "Edit $Label" { Find-Element "Edit $Label" })
    $field = Wait-Until "shortcut for $Label" { Find-Element "Shortcut for $Label" }
    $value = $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    if ($value.Current.Value -ne $Before) { throw "Expected $Before for $Label, found $($value.Current.Value)" }
    $value.SetValue($After)
    Invoke-Button (Wait-Until "save $After" {
        $save = Find-Element 'Save shortcut'
        if ($save -and $save.Current.IsEnabled) { return $save }
    })
    $null = Wait-Until "saved $Label" { Find-Element "Edit $Label" }
}
function Set-Query([string]$Query) {
    (Find-Field).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Query)
}
function Set-Option([string]$Name, [bool]$Enabled) {
    $box = Wait-Until $Name { Find-Element $Name ([System.Windows.Automation.ControlType]::CheckBox) }
    $toggle = $box.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
    if (($toggle.Current.ToggleState -eq 'On') -ne $Enabled) { $toggle.Toggle() }
    $null = Wait-Until "$Name = $Enabled" {
        $current = Find-Element $Name ([System.Windows.Automation.ControlType]::CheckBox)
        $current -and (($current.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -eq 'On') -eq $Enabled)
    }
}
function Wait-Status([string]$Expected) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'find-status')
    $last = [ref]''
    try {
        return Wait-Until "Find status matching $Expected" {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            $status = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
            if ($status) {
                $last.Value = $status.Current.Name
                if ($last.Value -match $Expected) { return $last.Value }
            }
        }
    } catch { throw "$_ Last Find status: '$($last.Value)'" }
}
function Close-Find([int]$Pane) {
    Press-Key 0x1b
    $null = Wait-Until 'Escape to dismiss Find' { !(Find-Field) }
    Wait-Focus $Pane
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','find-fixture-ready')
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
    Invoke-Button (Wait-Until 'settings menu' { Find-Element 'Settings' ([System.Windows.Automation.ControlType]::MenuItem) })
    Invoke-Button (Wait-Until 'keyboard settings' { Find-Element 'Keyboard' })
    Set-FindBinding 'Find in scrollback' 'Ctrl+Shift+F' 'F6'
    Set-FindBinding 'Find next' 'F3' 'F2'
    Set-FindBinding 'Find previous' 'Shift+F3' 'F1'
    Invoke-Button (Wait-Until 'back to settings' { Find-Element 'Back to settings' })
    $terminalTab = Wait-Until 'SSH and PowerShell tab' { Find-Element 'Palette PowerShell' ([System.Windows.Automation.ControlType]::TabItem) }
    $terminalTab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Wait-Focus $local
    Run-Palette 'Focus left    Ctrl+Alt+Left'
    Wait-Focus $remote
    $screen = Control @('get-text','--pane',"$remote")
    if ($screen.cols -ge 360 -or $screen.text -match 'CaseOne-start') {
        throw 'Expected soft-wrapped matches above the visible SSH viewport'
    }

    Open-Find
    $query = 'CaseOne-startx{360}-End-[0-9]{2}'
    Set-Query $query
    $null = Wait-Status '^No matches$' # the expression is literal by default
    Set-Option 'Regular expression' $true
    $initial = [int]((Wait-Status '^[1-3] / 3$') -split ' / ')[0]
    (Find-Field).SetFocus()
    Press-Key 13
    $next = ($initial % 3) + 1
    $null = Wait-Status "^$next / 3$"
    Press-Key 0x70 # rebound Find previous
    $null = Wait-Status "^$initial / 3$"
    Press-Key 0x71 # rebound Find next
    $null = Wait-Status "^$next / 3$"
    Invoke-Button (Find-Element 'Previous match (Shift+Enter)' ([System.Windows.Automation.ControlType]::Button))
    $null = Wait-Status "^$initial / 3$"
    Set-Option 'Match case' $true
    $null = Wait-Status '^[12] / 2$'
    Set-Option 'Regular expression' $false
    $null = Wait-Status '^No matches$'
    Set-Option 'Regular expression' $true
    $null = Wait-Status '^[12] / 2$'
    Set-Query '('
    $null = Wait-Status '^Invalid regex'
    (Find-Field).SetFocus()
    Post-Key 0x102 ([uint32][char]'q')
    Press-Key 13
    $null = Wait-Status '^Invalid regex'
    Set-Query ('a' * 4097)
    $null = Wait-Status '^Invalid regex.*4096'
    Set-Query $query
    $null = Wait-Status '^[12] / 2$'
    Close-Find $remote
    # The terminal's UIA document returns when the search edit releases focus.
    $null = Wait-Until 'the SSH viewport at the historical match' {
        $document = Find-Element ('Terminal ' + [char]0xb7 + ' palette-ssh')
        if ($document) {
            $text = $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
            $text.DocumentRange.GetText(-1) -match 'CaseOne-start'
        }
    }
    foreach ($c in 'find-ssh-resumed'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','find-ssh-resumed')
    Press-Key 0x72 # F3 belongs to the SSH program after rebinding Find next.

    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $local
    Open-Find -Palette
    Set-Query $query
    $null = Wait-Status '^No matches$'
    Close-Find $local
    foreach ($c in "'find-local-' + (40+2)".ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','10','find-local-42')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
