[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$real = $env:GITHUB_ACTIONS -ceq 'true' -and $env:RUNNER_ENVIRONMENT -ceq 'github-hosted' -and $env:W11TERM_VERIFY_HOSTED_PASTE -ceq '1'
$utf8 = [Text.UTF8Encoding]::new($false)
$sentinel = 'existing clipboard ' + [Guid]::NewGuid().ToString('N')
$label = 'Select all terminal text'
if ($real) {
    Remove-Item Env:W11TERM_TEST_COPY_FILE -ErrorAction SilentlyContinue
    . (Join-Path $PSScriptRoot 'support/clipboard.ps1')
    . (Join-Path $PSScriptRoot 'support/cursor-cell.ps1')
    Add-Type -AssemblyName System.Windows.Forms
} else {
    # On a developer desktop, direct writes to the fixture and post keyboard
    # events only to w11term. The hosted run exercises the default physical chords.
    $env:W11TERM_TEST_COPY_FILE = Join-Path $State 'private-clipboard.txt'
    $settings = Get-Content -LiteralPath $env:W11TERM_SETTINGS_PATH -Raw | ConvertFrom-Json
    $settings.shortcuts | Add-Member -NotePropertyName select_all -NotePropertyValue 'F10'
    [IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, ($settings | ConvertTo-Json -Depth 5), $utf8)
}

function Send-Chord([string]$Keys) {
    if (!$real -or [FixtureMessages]::GetForegroundWindow() -ne $window) { throw 'Physical chord requires the hosted fixture to own foreground' }
    [Windows.Forms.SendKeys]::SendWait($Keys)
}
function Set-Sentinel {
    if (!$real) { [IO.File]::WriteAllText($env:W11TERM_TEST_COPY_FILE,$sentinel,$utf8); return 0 }
    $before = [FixtureClipboard]::GetClipboardSequenceNumber()
    $null = Wait-Until 'hosted clipboard initialized' {
        try {
            [Windows.Forms.Clipboard]::SetText($sentinel,[Windows.Forms.TextDataFormat]::UnicodeText)
            [FixtureClipboard]::ContainsNewText($before,$sentinel)
        } catch [Runtime.InteropServices.ExternalException] { $false }
    }
    return [FixtureClipboard]::GetClipboardSequenceNumber()
}
function Assert-Uncopied([uint32]$Sequence) {
    if ($real) {
        if ([FixtureClipboard]::GetClipboardSequenceNumber() -ne $Sequence) { throw 'Selection modified the Windows clipboard before Copy' }
    } elseif ((Read-ClipboardFixture) -cne $sentinel) { throw 'Selection copied text before explicit Copy' }
}
function Start-Selection([string]$Scope, [string]$Marker, [switch]$Palette) {
    if ($Palette) {
        $chord = if ($real) { 'Ctrl+Shift+A' } else { 'F10' }
        Run-Palette "$label    $chord"
    }
    elseif ($real) { Send-Chord '^+a' }
    else { Press-Key 0x79 } # F10 rebound to the same shortcut.
    $dot = ' ' + [char]0xb7 + ' '
    $name = "Copy mode$dot$Scope${dot}y or Enter copy${dot}Esc cancel"
    $null = Wait-Until "selection of $Scope" { Find-Element $name }
    $null = Wait-Until "UI Automation selection includes $Marker" {
        $doc = Find-Element "Terminal${dot}palette-ssh"
        if (!$doc) { return $false }
        $provider = $doc.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
        $selected = @($provider.GetSelection() | ForEach-Object { $_.GetText(-1) }) -join ''
        $selected.Contains($Marker)
    }
}
function Copy-Selection([uint32]$Sequence,[string]$Expected) {
    if ($real) { Send-Chord '^+c' } else { Press-Key 0x59 } # y in copy mode
    $null = Wait-Until 'explicit Copy produced exactly the expected Unicode text' {
        if ($real) { return [FixtureClipboard]::ContainsNewText($Sequence,$Expected) }
        return (Read-ClipboardFixture) -ceq $Expected
    }
    $null = Wait-Until 'copy mode closed after Copy' { !(Copy-ModeOpen) }
}
function Send-RemoteBytes([byte[]]$Bytes) {
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64',[Convert]::ToBase64String($Bytes))
}
function Shell-Pid([string]$Tag) {
    $line = "'$Tag=' + " + '$PID'
    foreach ($ch in $line.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    Press-Key 13
    $pattern = "$Tag=([0-9]+)"
    $reply = Control @('wait-for-text','--pane',"$localPane",'--regex','--timeout','15',$pattern)
    return [int]([regex]::Match($reply.text,$pattern).Groups[1].Value)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','64','--rows','12') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $remote = (Control @('list')).sessions[0].id
    $localPane = (Control @('open','--profile','Palette PowerShell','--activate')).pane
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','20','PS ')
    $pidBefore = Shell-Pid 'private-neighbor-marker'
    Press-Key 0x77 # F8 selects the SSH tab.
    Wait-Focus $remote
    if ($real) { Show-CursorWindow }
    $null = Control @('resize','--pane',"$remote",'--cols','64','--rows','12')
    [IO.File]::Delete([IO.File]::ReadAllText((Join-Path $State 'output-gate')))
    $end = Control @('wait-for-text','--pane',"$remote",'--timeout','15','LAST-RETAINED-SSH')
    $first = (Control @('find-text','--pane',"$remote",'FIRST-RETAINED-SSH')).panes[0]
    $old = (Control @('find-text','--pane',"$remote",'EVICTED-SSH')).panes[0]
    if ($first.count -ne 1 -or $old.count -ne 0 -or $end.text.Contains('FIRST-RETAINED-SSH')) { throw 'Fixture did not evict the prelude and place the first retained marker offscreen' }
    $sequence = Set-Sentinel
    Start-Selection 'All retained text selected' 'LAST-RETAINED-SSH'
    Assert-Uncopied $sequence
    Press-Key 0x1b
    $null = Wait-Until 'Escape canceled the selection' { !(Copy-ModeOpen) }
    Assert-Uncopied $sequence
    Start-Selection 'All retained text selected' 'LAST-RETAINED-SSH'
    Assert-Uncopied $sequence
    Copy-Selection $sequence ([IO.File]::ReadAllText((Join-Path $State 'expected-primary.txt')))
    Wait-Focus $remote
    if ($real) { Send-Chord '^a' } else { Post-Key 0x102 1 } # translated Ctrl+A -> SOH
    $inputDir = [IO.File]::ReadAllText((Join-Path $State 'input-dir'))
    $null = Wait-Until 'only plain Ctrl+A reaches the SSH channel' {
        $files = @(Get-ChildItem -LiteralPath $inputDir -File)
        if ($files.Count -ne 1) { return $false }
        $reader = Open-PeerTraceReader $files[0].FullName
        try {
            $wire = Read-PeerTrace $reader
            $wire.Length -eq 1 -and $wire[0] -eq 1
        } finally { $reader.Dispose() }
    }
    Post-Key 0x102 0x21
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','LAST-RETAINED-SSH!')

    Send-RemoteBytes ([IO.File]::ReadAllBytes((Join-Path $State 'alternate.vt')))
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','alternate 12')
    $sequence = Set-Sentinel
    Start-Selection 'Alternate screen text selected' 'alternate 12' -Palette
    Assert-Uncopied $sequence
    Copy-Selection $sequence ([IO.File]::ReadAllText((Join-Path $State 'expected-alternate.txt')))
    Send-RemoteBytes ([Text.Encoding]::ASCII.GetBytes(([char]27 + '[?1049l')))
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','LAST-RETAINED-SSH!')
    $null = Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $localPane
    if ((Shell-Pid 'private-neighbor-after') -ne $pidBefore) { throw 'Select All interrupted the neighboring PowerShell' }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
