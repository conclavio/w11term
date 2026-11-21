[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$verifyClipboard = $env:GITHUB_ACTIONS -eq 'true' -and $env:RUNNER_ENVIRONMENT -eq 'github-hosted'
if ($verifyClipboard) { . (Join-Path $PSScriptRoot 'support/clipboard.ps1') }
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$word = '[' + [char]0xc5 + 'ngstr' + [char]0xf6 + 'm]'
$command = 'deploy ' + ('x' * 128) + ' ' + $word
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$dot = ' ' + [char]0xb7 + ' '
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

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','70') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','history-search-ready')
    Run-Palette 'Command history'
    $recent = "Selected, check-status${dot}exit 0"
    $null = Wait-Until 'newest command selected' { Find-Element $recent $button }
    Search $word.ToUpperInvariant()
    $old = 'Selected, ' + $command.Substring(0,120) + $dot + 'exit 17'
    $null = Wait-Until 'older full command found beyond its row label' {
        (Find-Element $old $button) -and (Find-Element $command) -and !(Find-Element "check-status${dot}exit 0" $button)
    }
    Click 'Refresh history'
    $null = Wait-Until 'refresh selected the newest matching command' { Find-Element $old $button }
    if ($verifyClipboard) {
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Click 'Copy command'
        $null = Wait-Until 'full original command on the clipboard' { [FixtureClipboard]::ContainsNewText($before, $command) }
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Click 'Copy output'
        $null = Wait-Until 'independent output on the clipboard' { [FixtureClipboard]::ContainsNewText($before, "archived deploy output`r`n") }
    }
    Search '[not a match]'
    $null = Wait-Until 'empty results disable both copy actions' {
        (Find-Element 'No matching commands') -and !(Find-Element 'Copy command' $button).Current.IsEnabled -and !(Find-Element 'Copy output' $button).Current.IsEnabled
    }
    Search ''
    $null = Wait-Until 'clearing selects the most recent command' { Find-Element $recent $button }
    Click 'Return to pane'
    Wait-Focus $remote
    foreach ($char in 'history-resumed'.ToCharArray()) { Post-Key 0x102 ([uint32]$char) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','history-resumed')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
