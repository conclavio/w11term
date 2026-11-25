[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"find_all_panes":"F6","command_palette":"F7","select_tab_1":"F8","select_tab_2":"F9"}}')
$profiles = Join-Path $State 'profiles.json'
$editType = [System.Windows.Automation.ControlType]::Edit
$buttonType = [System.Windows.Automation.ControlType]::Button
$valuePattern = [System.Windows.Automation.ValuePattern]::Pattern
$globalAction = 'Find in all panes' + [char]0x2026
$query = 'fleet.target'

function Find-Edit([string]$Name) {
    Wait-Until "focus in $Name" {
        $field = Find-Element $Name $editType
        if ($field -and $field.Current.HasKeyboardFocus) { $field }
    }
}
function Open-LocalFind {
    Run-Palette 'Find in scrollback    Ctrl+Shift+F'
    Find-Edit 'Find in scrollback'
}
function Wait-LocalStatus([string]$Value) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty,'find-status')
    $null = Wait-Until "local Find status $Value" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $node = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,$condition)
        $node -and $node.Current.Name -ceq $Value
    }
}
function Close-LocalFind {
    Press-Key 27
    $null = Wait-Until 'local Find closed' { !(Find-Element 'Find in scrollback' $editType) }
}
function Viewport([string]$Label, [string]$Witness) {
    return Wait-Until "$Witness visible in $Label" {
        $doc = Find-Element ('Terminal ' + [char]0xb7 + ' ' + $Label)
        if ($doc) {
            $range = $doc.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange
            $visible = $range.GetText(-1)
            if ($visible.Contains($Witness)) { $visible }
        }
    }
}
function Set-Anchor([string]$Label, [string]$Anchor) {
    $field = Open-LocalFind
    $field.GetCurrentPattern($valuePattern).SetValue($Anchor)
    Wait-LocalStatus '1 / 1'
    Close-LocalFind
    return Viewport $Label $Anchor
}
function Wait-Results([int]$SshCount = 2) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,$buttonType)
    $last = [ref]''
    try {
        return Wait-Until 'one accessible result per pane with its source, match count, and historical excerpt' {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            $labels = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$condition) |
                ForEach-Object { $_.Current.Name } | Where-Object { $_ -match '^Pane [0-9]+ ' })
            $last.Value = $labels -join ' / '
            if ($labels.Count -eq 2 -and $labels[0] -like "Pane $remote *palette-ssh*$SshCount match*ssh-first Fleet.Target north*" -and
                $labels[1] -like "Pane $local *Palette PowerShell*1 match*local-only Fleet.Target east*") { $labels }
        }
    } catch { throw "$_ Last result rows: $($last.Value)" }
}
function Type-Terminal([string]$Text) {
    foreach ($c in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
}
function Live-Screen([int]$Pane) { (Control @('get-text','--pane',"$Pane")).text }
function Assert-Query($Field, [string]$Expected) {
    $value = $Field.GetCurrentPattern($valuePattern).Current.Value
    if ($value -cne $Expected) { throw "Expected saved Find query '$Expected', got '$value'" }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','130','--rows','30') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ssh-window-search-ready')
    $local = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $local
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    # Assemble the search words at execution time so the echoed command cannot be a hit.
    $setup = '$global:FindAnswer=42; "local-only " + "Fleet." + "Target east"; 1..65 | % { "local old $_" }; "local-" + "prior-anchor"; 1..65 | % { "local recent $_" }; "global-ready-" + $global:FindAnswer'
    $packet = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($setup + [char]13))
    $null = Control @('send-text','--pane',"$local",'--no-paste','--base64',$packet)
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','global-ready-42')
    $panes = @((Control @('list')).sessions)
    if ($panes.Count -ne 2 -or @($panes.tab_id | Sort-Object -Unique).Count -ne 2) { throw 'Expected two terminal tabs' }
    if ((Live-Screen $remote) -match 'fleet.target' -or (Live-Screen $local) -match 'fleet.target') { throw 'Both hits must start above the live screens' }

    Press-Key 0x77 # F8, first tab
    Wait-Focus $remote
    $sshView = Set-Anchor 'palette-ssh' 'ssh-prior-anchor'
    Press-Key 0x78 # F9, second tab
    Wait-Focus $local
    $localView = Set-Anchor 'Palette PowerShell' 'local-prior-anchor'
    $field = Open-LocalFind
    Assert-Query $field 'local-prior-anchor'
    $beforeSsh = Live-Screen $remote
    $beforeLocal = Live-Screen $local

    # Open through the caret while ordinary Find owns focus.
    Invoke-Button (Wait-Until 'caret menu' { Find-Element 'Tab menu' $buttonType })
    Invoke-Button (Wait-Until 'Find in all panes menu action' { Find-Element ($globalAction + '    F6') })
    $all = Find-Edit 'Find in all panes'
    $all.GetCurrentPattern($valuePattern).SetValue('no-such-global-result')
    $null = Wait-Until 'empty search result' { Find-Element 'No matching panes' }
    Post-Key 0x102 ([uint32][char]'z')
    Press-Key 13
    $all = Find-Edit 'Find in all panes'
    $all.GetCurrentPattern($valuePattern).SetValue($query)
    $null = Wait-Results
    Wait-Focus $local
    if ((Live-Screen $remote) -cne $beforeSsh -or (Live-Screen $local) -cne $beforeLocal) { throw 'Searching changed the live terminal text' }
    Press-Key 27
    $field = Find-Edit 'Find in scrollback'
    Assert-Query $field 'local-prior-anchor'
    Wait-LocalStatus '1 / 1'
    Close-LocalFind
    if ((Viewport 'Palette PowerShell' 'local-prior-anchor') -cne $localView) { throw 'Cancel changed the active terminal viewport' }
    Press-Key 0x77
    Wait-Focus $remote
    if ((Viewport 'palette-ssh' 'ssh-prior-anchor') -cne $sshView) { throw 'Search changed the background SSH viewport' }
    $field = Open-LocalFind
    Assert-Query $field 'ssh-prior-anchor'
    Wait-LocalStatus '1 / 1'
    Close-LocalFind

    Press-Key 0x78
    Wait-Focus $local
    Press-Key 0x75 # F6, remapped global search from terminal input
    $all = Find-Edit 'Find in all panes'
    $all.GetCurrentPattern($valuePattern).SetValue($query)
    $null = Wait-Results
    Press-Key 13 # The visually first result belongs to the background SSH tab.
    Wait-Focus $remote
    $field = Find-Edit 'Find in scrollback'
    Assert-Query $field $query
    Wait-LocalStatus '1 / 2'
    Close-LocalFind
    $first = Viewport 'palette-ssh' 'ssh-first Fleet.Target north'
    if ($first.Contains('ssh-second FLEET.TARGET south')) { throw 'Expected separately visible SSH matches' }
    $field = Open-LocalFind
    Assert-Query $field $query
    Wait-LocalStatus '1 / 2'
    Press-Key 13
    Wait-LocalStatus '2 / 2'
    Close-LocalFind
    $null = Viewport 'palette-ssh' 'ssh-second FLEET.TARGET south'
    Type-Terminal 'global-ssh-ok'
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','global-ssh-ok')
    Press-Key 0x78
    Wait-Focus $local
    Type-Terminal 'Write-Output ("global-local-ok-" + $global:FindAnswer)'
    Press-Key 13
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','global-local-ok-42')

    Press-Key 0x75 # Find in all panes from PowerShell again.
    $null = Find-Edit 'Find in all panes'
    (Wait-CheckBox 'Regular expression in all panes' ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $all = Find-Edit 'Find in all panes'
    $all.GetCurrentPattern($valuePattern).SetValue('[')
    $status = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'pane-search-status')
    $null = Wait-Until 'invalid regex diagnostic' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $node = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $status)
        $node -and $node.Current.Name -like 'Invalid regex*'
    }
    $regex = 'Fleet[.]Target (north|south|east)'
    $all.GetCurrentPattern($valuePattern).SetValue($regex)
    $null = Wait-Results
    (Wait-CheckBox 'Match case in all panes' ([System.Windows.Automation.ToggleState]::Off)).Toggle()
    $null = Find-Edit 'Find in all panes'
    $null = Wait-Results 1
    Press-Key 13
    Wait-Focus $remote
    $field = Find-Edit 'Find in scrollback'
    Assert-Query $field $regex
    $null = Wait-CheckBox 'Regular expression' ([System.Windows.Automation.ToggleState]::On)
    $null = Wait-CheckBox 'Match case' ([System.Windows.Automation.ToggleState]::On)
    Wait-LocalStatus '1 / 1'
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
