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
$text = [System.Windows.Automation.TextPattern]::Pattern
$properties = 'Properties' + [char]0x2026
$parent = '/shell directory'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Wait-Properties([string]$Summary) {
    $null = Wait-Until "properties show '$Summary'" {
        $item = Find-Element 'File properties'
        $item -and $item.GetCurrentPattern($text).DocumentRange.GetText(-1).Contains($Summary)
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'

    Click 'broken-link'
    Click $properties
    Wait-Properties 'Link target  missing'
    Click 'Open link target'
    $null = Wait-Until 'broken target reports an error in Properties' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $statusId = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'sftp-preview-status')
        $status = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $statusId)
        $status -and $status.Current.Name.StartsWith('Could not open link target.')
    }
    Wait-Properties "Path  $parent/broken-link"
    Click 'Close properties'

    Click 'file-link'
    Click $properties
    Wait-Properties 'Link target  objects/payload [file].txt'
    Click 'Open link target'
    $null = Wait-Until 'file target appears and Properties is closed' {
        (Find-Element 'payload [file].txt' $button) -and !(Find-Element 'Close properties' $button)
    }
    # The target should be selected already.
    Click $properties
    Wait-Properties "Path  $parent/objects/payload [file].txt"
    Click 'Close properties'
    Click 'Back'
    $null = Wait-Until 'Back returns to the source folder' { Find-Element 'file-link' $button }

    Click ('Find files' + [char]0x2026)
    $query = Wait-Until 'search query' { Find-Element 'Filename contains' $edit }
    $query.GetCurrentPattern($value).SetValue('directory-link')
    Click 'Search'
    Click "Properties $parent/directory-link"
    Click 'Open link target'
    $null = Wait-Until 'directory target shows its contents and a route back to search' {
        (Find-Element 'marker.txt' $button) -and (Find-Element 'Back to search results' $button)
    }
    Click 'Back to search results'
    $null = Wait-Until 'search query and link result restored' {
        $field = Find-Element 'Filename contains' $edit
        $field -and $field.GetCurrentPattern($value).Current.Value -ceq 'directory-link' -and
            (Find-Element "Properties $parent/directory-link" $button)
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
