param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$fixtureProfile = @{ profiles = @{ 'Palette PowerShell' = @{
    backend = 'local'
    executable = 'powershell.exe'
    args = @('-NoProfile', '-NoLogo')
    cwd = [IO.Path]::GetPathRoot($State)
} } }
[IO.File]::WriteAllText($profiles, ($fixtureProfile | ConvertTo-Json -Depth 5))
$library = Join-Path $env:W11TERM_DATA_DIR 'saved-commands.json'
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$name = 'Reference recall sample'
$command = "Write-Output -NoE ('reference-' + (40 + 9) + 'reference-')"
$output = 'reference-49reference-'
$updated = "'reference-' + (80+9)"
$saveLabel = 'Save as command' + [char]0x2026
$fillLabel = 'Fill placeholders' + [char]0x2026
$dot = ' ' + [char]0xb7 + ' '
$template = "Write-Output {{[-NoE|-NoEnumerate]}} ('{{prefix}}' + (40 + {{offset}}) + '{{prefix}}')"
$custom = Join-Path $env:W11TERM_DATA_DIR 'command-reference/pages/common'
$null = [IO.Directory]::CreateDirectory($custom)
$tick = [char]0x60
[IO.File]::WriteAllText((Join-Path $custom 'example-values.md'),
    "# Example values`n`n> Format a short result.`n`n- Show the values:`n`n$tick$template$tick`n")
$byPreview = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'reference-command-preview')

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $button
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}

function Set-Field([string]$Name, [string]$Text) {
    $inputField = Wait-Until "edit field '$Name'" {
        $field = Find-Element $Name $edit
        if ($field -and $field.Current.IsEnabled) { $field }
    }
    $inputField.GetCurrentPattern($value).SetValue($Text)
    $null = Wait-Until "'$Name' contains the requested input" {
        (Find-Element $Name $edit).GetCurrentPattern($value).Current.Value -ceq $Text
    }
}

function Wait-Preview([string]$Text) {
    $null = Wait-Until 'the filled command preview matches' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $preview = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $byPreview)
        $preview -and $preview.Current.Name -ceq $Text
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--demo','--cols','100','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    Invoke-Button (Get-NativeMenuAction 'Tab menu' 'Command reference')
    Set-Field 'Search command reference' 'Get-ChildItem'
    $null = Wait-Until 'offline PowerShell manual is displayed' {
        (Find-Element 'Get-ChildItem') -and (Find-Element "Windows${dot}tldr-pages (CC BY 4.0)")
    }
    Click 'Edit example 1'
    $null = Wait-Until 'the example is editable even without a writable pane' {
        $field = Find-Element 'Edit command example' $edit
        $insert = Find-Element 'Insert into terminal' $button
        $field -and $field.GetCurrentPattern($value).Current.Value -ceq 'Get-ChildItem' -and
            $insert -and !$insert.Current.IsEnabled
    }
    Set-Field 'Search command reference' 'example-values'
    $null = Wait-Until 'private PowerShell template is displayed' { Find-Element 'Example values' }
    Click 'Edit example 1'
    Click $fillLabel
    Wait-Preview "Write-Output -NoEnumerate ('prefix' + (40 + offset) + 'prefix')"
    Set-Field 'Value for prefix' 'discarded'
    Wait-Preview "Write-Output -NoEnumerate ('discarded' + (40 + offset) + 'discarded')"
    Click 'Cancel placeholder values'
    $null = Wait-Until 'cancel keeps the original example' {
        (Find-Element 'Edit command example' $edit).GetCurrentPattern($value).Current.Value -ceq $template
    }
    Click $fillLabel
    Wait-Preview "Write-Output -NoEnumerate ('prefix' + (40 + offset) + 'prefix')"
    Set-Field 'Value for prefix' 'reference-'
    Set-Field 'Value for offset' '9'
    $dropdown = Wait-Until 'the short and long option choices' {
        Find-Element 'Option [-NoE|-NoEnumerate]' ([System.Windows.Automation.ControlType]::ComboBox)
    }
    $popup = $dropdown.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
    $popup.Expand()
    $option = Wait-Until 'the short option can be selected' {
        Find-Element '-NoE' ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $option.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
    Wait-Preview $command
    if ([IO.File]::Exists($library)) { throw 'Filling a preview wrote to Saved commands' }
    Click 'Apply values'
    $null = Wait-Until 'applying the preview returns an editable command with no writable shell' {
        $field = Find-Element 'Edit command example' $edit
        $insert = Find-Element 'Insert into terminal' $button
        $field -and $field.GetCurrentPattern($value).Current.Value -ceq $command -and
            $insert -and !$insert.Current.IsEnabled
    }
    Click $fillLabel
    Wait-Preview $command
    Click 'Cancel placeholder values'

    Click $saveLabel
    Set-Field 'Saved command name' $name
    Click 'Save to commands'
    $null = Wait-Until 'saved example is confirmed' { Find-Element "Saved command $name" }
    $saved = [IO.File]::ReadAllText($library)
    $entries = @($saved | ConvertFrom-Json)
    if ($entries.Count -ne 1 -or $entries[0].name -cne $name -or $entries[0].text -cne $command) {
        throw 'The saved library did not retain the exact edited line'
    }

    Set-Field 'Edit command example' $updated
    Click $saveLabel
    $lower = $name.ToLowerInvariant()
    Set-Field 'Saved command name' $lower
    Click 'Save to commands'
    $null = Wait-Until 'a duplicate name is rejected without discarding the draft' {
        (Find-Element "A command named $lower already exists.") -and
            (Find-Element 'Edit command example' $edit).GetCurrentPattern($value).Current.Value -ceq $updated
    }
    if ([IO.File]::ReadAllText($library) -cne $saved) { throw 'The rejected save replaced the original command' }
    Click 'Cancel saving command'
    Click 'Close command reference'

    $local = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $local
    Wait-Text $local 'PS '
    Press-Key 0x76
    $picker = Wait-Until 'the command palette is editable' { Find-Element 'Search commands' $edit }
    $picker.GetCurrentPattern($value).SetValue($name)
    Click "Insert command${dot}$name"
    $null = Wait-Until 'the palette has closed after inserting the command' {
        !(Find-Element 'Search commands' $edit)
    }
    try {
        Wait-Text $local $command
    } catch {
        throw "$_ PowerShell screen: $((Control @('get-text','--pane',"$local")).text)"
    }
    $before = (Control @('get-text','--pane',"$local")).text
    if ($before.Contains($output)) { throw 'Selecting a saved command submitted it before Enter' }
    # Send Enter to this pane after checking the recalled input. A posted key
    # can race the host's focus transfer when the palette closes.
    $null = Control @('send-text','--pane',"$local",'--no-paste','--base64','DQ==')
    Wait-Text $local $output
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
