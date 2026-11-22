[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -AssemblyName System.Drawing
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_UI_SMOKE = '1'
$env:W11TERM_SCROLLBAR_SMOKE = $State
$profiles = Join-Path $State 'profiles.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","focus_left":"F4","focus_right":"F10","find":"F6"}}')
$edit = [System.Windows.Automation.ControlType]::Edit
$statusId = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::AutomationIdProperty,'find-status')

function Read-Rail([int]$Pane) {
    $request = Join-Path $State 'rail-capture'
    $pending = Join-Path $State 'pending-capture'
    [IO.File]::WriteAllText($pending,"$Pane")
    [IO.File]::Move($pending,$request)
    $null = Wait-Until "GPU readback from pane $Pane" { ![IO.File]::Exists($request) }
    $png = [Drawing.Bitmap]::new((Join-Path $State 'rail.png'))
    try {
        $stripes = [Collections.Generic.List[object]]::new()
        $trackRows = 0
        $prior = ''
        for ($y = 0; $y -lt $png.Height; $y++) {
            $kind = ''
            $track = $false
            for ($x = 0; $x -lt $png.Width; $x++) {
                $c = $png.GetPixel($x,$y)
                # The rail painter uses yellow #FFD84C and orange #FF7B29.
                if ($c.R -ge 251 -and [math]::Abs($c.G-216) -le 3 -and [math]::Abs($c.B-76) -le 3 -and !$kind) { $kind = 'hit' }
                if ($c.R -ge 251 -and [math]::Abs($c.G-123) -le 3 -and [math]::Abs($c.B-41) -le 3) { $kind = 'current' }
                if (([math]::Abs($c.R-41) -le 2 -and [math]::Abs($c.G-46) -le 2 -and [math]::Abs($c.B-54) -le 2) -or
                    ([math]::Abs($c.R-143) -le 2 -and [math]::Abs($c.G-153) -le 2 -and [math]::Abs($c.B-166) -le 2)) { $track = $true }
            }
            if ($track) { $trackRows++ }
            if ($kind) {
                if ($prior -cne $kind) { $stripes.Add([pscustomobject]@{ Kind=$kind; Top=$y; Bottom=$y }) }
                else { $stripes[$stripes.Count-1].Bottom = $y }
            }
            $prior = $kind
        }
        return [pscustomobject]@{ Height=$png.Height; TrackRows=$trackRows; Stripes=$stripes.ToArray() }
    } finally { $png.Dispose() }
}
function Assert-Marks([int]$Pane, [int]$Current) {
    $rail = Read-Rail $Pane
    $stripes = $rail.Stripes
    if ($rail.TrackRows -lt $rail.Height / 2 -or $stripes.Count -ne 3) {
        throw "Expected three visible marks on the SSH scrollbar. Readback: $($rail | ConvertTo-Json -Compress)"
    }
    $expected = @(0.10,0.50,0.875)
    for ($i = 0; $i -lt 3; $i++) {
        $actual = ($stripes[$i].Top + $stripes[$i].Bottom + 1) / (2.0 * $rail.Height)
        $kind = if ($i + 1 -eq $Current) { 'current' } else { 'hit' }
        if ([math]::Abs($actual-$expected[$i]) -gt 0.055 -or $stripes[$i].Kind -cne $kind) {
            throw "Expected $kind at $($expected[$i]), found $($stripes[$i].Kind) at $actual. Find result $Current."
        }
    }
    $picked = $stripes[$Current-1]
    $ordinary = $stripes[$Current % 3]
    if ($picked.Bottom-$picked.Top -le $ordinary.Bottom-$ordinary.Top) { throw 'The current mark is not taller than an ordinary hit' }
}
function Assert-Unmarked([int]$Pane, [bool]$HasHistory) {
    $rail = Read-Rail $Pane
    if ($rail.Stripes.Count -ne 0 -or
        ($HasHistory -and $rail.TrackRows -lt $rail.Height / 2) -or
        (!$HasHistory -and $rail.TrackRows -gt 0)) {
        throw "Unexpected rail for pane $Pane (scrollable $HasHistory): $($rail | ConvertTo-Json -Compress)"
    }
}
function Status {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,$statusId).Current.Name
}
function Send-Bytes([int]$Pane, [string]$Data) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Data))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$encoded)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','128','--rows','30') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $original = (Control @('list')).sessions[0]
    $ssh = $original.id
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','line 200')
    Run-Palette 'Split right with Palette PowerShell'
    $localPane = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -ceq 'Palette PowerShell').id }
    Wait-Focus $localPane
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15','PS ')
    Send-Bytes $localPane ('$global:markPid=$PID; 1..90 | % { "neighbor-row-$_" }; "neighbor-ready-" + $PID' + [char]13)
    $pidFrame = Control @('wait-for-text','--pane',"$localPane",'--regex','--timeout','15','neighbor-ready-([0-9]+)')
    $localPid = [regex]::Match($pidFrame.text,'neighbor-ready-([0-9]+)').Groups[1].Value
    Press-Key 0x73 # F4 focuses SSH.
    Wait-Focus $ssh
    Assert-Unmarked $ssh $true
    Press-Key 0x75 # F6 opens Find.
    $field = Wait-Until 'Find field' { Find-Element 'Find in scrollback' $edit }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('rail-witness')
    $current = [int](Wait-Until 'three hits in SSH history' {
        $label = Status
        if ($label -match '^([1-3]) / 3$') { [int]$Matches[1] }
    })
    Assert-Marks $ssh $current
    Assert-Unmarked $localPane $true
    Press-Key 0x72 # F3 moves the current result.
    $next = $current % 3 + 1
    $null = Wait-Until "next result $next / 3" { (Status) -ceq "$next / 3" }
    Assert-Marks $ssh $next

    # Keep Find open while the remote application switches its active screen.
    Send-Bytes $ssh ([char]27 + '[?1049h' + [char]27 + '[Halt-screen-ready')
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','alt-screen-ready')
    # Screen content commits before the debounced Find scan. Observe the alternate
    # result so the wait after restoring primary cannot accept its previous count.
    $null = Wait-Until 'Find scanned the alternate screen' { (Status) -ceq 'No matches' }
    Assert-Unmarked $ssh $false
    Send-Bytes $ssh ([char]27 + '[?1049l')
    $restored = [int](Wait-Until 'restored primary search results' {
        if (!(Control @('get-text','--pane',"$ssh")).alternate_screen -and (Status) -match '^([1-3]) / 3$') { [int]$Matches[1] }
    })
    Assert-Marks $ssh $restored
    Press-Key 27
    $null = Wait-Until 'Find closed' { !(Find-Element 'Find in scrollback' $edit) }
    Assert-Unmarked $ssh $true
    foreach ($c in 'mark-search-closed'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','mark-search-closed')
    Press-Key 0x79 # F10 focuses the original PowerShell split.
    Wait-Focus $localPane
    Send-Bytes $localPane ('"same-shell-" + ($PID -eq $markPid) + "-" + $PID' + [char]13)
    $null = Control @('wait-for-text','--pane',"$localPane",'--timeout','15',"same-shell-True-$localPid")
    $same = (Control @('list')).sessions | Where-Object id -eq $ssh
    if ($same.tab_id -ne $original.tab_id -or $same.connection_generation -ne $original.connection_generation) { throw 'Find replaced the original SSH connection' }
} catch {
    throw "$($_.Exception.Message) at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
