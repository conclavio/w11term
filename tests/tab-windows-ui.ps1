# The Rust loopback fixture starts one SSH session, two real PowerShell children and one GUI.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State,
      [Parameter(Mandatory)][ValidateSet('Origin','Destination')][string]$FirstClose)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class TabFixtureWindows {
    private delegate bool Visit(IntPtr hwnd, IntPtr unused);
    [DllImport("user32.dll")] private static extern bool EnumWindows(Visit visitor, IntPtr unused);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint owner);
    [DllImport("user32.dll")] private static extern IntPtr GetWindow(IntPtr hwnd, uint relationship);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] private static extern int GetWindowText(IntPtr hwnd, StringBuilder value, int limit);
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool GetExitCodeProcess(IntPtr process, out uint exitCode);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    public static IntPtr[] OwnedBy(int expected) {
        var found = new List<IntPtr>();
        EnumWindows(delegate(IntPtr hwnd, IntPtr unused) {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner != expected || !IsWindowVisible(hwnd) || GetWindow(hwnd, 4) != IntPtr.Zero) return true;
            var title = new StringBuilder(256);
            GetWindowText(hwnd, title, title.Capacity);
            if (title.ToString().StartsWith("w11term", StringComparison.OrdinalIgnoreCase)) found.Add(hwnd);
            return true;
        }, IntPtr.Zero);
        return found.ToArray();
    }
}
"@
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'

function Find-Here([IntPtr]$Hwnd, [string]$Name) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($Hwnd)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    return $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
}

function Use-Window([IntPtr]$Hwnd) {
    if ([TabFixtureWindows]::OwnedBy($app.Id) -notcontains $Hwnd) { throw "Unexpected fixture HWND $Hwnd" }
    if ([TabFixtureWindows]::GetForegroundWindow() -ne $Hwnd) {
        $null = [TabFixtureWindows]::SetForegroundWindow($Hwnd)
        $until = [DateTime]::UtcNow.AddMilliseconds(400)
        while ([TabFixtureWindows]::GetForegroundWindow() -ne $Hwnd -and [DateTime]::UtcNow -lt $until) { Start-Sleep -Milliseconds 20 }
        if ([TabFixtureWindows]::GetForegroundWindow() -ne $Hwnd) {
            $focusable = Wait-Until 'focusable button in the fixture HWND' { Find-Here $Hwnd 'New tab' }
            $focusable.SetFocus()
        }
        $null = Wait-Until "foreground for fixture HWND $Hwnd" { [TabFixtureWindows]::GetForegroundWindow() -eq $Hwnd }
    }
}

function Move-Tab([IntPtr]$Hwnd) {
    Use-Window $Hwnd
    $window = $Hwnd
    Press-Key 0x76 # F7, fixture command palette
    $field = Wait-Until 'focused command palette in the requested HWND' {
        $element = Find-Here $Hwnd 'Search commands'
        if ($element -and $element.Current.HasKeyboardFocus) { $element }
    }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('move tab to new window')
    $null = Wait-Until 'enabled Move tab to new window action' {
        $action = Find-Here $Hwnd 'Move tab to new window'
        if ($action -and $action.Current.IsEnabled) { $action }
    }
    Press-Key 13
}

function Type-In([IntPtr]$Hwnd, [string]$Text, [switch]$Return) {
    Use-Window $Hwnd
    $window = $Hwnd
    foreach ($c in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    if ($Return) { Press-Key 13 }
}

function Send-Line([int]$Pane, [string]$Text) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Text + [char]13))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$encoded)
}

function Expect-Text([int]$Pane, [string]$Value) {
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15',$Value)
}

function Shell-State([int]$Pane, [string]$Label, [IntPtr]$Hwnd = [IntPtr]::Zero, [switch]$Init) {
    $line = if ($Init) { '$global:WindowNonce=[guid]::NewGuid().ToString("N"); ' } else { '' }
    $line += "'$Label=' + " + '$PID + "/" + $global:WindowNonce'
    if ($Hwnd -eq [IntPtr]::Zero) { Send-Line $Pane $line } else { Type-In $Hwnd $line -Return }
    # The echoed source contains the label but cannot contain the evaluated marker.
    $pattern = "$Label=([0-9]+/[a-f0-9]{32})"
    $result = Control @('wait-for-text','--pane',"$Pane",'--timeout','15','--regex',$pattern)
    return [regex]::Match($result.text, $pattern).Groups[1].Value
}

function Close-FixtureWindow([IntPtr]$Hwnd) {
    if ([TabFixtureWindows]::OwnedBy($app.Id) -notcontains $Hwnd) { throw "Refusing unowned HWND $Hwnd" }
    if (![FixtureMessages]::PostMessage($Hwnd, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero)) {
        throw "WM_CLOSE failed for fixture HWND $Hwnd"
    }
}

function Expect-Group([int]$Count, [long]$OldWindow) {
    return Wait-Until 'both split panes to be registered in another window' {
        $all = @((Control @('list')).sessions)
        $remote = $all | Where-Object id -eq $ssh.id
        $local = $all | Where-Object id -eq $moved.id
        if ($all.Count -eq $Count -and $local -and $remote -and $remote.window_id -and
            $remote.window_id -ne $OldWindow -and $remote.window_id -eq $local.window_id -and
            $remote.tab_id -eq $ssh.tab_id -and $local.tab_id -eq $ssh.tab_id -and
            $remote.visible -and $local.visible -and $remote.connection_generation -eq $ssh.connection_generation) {
            return $remote.window_id
        }
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--workspace', 'two-windows') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    # Retain the process handle before any native close so Windows PowerShell can read its exit code.
    $processHandle = $app.Handle
    $origin = Wait-Until 'the original terminal HWND' {
        $handles = [TabFixtureWindows]::OwnedBy($app.Id)
        if ($handles.Count -eq 1) { $handles[0] }
    }
    $manifest = Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json"
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath $manifest }
    $initial = Wait-Until 'three initial pane registrations' {
        $panes = @((Control @('list')).sessions)
        if ($panes.Count -eq 3) { $panes }
    }
    $ssh = $initial | Where-Object label -eq 'window-ssh'
    $moved = $initial | Where-Object label -eq 'Moved PowerShell'
    $anchor = $initial | Where-Object label -eq 'Source PowerShell'
    if (!$ssh -or !$moved -or !$anchor -or !$ssh.window_id -or
        $ssh.window_id -ne $moved.window_id -or $ssh.window_id -ne $anchor.window_id -or
        $ssh.tab_id -ne $moved.tab_id -or $anchor.tab_id -eq $ssh.tab_id) { throw 'Unexpected initial split/tabs' }
    Expect-Text $ssh.id 'ready'
    # Connection generation is published independently of the initial layout.
    $ssh = Wait-Until 'connected SSH pane' {
        (Control @('list')).sessions | Where-Object { $_.id -eq $ssh.id -and $_.connection_generation -gt 0 }
    }
    Expect-Text $moved.id 'PS '
    Expect-Text $anchor.id 'PS '
    $localIdentity = Shell-State $moved.id 'movedBefore' -Init
    $anchorIdentity = Shell-State $anchor.id 'anchorBefore' -Init
    if ($localIdentity -eq $anchorIdentity) { throw 'Expected independent PowerShell children' }
    Write-Output "Moving split from source $origin"
    Move-Tab $origin
    $destination = Wait-Until 'two distinct native HWNDs owned by the same process' {
        $handles = [TabFixtureWindows]::OwnedBy($app.Id)
        if ($handles.Count -eq 2 -and $handles -contains $origin) {
            $handles | Where-Object { $_ -ne $origin }
        }
    }
    Write-Output "Source $origin and destination $destination are both live"
    $newOwner = Expect-Group 3 $ssh.window_id
    $sourceAfter = (Control @('list')).sessions | Where-Object id -eq $anchor.id
    if ($sourceAfter.window_id -ne $anchor.window_id -or $sourceAfter.tab_id -ne $anchor.tab_id) {
        throw 'The source tab identity changed'
    }
    Use-Window $destination
    Wait-Focus $ssh.id
    Type-In $destination 'ssh-probe/'
    Expect-Text $ssh.id 'ssh-probe/'
    $window = $destination
    Press-Key 0x77 # F8 focuses the right PowerShell pane
    Wait-Focus $moved.id
    if ((Shell-State $moved.id 'movedAfter' $destination) -ne $localIdentity) { throw 'Moved PowerShell restarted' }
    Use-Window $origin
    Wait-Focus $anchor.id
    if ((Shell-State $anchor.id 'sourceAfter' $origin) -ne $anchorIdentity) { throw 'Source PowerShell restarted' }
    Use-Window $destination
    Wait-Focus $moved.id # source focus must not replace this window's remembered focused pane
    $window = $destination
    Press-Key 0x78 # F9 focuses the left SSH pane
    Wait-Focus $ssh.id

    Write-Output "Verified per-window input; closing $FirstClose first"
    if ($FirstClose -eq 'Origin') {
        Close-FixtureWindow $origin
        $null = Wait-Until 'only the destination and its two panes to remain' {
            $handles = [TabFixtureWindows]::OwnedBy($app.Id)
            $panes = @((Control @('list')).sessions)
            $handles.Count -eq 1 -and $handles[0] -eq $destination -and $panes.Count -eq 2 -and
                !($panes | Where-Object id -eq $anchor.id)
        }
        Type-In $destination 'origin-survived/'
        Expect-Text $ssh.id 'origin-survived/'
        # Move the only remaining tab too. The empty source HWND must close only after handoff.
        Move-Tab $destination
        $last = Wait-Until 'only the replacement HWND to remain' {
            $handles = [TabFixtureWindows]::OwnedBy($app.Id)
            if ($handles.Count -eq 1 -and $handles[0] -ne $destination) { $handles[0] }
        }
        $null = Expect-Group 2 $newOwner
        if ((Shell-State $moved.id 'secondMove') -ne $localIdentity) { throw 'Last-tab transfer restarted PowerShell' }
        Type-In $last 'only-tab/'
        Expect-Text $ssh.id 'only-tab/'
    } else {
        Close-FixtureWindow $destination
        $last = $origin
        $null = Wait-Until 'only the source and its own pane to remain' {
            $handles = [TabFixtureWindows]::OwnedBy($app.Id)
            $panes = @((Control @('list')).sessions)
            $handles.Count -eq 1 -and $handles[0] -eq $origin -and $panes.Count -eq 1 -and $panes[0].id -eq $anchor.id
        }
        if ((Shell-State $anchor.id 'dstClosed' $origin) -ne $anchorIdentity) { throw 'Source shell did not survive' }
    }
    Close-FixtureWindow $last
    if (!$app.WaitForExit(10000)) { throw "Final HWND closed but GUI PID $($app.Id) remained alive" }
    [uint32]$code = 0
    if (![TabFixtureWindows]::GetExitCodeProcess($processHandle, [ref]$code)) { throw 'Could not read final GUI exit code' }
    if ($code -ne 0) { throw "Final window closed with exit code $code for PID $($app.Id)" }
    if (Test-Path -LiteralPath $manifest) { throw 'Discovery record survived final window close' }
} finally {
    if (!$app.HasExited) {
        foreach ($handle in [TabFixtureWindows]::OwnedBy($app.Id)) { $null = [FixtureMessages]::PostMessage($handle, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero) }
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
