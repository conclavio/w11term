[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$fixture = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$env:W11TERM_SFTP_PICKER_SMOKE_DIR = Join-Path $State 'local'
$filename = 'config ' + [char]0x96ea + '.txt'
$remoteFile = Join-Path (Join-Path $fixture 'shell directory') $filename
$button = [System.Windows.Automation.ControlType]::Button
$text = [System.Windows.Automation.TextPattern]::Pattern

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Compare-With([string]$Name) {
    Click ('Compare with local file' + [char]0x2026)
    $title = 'Choose a local file to compare'
    $dialog = Wait-Until 'owned comparison file picker' { Find-OwnedDialog $title }
    if ($Name) {
        $path = Join-Path $env:W11TERM_SFTP_PICKER_SMOKE_DIR $Name
        $null = Wait-Until 'enter comparison filename' { [FixtureDialog]::SetFilename($dialog, $path, '') }
    }
    $id = if ($Name) { 1 } else { 2 }
    $null = Wait-Until 'dismiss local comparison picker' { [FixtureDialog]::ClickButton($dialog, $id) }
    $null = Wait-Until 'comparison file picker closed' { !(Find-OwnedDialog $title) }
}
function Wait-RemotePreview {
    $null = Wait-Until 'the original remote preview text is visible' {
        $field = Find-Element 'File contents'
        $field -and $field.GetCurrentPattern($text).DocumentRange.GetText(-1).Contains('from server')
    }
}
function Wait-Differences {
    $field = Wait-Until 'selectable changed lines are displayed' {
        $item = Find-Element 'Text differences'
        if ($item -and $item.Current.AutomationId -eq 'sftp-text-differences') { $item }
    }
    $range = $field.GetCurrentPattern($text)
    $lines = $range.DocumentRange.GetText(-1)
    $dot = [char]0xb7
    $removed = [char]0x2212
    foreach ($required in @(
        "$removed Remote line 1 $dot [UTF-8 BOM] header [CRLF]",
        "$removed Remote line 2 $dot from server [CRLF]",
        "+ Local line 2 $dot from local [LF]"
    )) {
        if (!$lines.Contains($required)) { throw "Missing comparison line '$required'. Displayed: $lines" }
    }
    $range.DocumentRange.Select()
    $null = Wait-Until 'the change list can be selected' {
        $selected = @($range.GetSelection())
        $selected.Count -eq 1 -and $selected[0].GetText(-1).Contains('from local')
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = (Control @('list')).sessions[0]
    $null = Control @('wait-for-text','--pane',"$($pane.id)",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Click $filename
    Click ('Preview text' + [char]0x2026)
    Wait-RemotePreview
    [IO.File]::WriteAllText($remoteFile, 'external change after preview', [Text.UTF8Encoding]::new($false))

    Compare-With 'same.txt'
    $null = Wait-Until 'original snapshot matches locally' { Find-Element 'The local file matches the remote preview.' }
    Compare-With 'different [local].txt'
    Wait-Differences
    if (Find-Element 'Edit' $button) { throw 'The comparison exposed an Edit action' }
    Compare-With ''
    Wait-Differences
    Compare-With 'binary.txt'
    $null = Wait-Until 'binary comparison reported inside the preview' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-text-comparison-status')
        $notice = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        $notice -and $notice.Current.Name.Contains('NUL bytes')
    }
    Click 'Return to preview'
    Wait-RemotePreview
    Click 'Close preview'
    $current = (Control @('list')).sessions[0]
    if ($current.id -ne $pane.id -or $current.connection_generation -ne $pane.connection_generation) {
        throw 'Comparison changed the SSH session'
    }
} catch {
    throw "$($_.Exception.Message)$([char]10)$($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
