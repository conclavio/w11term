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
    $field.SetFocus()
    $null = Wait-Until 'delimiter textbox focused' { $field.Current.HasKeyboardFocus }
    return $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
}
function Close-Editor([string]$Choice, [string]$ExpectedDraft) {
    # The UIA text assignment and the app's draft update are separate events.
    # Check the complete native value after leaving the field, before invoking Save/Cancel.
    $commit = Wait-Until "enabled '$Choice' in delimiter editor" {
        $entry = Find-Element $Choice $button
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    }
    $commit.SetFocus()
    $null = Wait-Until "delimiter draft retained on leaving the field for '$Choice'" {
        $field = Find-Element 'Delimiter characters' ([System.Windows.Automation.ControlType]::Edit)
        $field -and !$field.Current.HasKeyboardFocus -and
            $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq $ExpectedDraft
    }
    Invoke-Button $commit
    $null = Wait-Until 'return from the delimiter editor to Settings' {
        !(Find-Element 'Delimiter characters' ([System.Windows.Automation.ControlType]::Edit)) -and (Find-Element $action $button)
    }
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $pane
}
function Saved-Delimiters {
    return ([IO.File]::ReadAllText($env:W11TERM_SETTINGS_PATH) | ConvertFrom-Json).interaction.wordDelimiters
}
function Check-Word([string]$Needle, [string]$Expected) {
    # An existing UIA range keeps its original delimiters. Acquire the current
    # document on each attempt until the running pane exposes the new boundary.
    $text = Wait-Until "native SSH word '$Needle' expands to '$Expected'" {
        $document = Find-Element ('Terminal ' + [char]0xb7 + ' palette-ssh') ([System.Windows.Automation.ControlType]::Document)
        if (!$document) { return }
        $provider = $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
        $word = $provider.DocumentRange.FindText($Needle, $false, $false)
        if ($word) {
            $word.ExpandToEnclosingUnit([System.Windows.Automation.Text.TextUnit]::Word)
            if ($word.GetText(-1) -ceq $Expected) { $provider }
        }
    }
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
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','word-fixture-ready')
    $generation = ((Control @('list')).sessions | Where-Object id -eq $pane).connection_generation
    Check-Word 'alpha' 'alpha'
    $editor = Edit-Delimiters
    $default = $editor.Current.Value
    if (!$default.Contains('/') -or !$default.Contains(':')) { throw 'Fixture needs slash and colon in the factory settings' }
    $custom = $default.Replace('/','')
    $editor.SetValue($custom)
    Close-Editor 'Save' $custom
    Check-Word 'lpha' 'scope/srv/alpha/bin'
    $saved = Saved-Delimiters
    if ($saved -cne $custom) { throw "Saved delimiters '$saved' differ from the edited value '$custom'" }

    $editor = Edit-Delimiters
    if ($editor.Current.Value -cne $custom) { throw 'Editor lost the saved delimiters' }
    $editor.SetValue('!')
    Close-Editor 'Cancel' '!'
    if ((Saved-Delimiters) -cne $custom) { throw 'Cancelled edit changed settings on disk' }
    Check-Word 'pha' 'scope/srv/alpha/bin'

    $editor = Edit-Delimiters
    Click 'Restore default delimiters'
    $null = Wait-Until 'default draft in text box' { $editor.Current.Value -ceq $default }
    if ((Saved-Delimiters) -cne $custom) { throw 'Restoring the draft persisted it before Save' }
    Close-Editor 'Save' $default
    Check-Word 'ha' 'alpha'
    $saved = Saved-Delimiters
    if ($saved -cne $default) { throw "Saved delimiters '$saved' differ from restored default '$default'" }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
