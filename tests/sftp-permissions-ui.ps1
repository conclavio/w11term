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
$editMode = 'Edit permissions' + [char]0x2026

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
function Set-Mode([string]$Old, [string]$New) {
    Click $editMode
    $inputField = Wait-Until "mode field shows $Old" {
        $item = Find-Element 'Permission mode (octal)' $edit
        if ($item -and $item.GetCurrentPattern($value).Current.Value -ceq $Old) { $item }
    }
    $inputField.GetCurrentPattern($value).SetValue($New)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'

    Click 'permissions.txt'
    Invoke-SftpEntryAction $properties
    Wait-Properties 'Permissions  0640'
    Set-Mode '0640' '0890'
    $null = Wait-Until 'invalid octal entry cannot be applied' {
        $apply = Find-Element 'Apply permissions' $button
        $apply -and !$apply.Current.IsEnabled
    }
    Click 'Cancel edit'
    Wait-Properties 'Permissions  0640'
    Set-Mode '0640' '0750'
    Click 'Apply permissions'
    $null = Wait-Until 'file mode acknowledgement' { Find-Element 'Permissions updated to 0750' }
    Wait-Properties 'Permissions  0750'
    Click 'Close properties'

    Click 'link-folder'
    Invoke-SftpEntryAction $properties
    Wait-Properties 'Type  Symbolic link'
    if (Find-Element $editMode $button) { throw 'Symbolic link showed a permission edit action' }
    Click 'Close properties'

    Click ('Find files' + [char]0x2026)
    $query = Wait-Until 'search query' { Find-Element 'Filename contains' $edit }
    $query.GetCurrentPattern($value).SetValue('child-folder')
    Click 'Search'
    Click 'Properties /shell directory/child-folder'
    Wait-Properties 'Type  Directory'
    Set-Mode '0640' '755'
    Click 'Apply permissions'
    $null = Wait-Until 'directory mode acknowledgement' { Find-Element 'Permissions updated to 0755' }
    Wait-Properties 'Permissions  0755'
} catch {
    throw "$($_.Exception.Message)`n$($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
