[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$parent = '/shell directory'
$copy = 'Copy to folder' + [char]0x2026

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    Click 'archive/'
    $null = Wait-Until 'folders do not offer the regular-file copy action' {
        $action = Find-Element $copy $button
        $action -and !$action.Current.IsEnabled
    }
    Click 'copy-first.bin'
    Click $copy
    $null = Wait-Until 'the copy dialog shows the original file and destination editor' {
        (Find-Element 'Remote copy source') -and (Find-Element 'Destination folder' $edit)
    }
    Click ('Browse folders' + [char]0x2026)
    Click 'Browse folder archive'
    Click 'Use this folder'
    $null = Wait-Until 'the chosen folder is ready to review' {
        $destination = Find-Element 'Destination folder' $edit
        $destination -and $destination.GetCurrentPattern($value).Current.Value -ceq "$parent/archive"
    }
    Click 'Copy'
    $null = Wait-Until 'copy completes while keeping the original listed' {
        (Find-Element "Copied copy-first.bin to $parent/archive") -and (Find-Element 'copy-first.bin' $button)
    }

    Click ('Find files' + [char]0x2026)
    $query = Wait-Until 'recursive filename query' {
        $field = Find-Element 'Filename contains' $edit
        if ($field -and $field.Current.HasKeyboardFocus) { $field }
    }
    $query.GetCurrentPattern($value).SetValue('copy-nested.bin')
    Click 'Search'
    Click "Copy to folder $parent/sub/copy-nested.bin"
    $destination = Wait-Until 'copy from search shows the destination editor' {
        $field = Find-Element 'Destination folder' $edit
        if ($field -and (Find-Element 'Remote copy source')) { $field }
    }
    $destination.GetCurrentPattern($value).SetValue($parent)
    Click 'Copy'
    $null = Wait-Until 'the search refresh finds the original and its copy' {
        (Find-Element "Copied copy-nested.bin to $parent. 2 matches in 3 folders") -and
            (Find-Element "Reveal $parent/sub/copy-nested.bin" $button) -and
            (Find-Element "Reveal $parent/copy-nested.bin" $button)
    }
    Click 'Back to files'
    $null = Wait-Until 'the cached listing contains the copy' { Find-Element 'copy-nested.bin' $button }
} catch {
    throw "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
