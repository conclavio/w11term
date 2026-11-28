param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$catalog = Join-Path $env:W11TERM_DATA_DIR 'command-reference'
$archive = Join-Path $catalog 'removed-pages'
$command = 'fleet-route-archive-guide'
$target = Join-Path $catalog "pages/linux/$command.md"
$old = "# Older fleet archive`n`n> Review an earlier route.`n"
$new = "# Newer fleet archive`n`n> Review a recent route.`n"
$edited = "# Unsaved fleet notes`n`n> Preserve this draft on Back.`n"

function Write-Archive([string]$Folder, [string]$Name, [string]$Date, [string]$Text) {
    $directory = Join-Path $archive $Folder
    $null = [IO.Directory]::CreateDirectory($directory)
    $file = Join-Path $directory "$Name.md"
    [IO.File]::WriteAllText($file, $Text)
    [IO.Directory]::SetCreationTimeUtc($directory, [DateTimeOffset]::Parse($Date).UtcDateTime)
    $file
}

$oldFile = Write-Archive 'linux-first' $command '2026-01-01T03:04:05Z' $old
$newFile = Write-Archive 'linux-second' $command '2026-01-02T03:04:05Z' $new
$null = Write-Archive 'windows-third' $command '2026-01-03T03:04:05Z' "# Wrong platform`n"
$null = Write-Archive 'linux-fourth' 'another-command' '2026-01-04T03:04:05Z' "# Another command`n"

$edit = [System.Windows.Automation.ControlType]::Edit
$combo = [System.Windows.Automation.ControlType]::ComboBox
$value = [System.Windows.Automation.ValuePattern]::Pattern
$selection = [System.Windows.Automation.SelectionPattern]::Pattern
$browse = 'Archived versions' + [char]0x2026
$chooser = 'Archived private page versions'
$older = '2026-01-01 03:04:05.000 UTC'
$newer = '2026-01-02 03:04:05.000 UTC'

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

function Wait-Preview([string]$Heading, [string]$Version) {
    $null = Wait-Until "the selected archive displays '$Heading'" {
        $select = Find-Element $chooser $combo
        if ($select -and $select.Current.IsEnabled) {
            $choice = $select.GetCurrentPattern($selection).Current.GetSelection()
            $choice.Count -eq 1 -and $choice[0].Current.Name -ceq $Version -and
                (Get-NativeTextLabels | Where-Object { $_.StartsWith("# $Heading") })
        }
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $pane = (Sessions | Where-Object label -ceq 'palette-ssh').id
    Wait-Text $pane 'private-manual-recovery-ready'
    Invoke-NativeMenuAction 'Tab menu' 'Command reference'
    $search = Wait-Until 'command reference search' { Find-Element 'Search command reference' $edit }
    $search.GetCurrentPattern($value).SetValue($command)
    Click 'New private page'
    $null = Wait-Until 'the new manual editor shows its draft' { (Draft) -like "# $command*" }
    (Draft-Editor).GetCurrentPattern($value).SetValue($edited.Replace("`n","`r"))
    $null = Wait-Until 'the editor contains the unsaved draft' { (Draft) -ceq $edited }

    Click $browse
    Wait-Preview 'Newer fleet archive' $newer
    Select-NativeComboItem $chooser $older
    Wait-Preview 'Older fleet archive' $older
    if ([IO.File]::Exists($target)) { throw 'Preview published the manual' }
    Click 'Back to private page'
    $null = Wait-Until 'Back restores the unsaved draft' { (Draft) -ceq $edited }

    Click $browse
    Wait-Preview 'Newer fleet archive' $newer
    Select-NativeComboItem $chooser $older
    Wait-Preview 'Older fleet archive' $older
    Click 'Use archived draft'
    $null = Wait-Until 'the reviewed archive becomes the editable draft' { (Draft) -ceq $old }
    if ([IO.File]::Exists($target)) { throw 'Restoring a draft saved the manual before confirmation' }
    Click 'Save private page'
    $null = Wait-Until 'explicit Save publishes the selected archived version' {
        (Find-Element 'Older fleet archive') -and [IO.File]::Exists($target) -and [IO.File]::ReadAllText($target) -ceq $old
    }
    if ([IO.File]::ReadAllText($oldFile) -cne $old -or [IO.File]::ReadAllText($newFile) -cne $new) {
        throw 'Recovery changed the archived versions'
    }
} finally {
    Close-FixtureApp $app
}
