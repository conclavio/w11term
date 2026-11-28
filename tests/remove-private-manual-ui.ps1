param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$catalog = Join-Path $env:W11TERM_DATA_DIR 'command-reference'
$pages = Join-Path $catalog 'pages/linux'
$null = [IO.Directory]::CreateDirectory($pages)
$file = Join-Path $pages 'ip.md'
$archive = Join-Path $catalog 'removed-pages'
$original = "# Private network runbook`n`n> Review an address.`n"
$edited = "# Unsaved network notes`n`n> Preserve the active draft.`n"
$external = "# Externally revised network runbook`r`n`r`n> Review a route.`r`n"
[IO.File]::WriteAllText($file, $original)

$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$dot = ' ' + [char]0xb7 + ' '
$review = 'Review private page removal' + [char]0x2026

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

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $pane = (Sessions | Where-Object label -ceq 'palette-ssh').id
    Wait-Text $pane 'private-manual-removal-ready'
    Invoke-NativeMenuAction 'Tab menu' 'Command reference'
    $search = Wait-Until 'command reference search' { Find-Element 'Search command reference' $edit }
    $search.GetCurrentPattern($value).SetValue('ip')
    $null = Wait-Until 'the private manual overrides the offline page' {
        (Find-Element 'Private network runbook') -and (Find-Element "Linux${dot}Custom page")
    }
    Click 'Edit private page'
    $null = Wait-Until 'the saved text seeds the editor' { (Draft) -ceq $original }
    (Draft-Editor).GetCurrentPattern($value).SetValue($edited.Replace("`n", "`r"))
    $null = Wait-Until 'the active draft has unsaved edits' { (Draft) -ceq $edited }

    Click $review
    $null = Wait-Until 'removal review explains the fate of unsaved edits' {
        (Get-NativeTextLabels | Where-Object { $_.StartsWith('The saved Markdown will be archived.') }) -and
            (Find-Element 'Remove private page' $button)
    }
    if (![IO.File]::Exists($file) -or [IO.Directory]::Exists($archive)) { throw 'Review changed files before confirmation' }
    Click 'Back to private page'
    $null = Wait-Until 'returning from review preserves the draft' { (Draft) -ceq $edited }

    Click $review
    [IO.File]::WriteAllText($file, $external)
    Click 'Remove private page'
    $null = Wait-Until 'an external edit makes the removal stale' {
        (Get-NativeTextLabels | Where-Object { $_.StartsWith('The private page changed on disk.') }) -and
            (Draft) -ceq $edited
    }
    if ([IO.File]::ReadAllText($file) -cne $external -or [IO.Directory]::Exists($archive)) {
        throw 'A stale removal archived or changed the newer file'
    }

    Click 'Cancel private page edit'
    Click 'Edit private page'
    $null = Wait-Until 'reopening reads the new saved version' { (Draft) -ceq $external.Replace("`r`n", "`n") }
    Click $review
    Click 'Remove private page'
    $null = Wait-Until 'archiving exposes the bundled reference immediately' {
        (Find-Element "Linux${dot}tldr-pages (CC BY 4.0)") -and
            (Get-NativeTextLabels | Where-Object { $_.StartsWith('Archived private Markdown to ') }) -and
            !(Draft-Editor) -and ![IO.File]::Exists($file)
    }
    $backups = @(Get-ChildItem -LiteralPath $archive -Recurse -File -Filter 'ip.md')
    if ($backups.Count -ne 1 -or [IO.File]::ReadAllText($backups[0].FullName) -cne $external) {
        throw 'Archived Markdown does not match the reviewed on-disk version'
    }
} finally {
    Close-FixtureApp $app
}
