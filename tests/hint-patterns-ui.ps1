# The clipboard assertion is opt-in for disposable CI desktops.
[CmdletBinding()]
param([string]$Gui, [string]$Cli, [switch]$VerifyClipboard)
$ErrorActionPreference = 'Stop'
if (!$Gui) { $Gui = Join-Path $PSScriptRoot '..\target\debug\w11term.exe' }
if (!$Cli) { $Cli = Join-Path $PSScriptRoot '..\target\debug\w11term-cli.exe' }
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$state = (New-Item -ItemType Directory -Force -Path (Join-Path $PSScriptRoot ('..\.local\hint-pattern-smoke\' + [guid]::NewGuid().ToString('N')))).FullName
$profiles = Join-Path $state 'profiles.json'
$replay = Join-Path $state 'pattern-fixture.vt'
[IO.File]::WriteAllText($profiles, '{"profiles":{}}')
[IO.File]::WriteAllText($replay, 'OPS-104 BUG-203')
$oldData = $env:W11TERM_DATA_DIR
$oldSettings = $env:W11TERM_SETTINGS_PATH
$app = $null

function Click([string]$Name) {
    Invoke-Button (Wait-Until $Name {
        $button = Find-Element $Name ([System.Windows.Automation.ControlType]::Button)
        if ($button -and $button.Current.IsEnabled) { $button }
    })
}
function Edit-Value([string]$Name, [string]$Value) {
    $edit = Wait-Until $Name { Find-Element $Name ([System.Windows.Automation.ControlType]::Edit) }
    $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Value)
}
function Show-Patterns {
    Click 'Tab menu'
    Invoke-Button (Wait-Until 'settings menu' { Find-Element 'Settings' ([System.Windows.Automation.ControlType]::MenuItem) })
    Click 'Hint patterns'
}
function Show-Pane {
    Click 'Back to settings'
    Picker-Query 'pattern-fixture.vt'
    Press-Key 13
    Wait-Focus $pane
}
function Test-Match([string]$Name, [string]$Match) {
    Press-Key 0x77 # the remapped F8 shortcut
    Press-Key 0x41 # label a
    $label = $Name + ' ' + [char]0xb7 + ' Enter copy ' + [char]0xb7 + ' Esc cancel   ' + $Match
    $null = Wait-Until "hint for $Match" { Find-Element $label }
    if ($VerifyClipboard) {
        Set-Clipboard -Value 'pending'
        Press-Key 13
        $null = Wait-Until 'copying the exact match' { (Get-Clipboard -Raw) -ceq $Match }
    } else {
        Press-Key 27
    }
    $null = Wait-Until 'dismissed hints' { -not (Find-Element $label) }
}
function Stop-Fixture {
    if ($app -and -not $app.HasExited) {
        $null = $app.CloseMainWindow()
        if (-not $app.WaitForExit(3000)) { $app.Kill(); $app.WaitForExit() }
    }
}

try {
    $env:W11TERM_DATA_DIR = $state
    $env:W11TERM_SETTINGS_PATH = Join-Path $state 'settings.json'
    [IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"keyboard_hints":"F8"}}')
    for ($launch = 1; $launch -le 2; $launch++) {
        $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--replay', ('"' + $replay + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $state "run-$launch.log")
        $window = Wait-Until 'window handle' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
        $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
        $pane = (Control @('list')).sessions[0].id
        $null = Control @('wait-for-text','--pane',"$pane",'--timeout','10','OPS-104 BUG-203')
        if ($launch -eq 1) {
            Show-Patterns
            Click 'Add pattern'
            Edit-Value 'Pattern name' 'Issue'
            Edit-Value 'Regular expression' 'x*'
            Click 'Save pattern'
            $null = Wait-Until 'validation of empty matches' { Find-Element 'The expression must match at least one character every time.' }
            Edit-Value 'Regular expression' '\bOPS-[0-9]+\b'
            Click 'Save pattern'
            $null = Wait-Until 'saved rule' { Find-Element 'Edit pattern Issue' }
            Show-Pane
        }
        Test-Match 'Issue' 'OPS-104'
        if ($launch -eq 2) {
            Show-Patterns
            Click 'Edit pattern Issue'
            Edit-Value 'Pattern name' 'Bug'
            Edit-Value 'Regular expression' 'BUG-[0-9]+'
            Click 'Save pattern'
            $null = Wait-Until 'renamed rule' { Find-Element 'Edit pattern Bug' }
            Show-Pane
            Test-Match 'Bug' 'BUG-203'
            Show-Patterns
            Click 'Edit pattern Bug'
            Click 'Remove pattern'
            $null = Wait-Until 'rule removal' { Find-Element 'Add pattern' }
            Show-Pane
            Press-Key 0x77
            $null = Wait-Until 'no matches after removal' { Find-Element ('No matching hints in this pane ' + [char]0xb7 + ' Esc close') }
            $settings = Get-Content -Raw -LiteralPath $env:W11TERM_SETTINGS_PATH | ConvertFrom-Json
            if ($settings.hint_patterns) { throw 'Removed pattern was still saved' }
            Press-Key 27
        }
        Stop-Fixture
    }
    Write-Output 'Named hint patterns passed native add, edit, selection, restart and removal.'
} finally {
    Stop-Fixture
    $env:W11TERM_DATA_DIR = $oldData
    $env:W11TERM_SETTINGS_PATH = $oldSettings
}
