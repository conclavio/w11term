param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

[IO.File]::WriteAllText($profiles, '{"profiles":{"Palette PowerShell":{"backend":"local","executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}}}')
$library = Join-Path $env:W11TERM_DATA_DIR 'saved-commands.json'
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$name = 'Reference recall sample'
$command = "'reference-' + (40+9)"
$updated = "'reference-' + (80+9)"
$saveLabel = 'Save as command' + [char]0x2026
$dot = ' ' + [char]0xb7 + ' '

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $button
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}

function Set-Field([string]$Name, [string]$Text) {
    $inputField = Wait-Until "edit field '$Name'" { Find-Element $Name $edit }
    $inputField.SetFocus()
    $inputField = Wait-Until "focused edit field '$Name'" {
        $field = Find-Element $Name $edit -Visible
        if ($field -and $field.Current.HasKeyboardFocus) { $field }
    }
    $inputField.GetCurrentPattern($value).SetValue($Text)
    $null = Wait-Until "'$Name' contains the requested input" {
        (Find-Element $Name $edit).GetCurrentPattern($value).Current.Value -ceq $Text
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    Run-Palette 'Command reference'
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
    Set-Field 'Edit command example' $command
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
    $picker = Open-Palette $name
    $picker.GetCurrentPattern($value).SetValue($name)
    Click "Insert command${dot}$name"
    $null = Wait-Until 'the palette has closed after inserting the command' {
        !(Find-Element 'Search commands' $edit)
    }
    Wait-Text $local $command
    $before = (Control @('get-text','--pane',"$local")).text
    if ($before.Contains('reference-49')) { throw 'Selecting a saved command submitted it before Enter' }
    # Send Enter to this pane after checking the recalled input. A posted key
    # can race the host's focus transfer when the palette closes.
    $null = Control @('send-text','--pane',"$local",'--no-paste','--base64','DQ==')
    Wait-Text $local 'reference-49'
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
