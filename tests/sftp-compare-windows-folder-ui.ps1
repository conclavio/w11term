[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_SFTP_PICKER_SMOKE_DIR = Join-Path $State 'picker'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$text = [System.Windows.Automation.ControlType]::Text
$dot = [char]0xb7
$name = 'Windows reference ' + [char]0x96ea
$source = Join-Path $env:W11TERM_SFTP_PICKER_SMOKE_DIR $name
$pickerTitle = 'Choose a Windows folder for comparison'
$choose = 'Set Windows folder as reference' + [char]0x2026
$compare = 'Compare folder with reference' + [char]0x2026

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Read-TextById([string]$Id) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $byId = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
    $item = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $byId)
    if ($item) { $item.Current.Name }
}

function Normalize-FixturePath([string]$Path) {
    if ($Path.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) {
        return '\\' + $Path.Substring(8).TrimEnd('\')
    }
    if ($Path.StartsWith('\\?\')) { $Path = $Path.Substring(4) }
    return $Path.TrimEnd('\')
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = (Control @('list')).sessions[0]
    $null = Control @('wait-for-text','--pane',"$($pane.id)",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Invoke-SftpFolderAction $choose -Comparison
    $dialog = Wait-Until 'owned Windows folder picker' { Find-OwnedDialog $pickerTitle }
    Select-DialogItem $dialog $name
    $null = Wait-Until 'accept the selected Windows folder' { [FixtureDialog]::ClickButton($dialog, 1) }
    $captured = Wait-Until 'capture excludes the target of the Windows junction' {
        if (Find-Element "Captured folder reference $dot 5 entries") {
            Read-TextById 'sftp-reference-folder'
        }
    }
    # Windows can seed the test with an 8.3 temporary path and return the full path from its picker.
    $canonical = [IO.File]::ReadAllText((Join-Path $State 'canonical-picker-directory'))
    $expected = @((Normalize-FixturePath $source), (Normalize-FixturePath $canonical))
    if ($expected -inotcontains (Normalize-FixturePath $captured)) {
        throw "Picker captured '$captured'. Expected '$source' or '$canonical'"
    }
    Click 'Close folder reference'
    $label = "Windows folder reference  $captured"
    $null = Wait-Until 'the drawer shows the captured Windows reference' { Find-Element $label $text }
    Invoke-SftpFolderAction $choose -Comparison
    $dialog = Wait-Until 'folder picker reopened' { Find-OwnedDialog $pickerTitle }
    $null = Wait-Until 'cancel the second picker' { [FixtureDialog]::ClickButton($dialog, 2) }
    $null = Wait-Until 'picker cancellation keeps the current reference' {
        (Find-Element $label $text) -and !(Find-OwnedDialog $pickerTitle)
    }

    [IO.File]::WriteAllText((Join-Path $source 'nested/config.txt'), 'current!', [Text.UTF8Encoding]::new($false))
    Invoke-SftpFolderAction $compare -Comparison
    $null = Wait-Until 'the comparison matches exact path case and reports the link as uncertain' {
        (Find-Element "Windows reference folder  $captured" $text) -and
            (Find-Element '/shell directory' $text) -and
            (Find-Element 'Only in reference  2' $text) -and
            (Find-Element 'Only in current folder  2' $text) -and
            (Find-Element 'File metadata changed  1' $text) -and
            (Find-Element 'Needs inspection  1' $text) -and
            (Find-Element 'Matching entries  1' $text)
    }
    $null = Wait-Until 'the comparison shows the pinned Windows size with the remote size' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $text)
        $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition) | Where-Object {
            $label = $_.Current.Name
            $label.StartsWith("File metadata changed $dot nested/config.txt") -and
                $label.Contains('Reference  File  3 bytes') -and $label.Contains('Current  File  8 bytes')
        }
    }
    Click 'Close folder comparison'
    $null = Wait-Until 'the comparison dialog closed' { !(Find-Element 'Close folder comparison' $button) }
    $active = (Control @('list')).sessions[0]
    if ($active.id -ne $pane.id -or $active.connection_generation -ne $pane.connection_generation) {
        throw 'The Windows folder comparison changed the SSH connection'
    }
} catch {
    $failure = $_
    $details = try {
        if ($window -and !$app.HasExited) {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            $condition = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $text)
            ($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition) |
                ForEach-Object { $_.Current.Name } | Select-Object -Last 32) -join ' | '
        }
    } catch {}
    throw "$($failure.Exception.Message)$([char]10)$($failure.ScriptStackTrace)$([char]10)Window text: $details"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
