[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$verifyClipboard = $env:GITHUB_ACTIONS -eq 'true' -and $env:RUNNER_ENVIRONMENT -eq 'github-hosted'
if ($verifyClipboard) { . (Join-Path $PSScriptRoot 'support/clipboard.ps1') }
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_EXPORT_SMOKE_DIR = $State
$profiles = Join-Path $State 'profiles.json'
$word = '[' + [char]0xc5 + 'ngstr' + [char]0xf6 + 'm]'
$command = 'deploy ' + ('x' * 128) + ' ' + $word
$output = 'archived deploy output ' + $word + ' ' + ('y' * 90) + "`r`n`r`nsecond line`r`n"
$multiline = "echo first`n  echo second"
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$dot = ' ' + [char]0xb7 + ' '
$saveOutput = 'Save output as text' + [char]0x2026
function Click([string]$Label) {
    Invoke-Button (Wait-Until "enabled '$Label'" {
        $b = Find-Element $Label $button
        if ($b -and $b.Current.IsEnabled) { $b }
    })
}
function Search([string]$Text) {
    $field = Wait-Until 'command search field' { Find-Element 'Search command history' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Text)
}
function Find-OutputDialog {
    foreach ($handle in [FixtureDialog]::Windows($app.Id)) {
        if ([FixtureDialog]::GetWindow($handle, 4) -ne $window) { continue }
        $dialog = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        if ($dialog.Current.ClassName -eq '#32770' -and $dialog.Current.Name -eq 'Save command output as text') {
            return $handle
        }
    }
}
function Pick-Output([string]$Path, [int]$ButtonId, [string]$Suggestion) {
    Click $saveOutput
    $dialog = Wait-Until 'save dialog for the selected command' { Find-OutputDialog }
    $null = Wait-Until 'set the output filename' { [FixtureDialog]::SetFilename($dialog, $Path, $Suggestion) }
    $null = Wait-Until 'close the output save dialog' { [FixtureDialog]::ClickButton($dialog, $ButtonId) }
    $null = Wait-Until 'the output dialog has closed' { !(Find-OutputDialog) }
}
function Status-Box([string]$Expected) {
    Wait-Until "command status selector showing '$Expected'" {
        $box = Find-Element 'Command status' ([System.Windows.Automation.ControlType]::ComboBox)
        if ($box) {
            $items = $box.GetCurrentPattern([System.Windows.Automation.SelectionPattern]::Pattern).Current.GetSelection()
            if ($items.Count -eq 1 -and $items[0].Current.Name -ceq $Expected) { $box }
        }
    }
}
function Change-Status([string]$From, [string]$To) {
    $box = Status-Box $From
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $box.GetCurrentPattern($expand).Expand()
    $choice = Wait-Until "status option '$To'" {
        $item = Find-Element $To ([System.Windows.Automation.ControlType]::ListItem)
        if ($item -and $item.Current.IsEnabled -and !$item.Current.IsOffscreen) { $item }
    }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $popup = (Status-Box $To).GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
}
function Check-List([string]$Status, [string[]]$Expected) {
    $request = @('list-commands','--pane',"$remote")
    if ($Status) { $request += '--status', $Status }
    $result = Control $request
    $rows = @($result.commands)
    if ($result.pane -ne $remote -or $result.connection_generation -ne $generation -or $rows.Count -ne $Expected.Count) {
        throw "Unexpected history for '$Status': $($result | ConvertTo-Json -Depth 4 -Compress)"
    }
    for ($i = 0; $i -lt $Expected.Count; $i++) {
        if ($rows[$i].command -cne $Expected[$i]) {
            throw "Command $i with status '$Status' was '$($rows[$i].command)', expected '$($Expected[$i])'"
        }
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','70') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','history-search-ready')
    $generation = (Control @('inspect','--pane',"$remote")).connection_generation
    $all = @('check-status','deploy retry','deploy audit',$multiline,$command)
    Check-List '' $all
    Check-List 'all' $all
    Check-List 'failed' @('deploy retry',$command)
    Check-List 'succeeded' @('check-status',$multiline)
    Check-List 'unknown' @('deploy audit')
    $archivedId = ((Control @('list-commands','--pane',"$remote")).commands |
        Where-Object command -CEQ $command).id
    $suggestion = "pane-$remote-command-$archivedId.txt"
    Run-Palette 'Command history'
    $recent = 'check-status' + $dot + 'exit 0'
    $old = $command.Substring(0,120) + $dot + 'exit 17'
    $retry = 'deploy retry' + $dot + 'exit 2'
    $unknown = 'deploy audit' + $dot + 'status unknown'
    $null = Wait-Until 'newest command selected' { Find-Element "Selected, $recent" $button }
    Change-Status 'All commands' 'Failed'
    $null = Wait-Until 'both failures visible and newest failure selected' {
        (Find-Element "Selected, $retry" $button) -and (Find-Element $old $button) -and
            !(Find-Element $recent $button) -and !(Find-Element $unknown $button)
    }
    Click $old
    Search 'DEPLOY'
    $null = Wait-Until 'text filter kept the selected older failure' {
        (Find-Element "Selected, $old" $button) -and (Find-Element $retry $button)
    }
    Click 'Refresh history'
    $null = Wait-Until 'refresh selected the newest matching failure' { Find-Element "Selected, $retry" $button }
    Search $word.ToUpperInvariant()
    $null = Wait-Until 'older full command found beyond its row label' {
        (Find-Element "Selected, $old" $button) -and (Find-Element $command) -and !(Find-Element $retry $button)
    }
    $canceled = Join-Path $State 'canceled-output.txt'
    Pick-Output $canceled 2 $suggestion
    if ([IO.File]::Exists($canceled)) { throw 'Cancel created an output file' }
    $path = Join-Path $State 'selected-output.txt'
    Pick-Output $path 1 $suggestion
    $null = Wait-Until 'selected command output saved' { [IO.File]::Exists($path) }
    $bytes = [IO.File]::ReadAllBytes($path)
    if ([Convert]::ToBase64String($bytes) -cne [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($output))) {
        throw "The output file contained unexpected bytes: $([Convert]::ToBase64String($bytes))"
    }
    $savePrefix = "Saved command output from pane $remote to "
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $buttonFilter = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $button)
    $null = Wait-Until 'output export success on the history page' {
        foreach ($candidate in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $buttonFilter)) {
            $name = $candidate.Current.Name
            if ($name.StartsWith($savePrefix) -and [FixtureDialog]::SameFile($path, $name.Substring($savePrefix.Length))) {
                return $candidate
            }
        }
    }
    if ($verifyClipboard) {
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Click 'Copy command'
        $null = Wait-Until 'full original command on the clipboard' { [FixtureClipboard]::ContainsNewText($before, $command) }
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Click 'Copy output'
        $null = Wait-Until 'independent output on the clipboard' { [FixtureClipboard]::ContainsNewText($before, $output) }
    }
    Change-Status 'Failed' 'Succeeded'
    $null = Wait-Until 'matching successful output alone cannot enable copy' {
        (Find-Element 'No matching commands') -and !(Find-Element 'Copy command' $button).Current.IsEnabled -and
            !(Find-Element 'Copy output' $button).Current.IsEnabled -and !(Find-Element $saveOutput $button).Current.IsEnabled
    }
    Search ''
    $null = Wait-Until 'the successful command is selected' {
        (Find-Element "Selected, $recent" $button) -and !(Find-Element $unknown $button) -and
            !(Find-Element $old $button) -and !(Find-Element $retry $button)
    }
    Change-Status 'Succeeded' 'Status unknown'
    $null = Wait-Until 'only the command with no exit code is offered' {
        (Find-Element "Selected, $unknown" $button) -and !(Find-Element $recent $button) -and
            !(Find-Element $old $button) -and !(Find-Element $retry $button)
    }
    Change-Status 'Status unknown' 'All commands'
    $null = Wait-Until 'all results restored with the same command selected' {
        (Find-Element "Selected, $unknown" $button) -and (Find-Element $recent $button) -and
            (Find-Element $old $button) -and (Find-Element $retry $button)
    }
    $saveAs = 'Save as command' + [char]0x2026
    Click $saveAs
    $name = Wait-Until 'saved command name editor' { Find-Element 'Saved command name' $edit }
    $name.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Audit deployment')
    Click 'Cancel saving command'
    $null = Wait-Until 'history save editor closes' { !(Find-Element 'Saved command name' $edit) }
    Click $saveAs
    $name = Wait-Until 'new command name editor' { Find-Element 'Saved command name' $edit }
    $value = $name.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    if ($value.Current.Value) { throw 'Canceled name carried into a new save' }
    $value.SetValue('Audit deployment')
    Click 'Save to commands'
    $null = Wait-Until 'history reports the saved command' { Find-Element 'Saved command' }

    Click 'Return to pane'
    Wait-Focus $remote
    Run-Palette ('Insert command' + $dot + 'Audit deployment')
    Run-Palette 'Command history'
    Search 'deploy retry'
    Click 'Insert command'
    Wait-Focus $remote
    Run-Palette 'Command history'
    Search 'echo first'
    $null = Wait-Until 'unbracketed multiline insertion is disabled' {
        $candidate = Find-Element 'Insert command' $button
        $candidate -and !$candidate.Current.IsEnabled
    }
    $mode = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes(([char]27 + "[?2004hhistory-paste-ready`r`n")))
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64',$mode)
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','history-paste-ready')
    Click 'Refresh history'
    Click 'Insert command'
    Wait-Focus $remote
    foreach ($char in 'history-resumed'.ToCharArray()) { Post-Key 0x102 ([uint32]$char) }
    try {
        $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','history-resumed')
    } catch {
        throw "$_ Screen: $((Control @('get-text','--pane',"$remote")).text)"
    }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
