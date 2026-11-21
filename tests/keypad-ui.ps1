# Post only to the HWND owned by this fixture. Scan/E0, previous-state and
# transition bits are the same fields delivered by Windows for physical keys.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State, [Parameter(Mandatory)][string]$Trace,
      [switch]$Kitty, [switch]$PhysicalLayout)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:LOCALAPPDATA = $State
$profiles = Join-Path $State 'profiles.json'

function Key-Event([uint32]$Vk, [int]$Scan, [bool]$Extended,
                   [ValidateSet('Down','DownWithPrevious','Repeat','Up')][string]$Action) {
    [long]$detail = 1 -bor ($Scan -shl 16)
    if ($Extended) { $detail = $detail -bor 0x01000000 }
    $message = 0x100 # WM_KEYDOWN
    if ($Action -ne 'Down') { $detail = $detail -bor 0x40000000 }
    if ($Action -eq 'Up') { $message = 0x101; $detail = $detail -bor 2147483648L }
    if (![FixtureMessages]::PostMessage($window, $message, [UIntPtr]::new($Vk), [IntPtr]::new($detail))) {
        throw "Could not post $Action for VK $Vk, scan $Scan, E0 $Extended"
    }
}
function Tap([uint32]$Vk, [int]$Scan, [bool]$Extended) {
    Key-Event $Vk $Scan $Extended Down
    Key-Event $Vk $Scan $Extended Up
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profile','keypad-fixture','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-Until 'bound fixture window' {
        $app.Refresh()
        if ($app.MainWindowTitle -like 'w11term *keypad-fixture*') { $app.MainWindowHandle }
    }
    $pane = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','keypad-fixture-ready')
    if ($PhysicalLayout) {
        Add-Type -Path (Join-Path $PSScriptRoot 'support/physical-layout.cs')
        $key = [PhysicalLayoutFixture]::Printable($window, $app.Id)
        [IO.File]::WriteAllText((Join-Path $State 'logical-scalar.txt'), [string]$key[1])
        # Same logical VK held on the PC-101 Y and Z positions. Release Z first.
        Key-Event $key[0] 0x15 $false Down
        Key-Event $key[0] 0x2C $false DownWithPrevious
        Key-Event $key[0] 0x15 $false Repeat
        Key-Event $key[0] 0x2C $false Up
        Key-Event $key[0] 0x15 $false Up
    } else {
        Key-Event 0x0D 0x1C $false Down
        Key-Event 0x0D 0x1C $true DownWithPrevious # shared VK_RETURN is already down
        Key-Event 0x0D 0x1C $false Up
        Key-Event 0x0D 0x1C $true Repeat
        Key-Event 0x0D 0x1C $true Up
        Tap 0x25 0x4B $false # keypad with NumLock off
        Tap 0x25 0x4B $true  # enhanced Left
        Tap 0x64 0x4B $false # VK_NUMPAD4
        Tap 0x6B 0x4E $false # VK_ADD
    }
    Tap 0x7B 0x58 $false # F12 marks the end of the same message queue
    $fence = if ($Kitty) { [char]27 + '[24;1:3~' } else { [char]27 + '[24~' }
    $null = Wait-Until 'SSH peer to receive the trailing F12' {
        try { [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($Trace)).EndsWith($fence) }
        catch [IO.IOException] { $false }
    }
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
