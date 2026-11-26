param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$source = 'Palette PowerShell'
$copy = "$source copy"
$installed = 'Windows PowerShell'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
function Catalog {
    # The store atomically replaces profiles.json. A concurrent test read must allow its rename.
    $sharing = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
    $stream = [IO.File]::Open($profiles,[IO.FileMode]::Open,[IO.FileAccess]::Read,$sharing)
    $reader = [IO.StreamReader]::new($stream)
    try { ($reader.ReadToEnd() | ConvertFrom-Json).profiles } finally { $reader.Dispose() }
}
function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled button '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Text-Value([string]$Name) {
    $field = Wait-Until "field '$Name'" { Find-Element $Name $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
}
function Set-Field([string]$Name, [string]$Text) {
    $field = Wait-Until "editable '$Name'" { Find-Element $Name $edit }
    $field.SetFocus()
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Text)
    (Find-Element 'Profile name' $edit).SetFocus()
    $null = Wait-Until "'$Name' committed" {
        !$field.Current.HasKeyboardFocus -and (Text-Value $Name).Current.Value -ceq $Text
    }
}
function Send-Line([int]$Pane, [string]$Text) {
    $wire = [Text.Encoding]::UTF8.GetBytes($Text + [char]13)
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',[Convert]::ToBase64String($wire))
}
# The copied shell publishes its identity when its own startup command runs.
$fixtureCatalog = Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json
$fixtureCatalog.profiles.$source.args += @('-NoExit','-Command','$global:profileFixture = $PID; "copy-shell-" + $PID')
[IO.File]::WriteAllText($profiles, ($fixtureCatalog | ConvertTo-Json -Depth 20))
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')

    Run-Palette 'Settings'
    Click "Edit local profile $source"
    $saved = (Catalog).$source
    Set-Field 'Executable' 'unsaved-command.exe'
    Click ('Duplicate saved profile' + [char]0x2026)
    $null = Wait-Until 'fresh copy with saved executable' {
        (Text-Value 'Profile name').Current.Value -ceq $copy -and
        (Text-Value 'Executable').Current.Value -ceq $saved.executable
    }
    Set-Field 'Scrollback lines' '27'
    Click 'Save copy'
    # Verify the on-disk document only after Settings reports that the
    # replacement succeeded. An open reader can race Windows' atomic rename.
    $null = Wait-Until 'saved copy appears in Settings' { Find-Element "Edit local profile $copy" $button }
    $entries = Catalog
    if ($entries.$source.executable -cne $saved.executable -or $entries.$source.scrollback_lines -ne $saved.scrollback_lines -or
        $entries.$copy.executable -cne $saved.executable -or ($entries.$copy.args -join '|') -cne ($saved.args -join '|') -or
        $entries.$copy.scrollback_lines -ne 27) {
        throw 'Duplicating the saved profile changed its source or failed to save the copy'
    }
    $pane = (Control @('open','--profile',$copy,'--activate')).pane
    Wait-Focus $pane
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','PS ')
    $initial = (Control @('wait-for-text','--pane',"$pane",'--timeout','15','--regex','copy-shell-[0-9]+')).text
    $childPid = [regex]::Match($initial,'copy-shell-([0-9]+)').Groups[1].Value
    if (!$childPid) { throw 'Missing PowerShell process ID' }

    Run-Palette 'Settings'
    Click "Edit local profile $copy"
    Click ('Delete profile' + [char]0x2026)
    Click 'Keep profile'
    $null = Wait-Until 'cancellation restores editor' { Find-Element ('Delete profile' + [char]0x2026) $button }
    if ((Catalog).$copy.scrollback_lines -ne 27) { throw 'Cancel changed the saved copy' }
    Click ('Delete profile' + [char]0x2026)
    Click "Delete saved profile $copy"
    $null = Wait-Until 'deleted copy disappears from Settings' {
        (Find-Element "Edit local profile $source" $button) -and !(Find-Element "Edit local profile $copy" $button)
    }
    if ((Catalog).$copy) { throw 'Deleted copy still exists in the saved catalog' }
    Send-Line $pane '"still-running-" + $profileFixture + "-" + $PID'
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15',"still-running-$childPid-$childPid")

    Click "Edit local profile $installed"
    Set-Field 'Scrollback lines' '11'
    Click 'Save profile'
    $null = Wait-Until 'overridden profile appears in Settings' { Find-Element "Edit local profile $installed" $button }
    if ((Catalog).$installed.scrollback_lines -ne 11) { throw 'The installed override was not saved' }
    Click "Edit local profile $installed"
    Click ('Restore defaults' + [char]0x2026)
    Click "Restore defaults $installed"
    $null = Wait-Until 'installed shell rediscovered' { Find-Element "Edit local profile $installed" $button }
    if ((Catalog).$installed) { throw 'Restoring defaults left the installed override in the catalog' }
    Click "Edit local profile $installed"
    if ((Text-Value 'Scrollback lines').Current.Value -cne '10000') { throw 'Installed profile kept the override' }
    foreach ($action in 'Delete profile','Restore defaults') {
        if (Find-Element ($action + [char]0x2026) $button) { throw "Pristine installed shell offered '$action'" }
    }
    $null = Control @('send-text','--pane',"$remote",'--no-paste','profile-actions-ok')
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','profile-actions-ok')
} catch {
    $failure = $_
    if ($window -and !$app.HasExited) {
        $screen = if ($pane) { (Control @('get-text','--pane',"$pane")).text } else { '(not launched)' }
        $catalogEntries = Catalog
        $names = $catalogEntries.PSObject.Properties.Name -join ', '
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,[System.Windows.Automation.ControlType]::Text)
        $labels = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$condition) | ForEach-Object { $_.Current.Name }) -join ' / '
        throw "$failure at $($failure.ScriptStackTrace). Catalog: $names. Copy limit: $($catalogEntries.$copy.scrollback_lines). Page: $labels. Local pane: $screen"
    }
    throw
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
