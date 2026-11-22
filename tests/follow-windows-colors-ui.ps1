param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/palette-pixels.ps1')
. (Join-Path $PSScriptRoot 'support/profile-scheme-editor.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$peer = [IO.File]::ReadAllText((Join-Path $State 'peer-root'))
$outputFile = Join-Path $peer 'copy-history.vt'
$traces = Join-Path $peer 'split-inputs'

function Expect-Reports([IO.BinaryReader]$Reader, [string]$Expected) {
    $null = Wait-Until 'exact terminal reports at the SSH peer' {
        $arrived = Read-PeerTrace $Reader
        $actual = [Text.Encoding]::ASCII.GetString($arrived)
        if (!$Expected.StartsWith($actual,[StringComparison]::Ordinal)) {
            throw "Unexpected SSH bytes: $([BitConverter]::ToString($arrived))"
        }
        $actual -ceq $Expected
    }
}
function Wait-SchemeFrame([int]$Pane, $Before, [string]$Scheme) {
    $frame = Wait-Until "published scheme $Scheme" {
        $current = Control @('get-text','--pane',"$Pane")
        if ($current.generation -gt $Before.generation) { $current }
    }
    if ($frame.text -cne $Before.text) { throw "Switching to $Scheme altered existing terminal text" }
}
function Use-Scheme([int]$Pane, [string]$Scheme) {
    $before = Control @('get-text','--pane',"$Pane")
    Run-Palette "Set pane color scheme to $Scheme"
    Wait-SchemeFrame $Pane $before $Scheme
}

function Windows-IsDark($Settings) {
    $fg = $Settings.GetColorValue([Windows.UI.ViewManagement.UIColorType,Windows.UI.ViewManagement,ContentType=WindowsRuntime]::Foreground)
    return (5 * [int]$fg.G + 2 * [int]$fg.R + [int]$fg.B) -gt (8 * 128)
}

function Check-HostedTransition([int]$Pane, [string]$Reports) {
    # Only the disposable, GitHub-hosted runner can change its test user's preference.
    if ($env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_ENVIRONMENT -cne 'github-hosted') { return }
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class HostedThemeChange {
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    static extern IntPtr SendMessageTimeout(IntPtr window, uint message, UIntPtr key,
        string setting, uint flags, uint milliseconds, out UIntPtr result);

    public static void Broadcast() {
        UIntPtr result;
        // WM_SETTINGCHANGE, HWND_BROADCAST, SMTO_ABORTIFHUNG. An unrelated
        // timed-out window does not invalidate the independent WinRT observation.
        SendMessageTimeout(new IntPtr(0xffff), 0x001a, UIntPtr.Zero, "ImmersiveColorSet", 2, 200, out result);
    }
}
'@
    $themes = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Themes', $true)
    if (!$themes) { throw 'Hosted Windows has no current-user Themes registry key' }
    $key = $null
    $created = $false
    $name = 'AppsUseLightTheme'
    try {
        $key = $themes.OpenSubKey('Personalize', $true)
        if (!$key) { $key = $themes.CreateSubKey('Personalize'); $created = $true }
        $hadValue = $key.GetValueNames() -contains $name
        if ($hadValue) {
            $oldKind = $key.GetValueKind($name)
            $oldValue = $key.GetValue($name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        }
        $before = Control @('get-text','--pane',"$Pane")
        try {
            $key.SetValue($name, [int]$windowsDark, [Microsoft.Win32.RegistryValueKind]::DWord)
            [HostedThemeChange]::Broadcast()
            $null = Wait-Until 'independent Windows UISettings to change after the hosted preference write' {
                (Windows-IsDark $settings) -ne $windowsDark
            }
            Expect-Reports $reader ($Reports + $oppositeReport)
            Wait-SchemeFrame $Pane $before 'changed Windows appearance'
            Check-Pixels $Pane 'hosted-os-event' $oppositeColors
            $before = Control @('get-text','--pane',"$Pane")
        } finally {
            if ($hadValue) { $key.SetValue($name, $oldValue, $oldKind) }
            else { $key.DeleteValue($name, $false) }
            [HostedThemeChange]::Broadcast()
        }
        $null = Wait-Until 'independent Windows UISettings to return to the original preference' {
            (Windows-IsDark $settings) -eq $windowsDark
        }
        Expect-Reports $reader ($Reports + $oppositeReport + $hostReport)
        Wait-SchemeFrame $Pane $before 'restored Windows appearance'
        Check-Pixels $Pane 'hosted-os-restored' $hostColors
    } finally {
        if ($key) {
            $removeKey = $created -and $key.SubKeyCount -eq 0 -and $key.ValueCount -eq 0
            $key.Dispose()
            if ($removeKey) { $themes.DeleteSubKey('Personalize', $false) }
        }
        $themes.Dispose()
    }
}

for ($launch=1; $launch -le 2; $launch++) {
    # The second SSH peer asks for the current preference, then observes local changes.
    $wire = $sample
    if ($launch -eq 2) { $wire += $esc + '[?2031h' + $esc + '[?996n' }
    [IO.File]::WriteAllText($outputFile,$wire)
    $log = Join-Path $State "gui-$launch.log"
    $reader = $null
    $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','90','--rows','20') -WindowStyle Hidden -PassThru -RedirectStandardError $log
    try {
        $window = Wait-InputWindow
        $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
        $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
        $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','palette-witness')
        if ($launch -eq 1) {
            Edit-Scheme 'palette-ssh' 'Edit profile' 'Follow Windows' 'Save profile changes'
            Check-Pixels $pane 'original-fixed-profile' $legacy
            continue
        }

        # Ask the Windows Runtime independently of w11term. Windows exposes the
        # foreground text color; a light foreground means the app background is dark.
        $settings = [Windows.UI.ViewManagement.UISettings,Windows.UI.ViewManagement,ContentType=WindowsRuntime]::new()
        $windowsDark = Windows-IsDark $settings
        if ($windowsDark) {
            $hostColors = $dark; $oppositeColors = $light; $fixed = 'One Half Light'
            $hostReport = $esc + '[?997;1n'; $oppositeReport = $esc + '[?997;2n'
        } else {
            $hostColors = $light; $oppositeColors = $dark; $fixed = 'One Half Dark'
            $hostReport = $esc + '[?997;2n'; $oppositeReport = $esc + '[?997;1n'
        }
        $channel = Wait-Until 'one SSH channel receiving automatic terminal reports' {
            $paths = @([IO.Directory]::GetFiles($traces))
            if ($paths.Count -eq 1) { $paths[0] }
        }
        $reader = Open-PeerTraceReader $channel
        Expect-Reports $reader $hostReport
        Check-Pixels $pane 'follow-at-startup' $hostColors
        $original = ((Control @('list')).sessions | Where-Object id -eq $pane).connection_generation

        Use-Scheme $pane $fixed
        Expect-Reports $reader ($hostReport + $oppositeReport)
        Check-Pixels $pane 'temporarily-fixed' $oppositeColors
        Use-Scheme $pane 'Follow Windows'
        Expect-Reports $reader ($hostReport + $oppositeReport + $hostReport)
        Check-Pixels $pane 'following-again' $hostColors
        Check-HostedTransition $pane ($hostReport + $oppositeReport + $hostReport)
        Wait-Focus $pane
        if ((Saved-Scheme 'palette-ssh') -cne 'follow-windows' -or
            ((Control @('list')).sessions | Where-Object id -eq $pane).connection_generation -ne $original) {
            throw 'A live color change replaced the SSH connection or the saved Follow Windows preference'
        }
    } catch {
        $failure = $_
        if ($launch -eq 1) {
            try { $diagnostics = Editor-State } catch { $diagnostics = "UI diagnostics unavailable: $_" }
        } else {
            $diagnostics = "Original Windows dark preference: $windowsDark; trace: $channel"
        }
        throw "Launch $launch failed: $failure at $($failure.ScriptStackTrace). $diagnostics"
    } finally {
        if ($reader) { $reader.Dispose() }
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
}
