[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$fixture = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$gate = Join-Path $fixture 'hold-opendir'
$paused = Join-Path $fixture 'opendir-paused'
$button = [System.Windows.Automation.ControlType]::Button
$contents = 'Folder contents' + [char]0x2026
$properties = 'Properties' + [char]0x2026

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Wait-Row([string]$Key, [string]$Expected) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, "sftp-contents-$Key")
    $null = Wait-Until "$Key includes '$Expected'" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $item = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        $item -and ($item.Current.Name -replace '\s+', ' ').Contains($Expected)
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Click 'payload.bin'
    Invoke-SftpFolderAction $contents
    Wait-Row 'folder' '/shell directory'
    Wait-Row 'logical-bytes' '5 KiB (5120 bytes)'
    Wait-Row 'files' 'Regular files 2'
    Wait-Row 'folders' 'Folders visited 2'
    Wait-Row 'links' 'Symbolic links skipped 1'

    [IO.File]::WriteAllText($gate, '/shell directory/sub')
    Click 'Measure again'
    $null = Wait-Until 'second scan blocks in the child folder' { [IO.File]::Exists($paused) }
    Click 'Stop scan'
    $null = Wait-Until 'the scan reports that it is stopping' { Find-Element ('Stopping folder scan' + [char]0x2026) }
    [IO.File]::Delete($gate)
    Wait-Row 'partial' 'Partial results. Scan stopped.'
    Wait-Row 'logical-bytes' '4 KiB (4096 bytes)'
    Wait-Row 'files' 'Regular files 1'
    Click 'Close contents report'
    $null = Wait-Until 'the selected file stays active after closing' {
        $item = Find-Element $properties $button
        $item -and $item.Current.IsEnabled
    }
} catch {
    throw "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
} finally {
    [IO.File]::Delete($gate)
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
