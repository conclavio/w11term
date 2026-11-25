[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$buttons = [System.Windows.Automation.ControlType]::Button
$edits = [System.Windows.Automation.ControlType]::Edit
$insert = 'Insert command ' + [char]0xb7 + ' Status check'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled button '$Name'" {
        $item = Find-Element $Name $buttons
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Set-Field([string]$Name, [string]$Value) {
    $edit = Wait-Until "edit field '$Name'" { Find-Element $Name $edits }
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value)
}

function Select-Pane([string]$Label, [int]$Pane) {
    Picker-Query $Label
    Press-Key 13
    Wait-Focus $Pane
}

function Insert-Command {
    $search = Open-Palette 'Status check'
    $search.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('status check')
    $null = Wait-Until 'saved command result' {
        $row = Find-Element $insert $buttons
        if ($row -and $row.Current.IsEnabled) { $row }
    }
    Press-Key 13
    $null = Wait-Until 'command palette closes' { !(Find-Element 'Search commands' $edits) }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','command-fixture-ready')
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell neighbor' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')

    Run-Palette 'Settings'
    Click 'Saved commands'
    Click 'Add command'
    Set-Field 'Command name' 'Status check'
    Set-Field 'Command text' "'saved-' + (40+6)"
    Click 'Save command'
    Click 'Edit command Status check'
    Set-Field 'Command text' ''
    $null = Wait-Until 'editor sees cleared command text' {
        $save = Find-Element 'Save command' $buttons
        $save -and !$save.Current.IsEnabled
    }
    Set-Field 'Command text' "'saved-' + (40+7)"
    Click 'Save command'
    $null = Wait-Until 'edited command appears in list' {
        Find-Element "'saved-' + (40+7)" ([System.Windows.Automation.ControlType]::Text)
    }
    Click 'Back to settings'

    Select-Pane 'Palette PowerShell' $local
    Insert-Command
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','10','saved-')
    if ((Control @('get-text','--pane',"$local")).text -match 'saved-47') { throw 'Palette selection executed the PowerShell command' }
    Press-Key 13
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','10','saved-47')

    Select-Pane 'palette-ssh' $remote
    Insert-Command
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','saved-')

    Run-Palette 'Settings'
    Click 'Saved commands'
    Click 'Edit command Status check'
    Click 'Remove command'
    $null = Wait-Until 'command removed from Settings' {
        (Find-Element 'Add command' $buttons) -and !(Find-Element 'Edit command Status check' $buttons)
    }
    Select-Pane 'palette-ssh' $remote
    $search = Open-Palette 'Status check'
    $search.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('status check')
    $null = Wait-Until 'removed command absent from palette' { Find-Element 'No matching commands' }
    if (Find-Element $insert $buttons) { throw 'Removed command still appears in palette' }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
