[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$fixture = [IO.File]::ReadAllText((Join-Path $State 'fixture-root'))
$name = 'server ' + [char]0x96ea + '.log'
$disk = Join-Path (Join-Path $fixture 'shell directory') $name
$button = [System.Windows.Automation.ControlType]::Button
$tail = 'Tail text file' + [char]0x2026
$utf8 = [Text.UTF8Encoding]::new($false)

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $b = Find-Element $Name $button
        if ($b -and $b.Current.IsEnabled) { $b }
    })
}
function Wait-Tail([string]$Expected) {
    $null = Wait-Until "selectable tail '$Expected'" {
        $field = Find-Element 'File tail'
        if ($field -and $field.Current.AutomationId -eq 'sftp-file-tail') {
            $value = $field.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1)
            $value.Replace([string][char]13, '').TrimEnd([char]10) -ceq $Expected
        }
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = (Control @('list')).sessions[0]
    $null = Control @('wait-for-text','--pane',"$($pane.id)",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Click $name
    Invoke-SftpEntryAction $tail
    Wait-Tail 'initial line'
    $size = [IO.FileInfo]::new($disk).Length
    $summary = "File size  $size bytes " + [char]0xb7 + ' Earlier text omitted'
    $null = Wait-Until 'earlier text omitted indicator' { Find-Element $summary }
    if (Find-Element 'Edit' $button) { throw 'The tail exposed a write action' }

    [IO.File]::AppendAllText($disk, "new line$([char]10)", $utf8)
    Click 'Refresh tail'
    Wait-Tail "initial line$([char]10)new line"
    [IO.File]::Move($disk, "$disk.old")
    Click 'Refresh tail'
    $null = Wait-Until 'the failed refresh is reported' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $match = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-preview-status')
        $message = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $match)
        $message -and $message.Current.Name.StartsWith('Could not refresh the file tail.')
    }
    Wait-Tail "initial line$([char]10)new line"

    [IO.File]::WriteAllText($disk, "rotated log$([char]10)", $utf8)
    Click 'Refresh tail'
    Wait-Tail 'rotated log'
    Click 'Close tail'
    Click ('Find files' + [char]0x2026)
    $query = Wait-Until 'filename search query' { Find-Element 'Filename contains' }
    $query.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($name)
    Click 'Search'
    Click "Tail text file /shell directory/$name"
    Wait-Tail 'rotated log'
    Click 'Close tail'
    $null = Wait-Until 'search results remain open' { Find-Element "Tail text file /shell directory/$name" $button }
    $current = (Control @('list')).sessions | Where-Object id -eq $pane.id
    if (!$current.active -or $current.connection_generation -ne $pane.connection_generation) {
        throw 'The tail changed the selected SSH connection'
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
