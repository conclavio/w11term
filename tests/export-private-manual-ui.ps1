param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')

$pages = Join-Path $env:W11TERM_DATA_DIR 'command-reference/pages/linux'
$null = [IO.Directory]::CreateDirectory($pages)
$libraryFile = Join-Path $pages 'fleet-review.md'
$saved = "# Saved fleet manual`n`n> Review the current fleet.`n"
[IO.File]::WriteAllText($libraryFile, $saved)
$outside = Join-Path $State 'export copy'
$exported = "$outside.md"
$inside = Join-Path $pages 'library-copy.md'
$accent = [char]0xe9
$draftText = "# Caf${accent} fleet draft`n`n> Review the draft route.`n"
$replacement = $draftText.Replace('draft route', 'revised route')

$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$dialogTitle = 'Export private page Markdown'
$exportLabel = 'Export Markdown' + [char]0x2026

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
        $field.GetCurrentPattern($value).Current.Value.Replace("`r`n", "`n").Replace("`r", "`n")
    }
}

function Set-Draft([string]$Text) {
    (Draft-Editor).GetCurrentPattern($value).SetValue($Text.Replace("`n", "`r"))
    $null = Wait-Until 'apply the unsaved draft' { (Draft) -ceq $Text }
}

function Export-Markdown([string]$Path, [switch]$Replace) {
    Click $exportLabel
    $dialog = Wait-OwnedDialog $dialogTitle
    if ($Path) {
        $null = Wait-Until 'enter the draft export destination' {
            [FixtureDialog]::SetFilename($dialog, $Path, 'fleet-review.md')
        }
        $buttonId = 1
    } else {
        $buttonId = 2
    }
    $null = Wait-Until 'submit the Save dialog choice' { [FixtureDialog]::ClickButton($dialog, $buttonId) }
    if ($Replace) {
        $confirm = Wait-Until 'the replacement confirmation is owned by the Save dialog' {
            $handle = [FixtureDialog]::FindOwnedDialog($app.Id, $dialog, 'Confirm Save As')
            if ($handle -ne [IntPtr]::Zero) { $handle }
        }
        $null = Wait-Until 'confirm replacement of the exported copy' {
            [FixtureDialog]::ClickButton($confirm, 6) -or [FixtureDialog]::ConfirmReplace($confirm)
        }
    }
    $null = Wait-Until 'the draft export picker has closed' { !(Find-OwnedDialog $dialogTitle) }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $pane = (Sessions | Where-Object label -ceq 'palette-ssh').id
    Wait-Text $pane 'private-manual-export-ready'
    Invoke-NativeMenuAction 'Tab menu' 'Command reference'
    $search = Wait-Until 'command reference editor' { Find-Element 'Search command reference' $edit }
    $search.GetCurrentPattern($value).SetValue('fleet-review')
    $null = Wait-Until 'the existing private manual is open' { Find-Element 'Saved fleet manual' }
    Click 'Edit private page'
    $null = Wait-Until 'the saved text seeds the editor' { (Draft) -ceq $saved }
    Set-Draft $draftText

    Export-Markdown ''
    $null = Wait-Until 'canceling Save As keeps the draft and writes no file' {
        (Draft) -ceq $draftText -and ![IO.File]::Exists($exported)
    }
    Export-Markdown $inside
    $null = Wait-Until 'the library destination is refused without changing the draft' {
        (Get-NativeTextLabels | Where-Object { $_.StartsWith('Choose a location outside the command reference library.') }) -and
            (Draft) -ceq $draftText -and ![IO.File]::Exists($inside)
    }
    Export-Markdown $outside
    $null = Wait-Until 'Save As adds the Markdown extension and exports the unsaved draft' {
        [IO.File]::Exists($exported) -and [IO.File]::ReadAllText($exported) -ceq $draftText -and
            (Get-NativeTextLabels | Where-Object { $_.StartsWith('Exported Markdown to ') })
    }
    Set-Draft $replacement
    Export-Markdown $exported -Replace
    $null = Wait-Until 'the confirmed replacement contains the latest snapshot in UTF-8' {
        $expected = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($replacement))
        [Convert]::ToBase64String([IO.File]::ReadAllBytes($exported)) -ceq $expected -and
            (Draft) -ceq $replacement
    }
    if ([IO.File]::ReadAllText($libraryFile) -cne $saved) { throw 'Export changed the saved manual' }
    Click 'Cancel private page edit'
    $null = Wait-Until 'the catalog still shows the original private manual' { Find-Element 'Saved fleet manual' }
} finally {
    Close-FixtureApp $app
}
