[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$fixture = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$button = [System.Windows.Automation.ControlType]::Button
$dot = [char]0xb7
$reference = '/shell directory/archive'
$current = '/shell directory/live'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $paneId = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$paneId",'--timeout','15','src/trace.rs')
    $connection = ((Control @('list')).sessions | Where-Object id -eq $paneId).connection_generation
    Run-Palette 'Open files here'
    Click 'archive/'
    Click 'Open folder'
    Click ('Set folder as comparison reference' + [char]0x2026)
    $null = Wait-Until 'the reference inventory is captured' {
        Find-Element "Captured folder reference $dot 4 entries"
    }
    Click 'Close folder reference'
    $null = Wait-Until 'the folder reference is visible in the drawer' {
        (Find-Element "Folder reference  $reference") -and !(Find-Element 'Close folder reference' $button)
    }
    [IO.File]::WriteAllText((Join-Path $fixture 'shell directory/archive/nested/data.txt'), 'current!', [Text.UTF8Encoding]::new($false))

    Click 'Back'
    Click 'live/'
    Click 'Open folder'
    Click ('Compare folder with reference' + [char]0x2026)
    $null = Wait-Until 'a comparison identifies both folders and the expected counts' {
        (Find-Element "Reference folder  $reference") -and (Find-Element $current) -and
            (Find-Element 'Only in reference  1') -and (Find-Element 'Only in current folder  1') -and
            (Find-Element 'File metadata changed  1') -and (Find-Element 'Needs inspection  1')
    }
    $null = Wait-Until 'the changed nested file shows the pinned and current sizes' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $text = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Text)
        $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text) | Where-Object {
            $name = $_.Current.Name
            $name.StartsWith("File metadata changed $dot nested/data.txt") -and
                $name.Contains('Reference  File  3 bytes') -and $name.Contains('Current  File  8 bytes')
        }
    }
    Click 'Close folder comparison'
    $null = Wait-Until 'the comparison dialog is closed' { !(Find-Element 'Close folder comparison' $button) }
    Click 'Clear folder reference'
    $null = Wait-Until 'comparison actions are removed' {
        !(Find-Element "Folder reference  $reference") -and
            !(Find-Element ('Compare folder with reference' + [char]0x2026) $button)
    }
    $session = (Control @('list')).sessions[0]
    if ($session.id -ne $paneId -or $session.connection_generation -ne $connection) {
        throw 'Comparing folders changed the SSH connection'
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
