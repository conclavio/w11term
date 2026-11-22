[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_UI_SMOKE = '1'
$env:W11TERM_WORD_SELECTION_SMOKE = $State
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$menu = [System.Windows.Automation.ControlType]::MenuItem
$action = 'Word selection delimiters' + [char]0x2026
$utf8 = [Text.UTF8Encoding]::new($false)
# Preserve the machine clipboard while exercising the real selection handler.
$seed = [IO.File]::ReadAllText($env:W11TERM_SETTINGS_PATH) | ConvertFrom-Json
$seed | Add-Member NoteProperty interaction ([pscustomobject]@{copyOnSelect=$false})
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, ($seed | ConvertTo-Json -Depth 5), $utf8)

function Click([string]$Name, [System.Windows.Automation.ControlType]$Type = $button) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $entry = Find-Element $Name $Type
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
}
function Edit-Delimiters {
    Click 'Tab menu'
    Click 'Settings' $menu
    Click $action
    $field = Wait-Until 'delimiter editor' { Find-Element 'Delimiter characters' ([System.Windows.Automation.ControlType]::Edit) }
    return $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
}
function Close-Editor([string]$Choice) {
    Click $Choice
    $null = Wait-Until 'return to Settings' { Find-Element $action $button }
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $pane
}
function Saved-Delimiters {
    return ([IO.File]::ReadAllText($env:W11TERM_SETTINGS_PATH) | ConvertFrom-Json).interaction.wordDelimiters
}
function Check-Word([string]$Needle, [string]$Expected) {
    $document = Wait-Until 'native SSH terminal text provider' {
        Find-Element ('Terminal ' + [char]0xb7 + ' palette-ssh') ([System.Windows.Automation.ControlType]::Document)
    }
    $text = $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
    $word = $text.DocumentRange.FindText($Needle, $false, $false)
    if (!$word) { throw "Fixture text '$Needle' missing from the terminal" }
    $word.ExpandToEnclosingUnit([System.Windows.Automation.Text.TextUnit]::Word)
    if ($word.GetText(-1) -cne $Expected) { throw "UIA Word gave '$($word.GetText(-1))', expected '$Expected'" }
    $request = Join-Path $State 'double-click'
    $pending = Join-Path $State 'pending-click'
    [IO.File]::WriteAllText($pending, $Needle, $utf8)
    [IO.File]::Move($pending, $request)
    $null = Wait-Until "completed double-click selects '$Expected'" {
        $selection = @($text.GetSelection())
        !(Test-Path -LiteralPath $request) -and $selection.Count -eq 1 -and $selection[0].GetText(-1) -ceq $Expected
    }
    $session = (Control @('list')).sessions | Where-Object id -eq $pane
    if (!$session.active -or $session.connection_generation -ne $generation) { throw 'Editing word delimiters replaced or deactivated the SSH pane' }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','word-fixture-ready')
    $generation = ((Control @('list')).sessions | Where-Object id -eq $pane).connection_generation
    Check-Word 'alpha' 'alpha'
    $editor = Edit-Delimiters
    $default = $editor.Current.Value
    if (!$default.Contains('/') -or !$default.Contains(':')) { throw 'Fixture needs slash and colon in the factory settings' }
    $custom = $default.Replace('/','')
    $editor.SetValue($custom)
    Close-Editor 'Save'
    $null = Wait-Until 'saved slash removal on disk' { (Saved-Delimiters) -ceq $custom }
    Check-Word 'lpha' 'scope/srv/alpha/bin'

    $editor = Edit-Delimiters
    if ($editor.Current.Value -cne $custom) { throw 'Editor lost the saved delimiters' }
    $editor.SetValue('!')
    Close-Editor 'Cancel'
    if ((Saved-Delimiters) -cne $custom) { throw 'Cancelled edit changed settings on disk' }
    Check-Word 'pha' 'scope/srv/alpha/bin'

    $editor = Edit-Delimiters
    Click 'Restore default delimiters'
    $null = Wait-Until 'default draft in text box' { $editor.Current.Value -ceq $default }
    if ((Saved-Delimiters) -cne $custom) { throw 'Restoring the draft persisted it before Save' }
    Close-Editor 'Save'
    $null = Wait-Until 'restored default saved on disk' { (Saved-Delimiters) -ceq $default }
    Check-Word 'ha' 'alpha'
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
