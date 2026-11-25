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
$textPattern = [System.Windows.Automation.TextPattern]::Pattern
$reference = '/shell directory/archive/config.txt'
$current = '/shell directory/live/config.txt'
$identical = '/shell directory/live/matching.txt'
$referenceLabel = "Comparison reference  $reference"

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Close-Preview {
    Click 'Close preview'
    $null = Wait-Until 'the preview closed' { !(Find-Element 'Close preview' $button) }
}
function Wait-Reference {
    $null = Wait-Until 'the pinned reference path is displayed' {
        $item = Find-Element $referenceLabel
        $item -and $item.Current.AutomationId -eq 'sftp-comparison-reference'
    }
}
function Wait-Difference {
    $field = Wait-Until 'remote text differences' {
        $item = Find-Element 'Text differences'
        if ($item -and $item.Current.AutomationId -eq 'sftp-text-differences') { $item }
    }
    $lines = $field.GetCurrentPattern($textPattern).DocumentRange.GetText(-1)
    $dot = [char]0xb7
    $removed = [char]0x2212
    if (!$lines.Contains("$removed Reference line 2 $dot pinned [CRLF]") -or
            !$lines.Contains("+ Current line 2 $dot live [LF]") -or
            $lines.Contains('archive changed')) {
        throw "Changed lines were routed to the wrong snapshot or side. Displayed: $lines"
    }
    $null = Wait-Until 'comparison names both remote paths' {
        (Find-Element "Reference  $reference") -and (Find-Element "Current preview  $current")
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $paneId = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$paneId",'--timeout','15','src/trace.rs')
    $pane = (Control @('list')).sessions | Where-Object id -eq $paneId
    Run-Palette 'Open files here'
    Click 'archive/'
    Click 'Open folder'
    Click 'config.txt'
    Click ('Preview text' + [char]0x2026)
    Click 'Set as comparison reference'
    Wait-Reference
    $null = Wait-Until 'pinning returns to the file list' { !(Find-Element 'Close preview' $button) }
    $archive = Join-Path $fixture 'shell directory/archive/config.txt'
    [IO.File]::WriteAllText($archive, 'archive changed', [Text.UTF8Encoding]::new($false))

    Click 'Back'
    Click 'live/'
    Click 'Open folder'
    Wait-Reference
    Click 'config.txt'
    Click ('Compare selected with reference' + [char]0x2026)
    Wait-Difference
    Click 'Return to preview'
    $null = Wait-Until 'the second file can be read in its normal preview' {
        $field = Find-Element 'File contents'
        $field -and $field.GetCurrentPattern($textPattern).DocumentRange.GetText(-1).Contains('live')
    }
    Click 'Compare with reference'
    Wait-Difference
    Close-Preview

    Click ('Find files' + [char]0x2026)
    Wait-Reference
    $query = Wait-Until 'filename search query' { Find-Element 'Filename contains' }
    $query.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('matching.txt')
    Click 'Search'
    $compareResult = "Compare $identical with reference"
    Click $compareResult
    $null = Wait-Until 'search result matches the pinned snapshot' {
        Find-Element 'The current preview matches the reference.'
    }
    Close-Preview
    Click 'Clear comparison reference'
    $null = Wait-Until 'clearing the reference removes comparisons but leaves results open' {
        !(Find-Element $referenceLabel) -and !(Find-Element $compareResult $button) -and
            (Find-Element "Preview text $identical" $button)
    }
    $active = (Control @('list')).sessions[0]
    if ($active.id -ne $pane.id -or $active.connection_generation -ne $pane.connection_generation) {
        throw 'Comparing remote files changed the SSH session'
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
