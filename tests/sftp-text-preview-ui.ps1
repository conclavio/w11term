[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$textPattern = [System.Windows.Automation.TextPattern]::Pattern
$preview = 'Preview text' + [char]0x2026
$snow = [char]0x96ea
$name = "build $snow.txt"
$needle = 'second caf' + [char]0xe9
$outer = "first $snow`n`t$needle`n`nlast line"
$inner = "different directory`ninner $snow"

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $b = Find-Element $Name $button
        if ($b -and $b.Current.IsEnabled) { $b }
    })
}
function Type-Line([string]$Line) {
    foreach ($ch in $Line.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    Press-Key 13
}
function Preview-Text($Range) {
    # Both the WinUI text provider and Windows edit controls can normalize
    # paragraph separators. Preserve all logical lines, tabs and blank lines.
    return $Range.GetText(-1).Replace("`r`n", "`n").Replace("`r", "`n")
}
function Wait-Preview([string]$Expected) {
    $label = Wait-Until 'selectable contents in the file preview dialog' {
        $field = Find-Element 'File contents'
        if ($field -and $field.Current.AutomationId -eq 'sftp-preview-text') { $field }
    }
    $text = $label.GetCurrentPattern($textPattern)
    $value = Preview-Text $text.DocumentRange
    if ($value -cne $Expected) { throw "Unexpected preview text '$value', expected '$Expected'" }
    $mutable = $null
    if ($label.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$mutable) -and !$mutable.Current.IsReadOnly) {
        throw 'File preview exposes a writable UI Automation value'
    }
    return $text
}
function Close-Preview {
    Click 'Close preview'
    $null = Wait-Until 'file preview dialog closed' { !(Find-Element 'Close preview' $button) }
}
function Wait-DisabledPreview {
    $null = Wait-Until 'preview disabled for the current selection' {
        $b = Find-Element $preview $button
        $b -and !$b.Current.IsEnabled
    }
}
function Close-Drawer {
    # The window caption also has a Close button. Find the one in the nearest
    # common WinUI container with the file actions.
    $scope = Find-Element $preview $button
    $condition = [System.Windows.Automation.AndCondition]::new(
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, 'Close'),
        [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty, $button))
    while ($scope -and $scope.Current.ProcessId -eq $app.Id) {
        $options = $scope.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)
        if ($options.Count -eq 1) { Invoke-Button $options[0]; return }
        if ($options.Count -gt 1) { break }
        $scope = [System.Windows.Automation.TreeWalker]::RawViewWalker.GetParent($scope)
    }
    throw 'Could not locate the drawer Close button independently of the window caption'
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $ssh = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh')
    $remote = $ssh.id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    $generation = ((Control @('list')).sessions | Where-Object id -eq $remote).connection_generation
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'PowerShell neighbor' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','PS ')
    Type-Line '$global:previewPid = $PID; "ps-preview-" + "seeded"'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','ps-preview-seeded')
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    Run-Palette 'Open files here'
    $null = Wait-Until 'shell directory listing' { (Find-Element '/shell directory') -and (Find-Element $name $button) }
    Wait-DisabledPreview

    Click $name
    Click $preview
    $text = Wait-Preview $outer
    $text.DocumentRange.Select()
    $null = Wait-Until 'native preview selection contains the displayed text' {
        $ranges = @($text.GetSelection())
        $ranges.Count -eq 1 -and (Preview-Text $ranges[0]) -ceq $outer
    }
    $null = Press-FocusedKey 0x4b
    $null = Wait-Preview $outer
    Close-Preview
    # A second preview without clicking a row proves the drawer kept its selection.
    Click $preview
    $null = Wait-Preview $outer
    Close-Preview
    Click 'nested/'
    Wait-DisabledPreview
    Click 'Open folder'
    $null = Wait-Until 'child directory listing' { (Find-Element '/shell directory/nested') -and (Find-Element 'binary.dat' $button) }
    Click $name
    Click $preview
    $null = Wait-Preview $inner
    Close-Preview
    Click 'binary.dat'
    Click $preview
    $null = Wait-Until 'binary file rejected inside the preview dialog' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-preview-error')
        $msg = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        $msg -and $msg.Current.Name.Contains('NUL bytes') -and !(Find-Element 'File contents')
    }
    Close-Preview
    Close-Drawer
    $null = Wait-Until 'drawer closed' { !(Find-Element $preview $button) }
    foreach ($ch in 'after-preview'.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','after-preview')
    $active = (Control @('list')).sessions | Where-Object id -eq $remote
    if (!$active.active -or $active.connection_generation -ne $generation -or $active.tab_id -ne $ssh.tab_id) { throw 'SSH pane or connection changed during preview' }
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $localPane
    Type-Line 'if ($previewPid -eq $PID) { "ps-preview-" + "still-alive" }'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','ps-preview-still-alive')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
