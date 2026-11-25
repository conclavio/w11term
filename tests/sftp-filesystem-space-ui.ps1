[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$fixture = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$statsPath = Join-Path $fixture 'fixture-space.json'
$button = [System.Windows.Automation.ControlType]::Button
$space = 'Filesystem space' + [char]0x2026

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Wait-Row([string]$Key, [string]$Expected) {
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, "sftp-space-$Key")
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

    Click 'keep.txt'
    Invoke-SftpFolderAction $space
    Wait-Row 'folder' '/shell directory'
    Wait-Row 'available' '1 GiB (1073741824 bytes)'
    Wait-Row 'used' '2 GiB (2147483648 bytes)'
    Wait-Row 'capacity' '4 GiB (4294967296 bytes)'
    Wait-Row 'reserved' '1 GiB (1073741824 bytes)'
    if (Find-Element 'Read-only filesystem') { throw 'Unexpected read-only report' }

    $stats = [IO.File]::ReadAllText($statsPath) | ConvertFrom-Json
    $stats.blocks_avail = 131072
    $stats.flags = 1
    [IO.File]::WriteAllText($statsPath, ($stats | ConvertTo-Json -Compress))
    Click 'Refresh space'
    Wait-Row 'available' '512 MiB (536870912 bytes)'
    Wait-Row 'reserved' '1.5 GiB (1610612736 bytes)'
    $null = Wait-Until 'the refreshed filesystem is read-only' { Find-Element 'Read-only filesystem' }
    Click 'Close space report'
    $null = Wait-Until 'the selected file remains active' {
        $item = Find-Element 'Selected entry actions' $button
        $item -and $item.Current.IsEnabled
    }

    Click 'child/'
    Click 'Open folder'
    $null = Wait-Until 'child is the displayed folder' { Find-Element '/shell directory/child' }
    Invoke-SftpFolderAction $space
    Wait-Row 'folder' '/shell directory/child'
    Wait-Row 'available' '512 MiB (536870912 bytes)'
    [IO.File]::Delete($statsPath)
    Click 'Refresh space'
    Wait-Row 'error' 'Could not read filesystem space.'
    Click 'Close space report'
    $null = Wait-Until 'the failed report leaves the child listing open' {
        (Find-Element '/shell directory/child') -and !(Find-Element 'Close space report' $button)
    }
} catch {
    throw "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
