param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$pages = Join-Path $env:W11TERM_DATA_DIR 'command-reference/pages/linux'
$null = [IO.Directory]::CreateDirectory($pages)
$libraryFile = Join-Path $pages 'fleet-review.md'
$old = "# Existing fleet runbook`n`n> Review the current fleet.`n"
[IO.File]::WriteAllText($libraryFile, $old)
$sourceFile = Join-Path $State 'Team runbook.markdown'
$badFile = Join-Path $State 'Incomplete runbook.md'
$title = 'Caf' + [char]0xe9 + ' fleet runbook'
$imported = "# $title`n`n> Inspect a reported route.`n"
[IO.File]::WriteAllText($sourceFile, $imported.Replace("`n", "`r`n"), [Text.UTF8Encoding]::new($true))
[IO.File]::WriteAllText($badFile, 'Missing the Markdown title')
$sourceBytes = [Convert]::ToBase64String([IO.File]::ReadAllBytes($sourceFile))
$draftText = "# Unsaved fleet notes`n`n> Keep the active draft.`n"

$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$dialogTitle = 'Open private page Markdown'
$importLabel = 'Import Markdown' + [char]0x2026

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $button
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}

function Draft-Editor { Find-Element 'Private page Markdown' $edit }

function Draft {
    $field = Draft-Editor
    if ($field -and $field.Current.IsEnabled) {
        $text = $field.GetCurrentPattern($value).Current.Value
        $text.Replace("`r`n", "`n").Replace("`r", "`n")
    }
}

function Import-Markdown([string]$Path) {
    Click $importLabel
    $dialog = Wait-OwnedDialog $dialogTitle
    if ($Path) {
        $null = Wait-Until 'enter the selected Markdown filename' {
            [FixtureDialog]::SetFilename($dialog, $Path, '')
        }
        $buttonId = 1
    } else {
        $buttonId = 2
    }
    $null = Wait-Until 'submit the file picker choice' { [FixtureDialog]::ClickButton($dialog, $buttonId) }
    $null = Wait-Until 'the Markdown picker has closed' { !(Find-OwnedDialog $dialogTitle) }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $pane = (Sessions | Where-Object label -ceq 'palette-ssh').id
    Wait-Text $pane 'private-manual-ready'
    Invoke-NativeMenuAction 'Tab menu' 'Command reference'
    $search = Wait-Until 'command reference editor' { Find-Element 'Search command reference' $edit }
    $search.GetCurrentPattern($value).SetValue('fleet-review')
    $null = Wait-Until 'existing private manual is open' { Find-Element 'Existing fleet runbook' }
    Click 'Edit private page'
    $null = Wait-Until 'the saved text seeds the editor' { (Draft) -ceq $old }
    (Draft-Editor).GetCurrentPattern($value).SetValue($draftText.Replace("`n","`r"))
    $null = Wait-Until 'the editor has an unsaved draft' { (Draft) -ceq $draftText }

    Import-Markdown ''
    $null = Wait-Until 'canceling the picker leaves the draft intact' { (Draft) -ceq $draftText }
    Import-Markdown $badFile
    $null = Wait-Until 'invalid Markdown leaves the draft intact' {
        (Find-Element 'Start the private page with a Markdown heading such as # command') -and
            (Draft) -ceq $draftText
    }
    Import-Markdown $sourceFile
    $null = Wait-Until 'valid Markdown becomes an editable multiline draft' { (Draft) -ceq $imported }
    if ([IO.File]::ReadAllText($libraryFile) -cne $old) { throw 'Import wrote into the library before Save' }
    Click 'Cancel private page edit'
    $null = Wait-Until 'canceling the editor keeps the existing manual' { Find-Element 'Existing fleet runbook' }

    Click 'Edit private page'
    $null = Wait-Until 'the editor reopened the original saved manual' { (Draft) -ceq $old }
    Import-Markdown $sourceFile
    $null = Wait-Until 'review the imported draft again' { (Draft) -ceq $imported }
    Click 'Save private page'
    $null = Wait-Until 'Save publishes the reviewed Markdown to the original command and platform' {
        (Find-Element $title) -and [IO.File]::ReadAllText($libraryFile) -ceq $imported
    }
    if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($sourceFile)) -cne $sourceBytes) {
        throw 'Saving the draft changed the selected source file'
    }
} finally {
    Close-FixtureApp $app
}
