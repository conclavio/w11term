[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$folder = [IO.File]::ReadAllText((Join-Path $State 'fixture-folder'))
$snow = [char]0x96ea
$name = "pilot $snow.ini"
$copy = "draft $snow.ini"
$file = Join-Path $folder $name
$preview = 'Preview text' + [char]0x2026
$saveAs = 'Save as' + [char]0x2026
$button = [System.Windows.Automation.ControlType]::Button
$getValue = [System.Windows.Automation.ValuePattern]::Pattern

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function By-Id([string]$Id) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $condition = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
    $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
}
function Wait-Draft([string]$Expected, [string]$Step) {
    try {
        Wait-Until 'editable draft with the expected contents' {
            $field = Find-Element 'File text' ([System.Windows.Automation.ControlType]::Edit)
            if ($field -and $field.Current.IsEnabled -and $field.Current.AutomationId -eq 'sftp-editor-text') {
                $value = $field.GetCurrentPattern($getValue)
                if (!$value.Current.IsReadOnly -and
                    $value.Current.Value.Replace("`r`n", "`n").Replace("`r", "`n") -ceq $Expected) { $value }
            }
        }
    } catch {
        $field = Find-Element 'File text'
        $observed = if ($field) { $field.GetCurrentPattern($getValue).Current.Value } else { '<no File text control>' }
        $text = $null
        $range = if ($field -and $field.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$text)) { $text.DocumentRange.GetText(-1) }
        $details = [pscustomobject]@{ Expected = $Expected; Actual = $observed; TextRange = $range; ControlType = $field.Current.ControlType.ProgrammaticName; Name = $field.Current.Name; Open = [bool](Find-Element 'Close editor' $button); Draft = [bool](Find-Element 'Resume draft' $button) }
        throw "Editor draft did not match during $Step. $($details | ConvertTo-Json -Compress)"
    }
}
function Open-Editor {
    Click $name
    Click $preview
    Click 'Edit'
}
function Replace-Word([string]$Old, [string]$New) {
    $edit = Find-Element 'File text' ([System.Windows.Automation.ControlType]::Edit)
    $edit.SetFocus()
    $document = $edit.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange
    $word = $document.FindText($Old, $false, $false)
    if (!$word) { throw "Could not select '$Old' in the editable document" }
    $word.Select()
    $target = Wait-Until 'focused editor input HWND' {
        $hwnd = [FixtureMessages]::FocusedWindow($window, $app.Id)
        if ($hwnd -ne [IntPtr]::Zero) { $hwnd }
    }
    foreach ($ch in $New.ToCharArray()) {
        if (![FixtureMessages]::PostKey($target, 0x102, [uint32]$ch)) { throw 'Could not type into the editor' }
    }
}
function Check-File([string]$Path, [string]$Expected) {
    # Check bytes. ReadAllText consumes the BOM and cannot detect its removal.
    $actual = [Convert]::ToBase64String([IO.File]::ReadAllBytes($Path))
    $wanted = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Expected))
    if ($actual -cne $wanted) { throw "Unexpected bytes in $Path" }
}
function Type-Line([string]$Line) {
    foreach ($ch in $Line.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    Press-Key 13
}

$original = "pilot=one`nspeed=slow`nlabel=$snow"
$saved = $original.Replace('slow','fast')
$draft = $saved.Replace('one','two')
$originalBytes = ([char]0xfeff).ToString() + "pilot=one`r`nspeed=slow`nlabel=$snow"
$savedBytes = $originalBytes.Replace('slow','fast')
$externalBytes = $savedBytes.Replace('fast','peer')

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','160','--rows','40') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','src/trace.rs')
    $ssh = ((Control @('list')).sessions | Where-Object id -eq $remote)
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'neighboring PowerShell' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','PS ')
    Type-Line '$editorPid = $PID; "ps-edit-" + "ready"'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','ps-edit-ready')
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    Run-Palette 'Open files here'

    Open-Editor
    $null = Wait-Draft $original 'initial load'
    Replace-Word 'slow' 'fast'
    $null = Wait-Draft $saved 'first edit'
    Click 'Close editor'
    $null = Wait-Until 'closed editor with a retained draft' { (Find-Element 'Resume draft' $button) -and !(By-Id 'sftp-editor-text') }
    Check-File $file $originalBytes
    Click 'Resume draft'
    $null = Wait-Draft $saved 'resuming draft'
    Click 'Save'
    $null = Wait-Until 'successful save in the original folder' { (Find-Element "Saved $name") -and !(By-Id 'sftp-editor-text') }
    Check-File $file $savedBytes

    Open-Editor
    $null = Wait-Draft $saved 'opening saved file'
    Replace-Word 'one' 'two'
    $null = Wait-Draft $draft 'second edit'
    # A second writer changes the bytes without changing the size or timestamp.
    $mtime = [IO.File]::GetLastWriteTimeUtc($file)
    [IO.File]::WriteAllBytes($file, [Text.Encoding]::UTF8.GetBytes($externalBytes))
    [IO.File]::SetLastWriteTimeUtc($file, $mtime)
    Click 'Save'
    $null = Wait-Until 'conflict reported with the draft still open' {
        $status = By-Id 'sftp-editor-status'
        $status -and $status.Current.Name.Contains('remote file changed')
    }
    $null = Wait-Draft $draft 'save conflict'
    Check-File $file $externalBytes
    Click $saveAs
    $field = Wait-Until 'new remote filename' { By-Id 'sftp-editor-filename' }
    $field.GetCurrentPattern($getValue).SetValue($copy)
    Click 'Create file'
    $null = Wait-Until 'new file in refreshed listing' { (Find-Element "Created $copy") -and (Find-Element $copy $button) }
    Check-File $file $externalBytes
    Check-File (Join-Path $folder $copy) ($savedBytes.Replace('one','two'))

    Run-Palette 'SFTP files'
    $null = Wait-Until 'drawer closed' { !(Find-Element $preview $button) }
    foreach ($ch in 'after-editor'.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','after-editor')
    $session = (Control @('list')).sessions | Where-Object id -eq $remote
    if (!$session.active -or $session.connection_generation -ne $ssh.connection_generation -or $session.tab_id -ne $ssh.tab_id) {
        throw 'Editor changed the SSH pane or connection'
    }
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $localPane
    Type-Line 'if ($editorPid -eq $PID) { "ps-edit-" + "still-alive" }'
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','ps-edit-still-alive')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
