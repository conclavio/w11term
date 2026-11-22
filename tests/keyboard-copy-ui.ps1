# Posted keys target this fixture. The real Ctrl+V chord runs on the disposable CI desktop.
[CmdletBinding()]
param([string]$Gui, [string]$Cli, [string]$State, [string]$Trace)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$verifyClipboard = $env:GITHUB_ACTIONS -ceq 'true' -and $env:RUNNER_ENVIRONMENT -ceq 'github-hosted'
if ($verifyClipboard) { . (Join-Path $PSScriptRoot 'support/clipboard.ps1') }
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$dot = ' ' + [char]0xb7 + ' '
function Read-SelectionRows {
    $document = Find-Element ("Terminal${dot}copy-fixture")
    if (!$document) { return }
    $pattern = $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
    foreach ($range in $pattern.GetSelection()) { $range.GetText(-1) }
}
function Wait-Selection([string[]]$Expected, [string]$Stage = 'keyboard selection') {
    try {
        $null = Wait-Until "$Stage with exact rows [$($Expected -join '|')]" {
            $rows = @(Read-SelectionRows)
            if ($rows.Count -ne $Expected.Length) { return }
            for ($i = 0; $i -lt $Expected.Length; $i++) {
                if ($rows[$i] -cne $Expected[$i]) { return }
            }
            return $true
        }
    } catch {
        $observed = ConvertTo-Json -InputObject @(Read-SelectionRows) -Compress
        throw "$_ Observed UIA selection: $observed"
    }
}
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'copy-fixture') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-Until 'rendered SSH window' { $app.Refresh(); if ($app.MainWindowTitle -like 'w11term*copy-fixture*') { $app.MainWindowHandle } }
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $pane = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','history-end')
    $null = Wait-Until 'measured canvas with the path outside the live screen' {
        $box = Control @('get-text','--pane',"$pane")
        ($box.cols -ne 80 -or $box.rows -ne 24) -and !$box.text.Contains('/srv/game/zone')
    }
    Press-Key 0x77 # F8, remapped in this profile's settings
    $null = Wait-Until 'copy mode' { Copy-ModeOpen }
    Press-Key 0x21 # PageUp
    Press-Key 0x47 # g, oldest retained row
    Press-Key 0x24 # Home
    Press-Key 0x56 # v
    Press-Key 0x4c # l
    Press-Key 0x27 # Right
    Post-Key 0x102 0x71 # unrelated WM_CHAR stays local
    Wait-Selection 'his'
    if ($verifyClipboard) {
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Press-Key 0x59 # y
        $null = Wait-Until 'copy of first three history cells' { [FixtureClipboard]::ContainsNewText($before, 'his') }
    } else {
        Press-Key 0x1b
    }
    $null = Wait-Until 'first copy mode closed' { !(Copy-ModeOpen) }

    Press-Key 0x77
    $null = Wait-Until 'copy mode for word navigation' { Copy-ModeOpen }
    # Post a burst: jump to retained output, pass the path, step back and
    # select through its final cell. Only ':' is configured as punctuation.
    foreach ($key in 0x47,0x24,0x57,0x57,0x42,0x56,0x45) { Press-Key $key } # g Home w w b v e
    Wait-Selection '/srv/game/zone'
    if ($verifyClipboard) {
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Press-Key 0x59
        $null = Wait-Until 'custom-delimited path on the clipboard' {
            [FixtureClipboard]::ContainsNewText($before, '/srv/game/zone')
        }
        $null = Wait-Until 'word selection copied and closed' { !(Copy-ModeOpen) }
        Press-Key 0x77
        $null = Wait-Until 'copy mode before Escape' { Copy-ModeOpen }
    }
    Press-Key 0x1b
    $null = Wait-Until 'return from Escape' { !(Copy-ModeOpen) }

    # From the first retained line move into the numeric column of the table.
    # The inline blanks are significant: the fixture requests padded block copies.
    $column = '731','42 ','8  '
    Press-Key 0x77
    $null = Wait-Until 'copy mode for table selection' { Copy-ModeOpen }
    foreach ($key in 0x47,0x24,0x4a,0x4c,0x4c,0x4c,0x4c) { Press-Key $key } # g Home j l l l l
    Press-Key 0x52 # r, upper left corner
    Wait-Selection '7' 'r anchor'
    foreach ($key in 0x4a,0x4a,0x4c,0x4c) { Press-Key $key } # j j l l
    Wait-Selection $column 'forward r selection'
    if ($verifyClipboard -and $env:W11TERM_VERIFY_HOSTED_PASTE -ceq '1') {
        # Start a fresh rectangle at the lower right corner with the real chord.
        Press-Key 0x52
        Wait-Selection '' 'cleared block selection'
        Add-Type -AssemblyName System.Windows.Forms
        if ([FixtureMessages]::GetForegroundWindow() -ne $window) { throw 'Fixture does not own keyboard input' }
        [Windows.Forms.SendKeys]::SendWait('^v')
        Wait-Selection ' ' 'Ctrl+V anchor'
        # Posted messages can overtake native input queued for a different process.
        # Deliver these movements through the same keyboard queue as the real chord.
        if ([FixtureMessages]::GetForegroundWindow() -ne $window) { throw 'Fixture lost keyboard input' }
        [Windows.Forms.SendKeys]::SendWait('hhkk')
        Wait-Selection $column 'reverse Ctrl+V selection'
    }
    if ($verifyClipboard) {
        $before = [FixtureClipboard]::GetClipboardSequenceNumber()
        Press-Key 0x59
        $null = Wait-Until 'padded table column on the Windows clipboard' {
            [FixtureClipboard]::ContainsNewText($before, ($column -join [Environment]::NewLine))
        }
    } else {
        Press-Key 0x1b
    }
    $null = Wait-Until 'block copy mode closed' { !(Copy-ModeOpen) }
    Post-Key 0x102 0x21 # explicit WM_CHAR proves that normal shell input resumed
    $null = Wait-Until 'SSH peer received following input' { (Get-Item -LiteralPath $Trace).Length -gt 0 }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
