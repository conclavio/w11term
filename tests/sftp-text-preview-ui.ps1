[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$textPattern = [System.Windows.Automation.TextPattern]::Pattern
$preview = 'Preview text' + [char]0x2026
$previewImage = 'Preview image' + [char]0x2026
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
function Wait-MapPreview {
    $dimensions = '64 ' + [char]0x00d7 + ' 32 pixels'
    $null = Wait-Until 'image bitmap and original dimensions' {
        $image = Find-Element "Image preview of map $snow.PNG" ([System.Windows.Automation.ControlType]::Image) -Visible
        $image -and $image.Current.AutomationId -eq 'sftp-preview-image' -and
            $image.Current.BoundingRectangle.Height -gt 0 -and
            $image.Current.BoundingRectangle.Width -gt 0 -and (Find-Element $dimensions)
    }
}
function Close-Drawer {
    Click 'Close files'
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
    $null = Wait-Until 'entry actions disabled with no selection' {
        $item = Find-Element 'Selected entry actions' $button
        $item -and !$item.Current.IsEnabled
    }

    Click $name
    Invoke-SftpEntryAction $preview
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
    Invoke-SftpEntryAction $preview
    $null = Wait-Preview $outer
    Close-Preview
    Click 'nested/'
    $properties = Get-SftpEntryAction ('Properties' + [char]0x2026)
    if (Find-Element $preview ([System.Windows.Automation.ControlType]::MenuItem)) {
        throw 'A directory offered text preview'
    }
    Invoke-Button $properties
    $null = Wait-Until 'the menu is scoped to the selected directory' {
        $item = Find-Element 'File properties'
        if ($item) {
            $body = $item.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1)
            $body.Contains('Path  /shell directory/nested') -and $body.Contains('Type  Directory')
        }
    }
    Click 'Close properties'
    Click 'Open folder'
    $null = Wait-Until 'child directory listing' { (Find-Element '/shell directory/nested') -and (Find-Element 'binary.dat' $button) }
    Click $name
    Invoke-SftpEntryAction $preview
    $null = Wait-Preview $inner
    Close-Preview
    Click 'binary.dat'
    Invoke-SftpEntryAction $preview
    $null = Wait-Until 'binary file rejected inside the preview dialog' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-preview-error')
        $msg = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        $msg -and $msg.Current.Name.Contains('NUL bytes') -and !(Find-Element 'File contents')
    }
    Close-Preview
    Click "map $snow.PNG"
    Invoke-SftpEntryAction $previewImage
    Wait-MapPreview
    Close-Preview
    Click 'broken.jpeg'
    Invoke-SftpEntryAction $previewImage
    $null = Wait-Until 'invalid image error and no native bitmap' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-image-error')
        $field = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        $field -and $field.Current.Name.StartsWith('Could not preview this image.') -and
            !(Find-Element 'Image preview of broken.jpeg' ([System.Windows.Automation.ControlType]::Image))
    }
    Close-Preview
    Click ('Find files' + [char]0x2026)
    $query = Wait-Until 'filename search query' {
        Find-Element 'Filename contains' ([System.Windows.Automation.ControlType]::Edit)
    }
    $query.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('map')
    Click 'Search'
    Click "Preview image /shell directory/nested/map $snow.PNG"
    Wait-MapPreview
    Close-Preview
    Click 'Back to files'
    $null = Wait-Until 'file actions restored after search' { Find-Element 'Selected entry actions' $button }
    Close-Drawer
    $null = Wait-Until 'drawer closed' { !(Find-Element 'Selected entry actions' $button) }
    foreach ($ch in 'after-preview'.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','after-preview')
    $active = (Control @('list')).sessions | Where-Object id -eq $remote
    if (!$active.active -or $active.connection_generation -ne $generation -or $active.tab_id -ne $ssh.tab_id) { throw 'SSH pane or connection changed during preview' }
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $localPane
    Type-Line 'if ($previewPid -eq $PID) { "ps-preview-" + "still-alive" }'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','ps-preview-still-alive')

    $null = Control @('focus-pane','--pane',"$remote")
    Wait-Focus $remote
    Run-Palette 'Open files here'
    Click '.bashrc'
    Invoke-SftpEntryAction $preview
    Click 'Use in dotfiles editor'
    $edit = [System.Windows.Automation.ControlType]::Edit
    $value = [System.Windows.Automation.ValuePattern]::Pattern
    $before = [IO.File]::ReadAllText($profiles)
    $null = Wait-Until 'the profile editor shows the verified SSH excerpt source' {
        $label = Get-NativeTextLabels | Where-Object {
            $_.StartsWith('SSH excerpt from ') -and $_.Contains('/shell directory/.bashrc')
        }
        $label -and (Find-Element 'First SSH line' $edit) -and (Find-Element 'Last SSH line' $edit)
    }
    Invoke-Button (Get-ScrollableMenuAction 'Append SSH lines' 'Append to Bash fragment' -VisibleItem)
    $null = Wait-Until 'the managed startup block cannot be imported into itself' {
        Find-Element 'Choose lines outside the w11term-managed startup block'
    }
    foreach ($field in @('First SSH line','Last SSH line')) {
        (Find-Element $field $edit).GetCurrentPattern($value).SetValue('2')
    }
    $snippet = "alias ll='ls -l' # $snow`n"
    $null = Wait-Until 'the staged preview has only line two selected' {
        $text = Find-Element 'Selected SSH lines'
        if ($text) {
            (Preview-Text $text.GetCurrentPattern($textPattern).DocumentRange) -ceq "2  $snippet"
        }
    }
    Invoke-Button (Get-ScrollableMenuAction 'Append SSH lines' 'Append to Bash fragment' -VisibleItem)
    $null = Wait-Until 'only the chosen SSH excerpt is in the Bash fragment' {
        $field = Find-Element 'Bash startup fragment' $edit
        if ($field) {
            ($field.GetCurrentPattern($value).Current.Value -replace '\r\n?', "`n") -ceq $snippet
        }
    }
    Click 'Close SSH excerpt'
    Click 'Save dotfiles bundle'
    $sessionBundle = Join-Path $State 'Imported SSH excerpt.json'
    $dialog = Wait-OwnedDialog 'Save new dotfiles bundle'
    $null = Wait-Until 'enter a destination for the imported fragment' {
        [FixtureDialog]::SetCommonDialogFilename($dialog, $sessionBundle)
    }
    $null = Wait-Until 'save the imported fragment' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'the local bundle has only the reviewed excerpt' {
        if ([IO.File]::Exists($sessionBundle)) {
            $bundle = [IO.File]::ReadAllText($sessionBundle) | ConvertFrom-Json
            $bundle.bash -ceq $snippet -and $null -eq $bundle.zsh -and $null -eq $bundle.environment
        }
    }
    if ([IO.File]::ReadAllText($profiles) -cne $before) { throw 'Importing an excerpt saved the SSH profile' }
    $stillConnected = (Control @('list')).sessions | Where-Object id -eq $remote
    if ($stillConnected.connection_generation -ne $generation) { throw 'Import changed the SSH connection' }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
