# The Rust loopback fixture starts one SSH session, two real PowerShell children and one GUI.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State,
      [Parameter(Mandatory)][ValidateSet('Origin','Destination','Existing','AllTabs','Context','Search','PaneSearch','TextSearch','PaneMove','PaneWindow','WindowName','WindowSearch','ProfileWindow','DuplicateWindow','WorkspaceWindow','ManageWorkspace','ReplaceWorkspace','CopyWorkspace','RepairWorkspace','CliWindows','CliWorkspace','CliWorkspaceWindow','CliProfiles')][string]$FirstClose)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/tab-context.ps1')
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
    [DllImport("user32.dll")] public static extern bool ShowWindowAsync(IntPtr hwnd, int command);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hwnd);
    public static string Title(IntPtr hwnd) {
        var title = new StringBuilder(512);
        GetWindowText(hwnd, title, title.Capacity);
        return title.ToString();
    }
    public static IntPtr[] OwnedBy(int expected) {
        var found = new List<IntPtr>();
        EnumWindows(delegate(IntPtr hwnd, IntPtr unused) {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            if (owner != expected || !IsWindowVisible(hwnd) || GetWindow(hwnd, 4) != IntPtr.Zero) return true;
            if (Title(hwnd).StartsWith("w11term", StringComparison.OrdinalIgnoreCase)) found.Add(hwnd);
            return true;
        }, IntPtr.Zero);
        return found.ToArray();
    }
}
"@
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'

function Find-Here([IntPtr]$Hwnd, [string]$Name, [System.Windows.Automation.ControlType]$Type) {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($Hwnd)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    if ($Type) {
        $byType = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $Type)
        $condition = [System.Windows.Automation.AndCondition]::new($condition, $byType)
    }
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
            try { $focusable.SetFocus() } catch [System.Management.Automation.MethodInvocationException] {
                # The HWND restore below handles a declined UI Automation focus request.
            }
        }
        if ([TabFixtureWindows]::GetForegroundWindow() -ne $Hwnd) {
            # Windows can decline activation by a background automation process.
            # Request a real restore on the verified fixture window's own GUI thread.
            $null = [TabFixtureWindows]::ShowWindowAsync($Hwnd, 7) # SW_SHOWMINNOACTIVE
            $null = Wait-Until 'fixture HWND to minimize' { [TabFixtureWindows]::IsIconic($Hwnd) }
            $null = [TabFixtureWindows]::ShowWindowAsync($Hwnd, 9) # SW_RESTORE
            $null = Wait-Until 'fixture HWND to restore' { ![TabFixtureWindows]::IsIconic($Hwnd) }
        }
        $null = Wait-Until "foreground for fixture HWND $Hwnd" {
            if ([TabFixtureWindows]::GetForegroundWindow() -eq $Hwnd) { return $true }
            if ([TabFixtureWindows]::OwnedBy($app.Id) -notcontains $Hwnd) { throw "Fixture HWND $Hwnd closed while activating" }
            $null = [TabFixtureWindows]::SetForegroundWindow($Hwnd)
            return [TabFixtureWindows]::GetForegroundWindow() -eq $Hwnd
        }
    }
}

function Run-WindowCommand([IntPtr]$Hwnd, [string]$Action) {
    Use-Window $Hwnd
    $window = $Hwnd
    # These tests exercise commands in the chosen window. Activate its native
    # palette control so opening the picker does not depend on keyboard routing.
    $launcher = Find-Here $Hwnd 'Command palette'
    if ($launcher -and $launcher.Current.IsEnabled -and !$launcher.Current.IsOffscreen) {
        Invoke-Button $launcher
    } else {
        $menuLabel = 'Command palette' + [char]0x2026 + '    F7'
        Invoke-Button (Get-NativeMenuAction 'Tab menu' $menuLabel)
    }
    $field = Wait-Until "focused command palette for '$Action' in the requested HWND" {
        $element = Find-Here $Hwnd 'Search commands'
        if ($element -and $element.Current.HasKeyboardFocus) { $element }
    }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Action)
    $null = Wait-Until "invoke palette command '$Action' in the requested HWND" {
        try {
            $item = Find-Here $Hwnd $Action ([System.Windows.Automation.ControlType]::Button)
            $invoke = $null
            if ($item -and $item.Current.IsEnabled -and
                $item.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$invoke)) {
                $invoke.Invoke()
                return $true
            }
        } catch {
            $cause = $_.Exception.GetBaseException()
            $retired = $cause -is [System.Windows.Automation.ElementNotAvailableException] -or
                       ($cause -is [Runtime.InteropServices.COMException] -and $cause.HResult -eq -2147467259)
            if (!$retired) { throw }
        }
    }
    $null = Wait-Until "command palette closes after '$Action'" {
        [TabFixtureWindows]::OwnedBy($app.Id) -notcontains $Hwnd -or
            !(Find-Here $Hwnd 'Search commands')
    }
}

function Open-TabSearch([IntPtr]$Hwnd, [string]$Query, [string]$Result) {
    Use-Window $Hwnd
    $window = $Hwnd
    Press-Key 0x7b # F12, fixture Find tab shortcut
    $field = Wait-Until 'focused tab search in the requested HWND' {
        $element = Find-Here $Hwnd 'Search tabs'
        if ($element -and $element.Current.HasKeyboardFocus) { $element }
    }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Query)
    $null = Wait-Until "tab search result '$Result'" { Find-Here $Hwnd $Result }
}

function Open-WindowSearch([IntPtr]$Hwnd, [string]$Query, [string]$Result) {
    Use-Window $Hwnd
    $window = $Hwnd
    Press-Key 0x70 # F1, fixture Find window shortcut
    $field = Wait-PickerEditor 'Search windows' "F1 for '$Query'"
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Query)
    $null = Wait-Until "window search result '$Result'" { Find-Here $Hwnd $Result }
}

function Set-FixtureWindowName([IntPtr]$Hwnd, [string]$Name) {
    Run-WindowCommand $Hwnd ('Rename window' + [char]0x2026)
    $field = Wait-Until 'window name editor' { Find-Here $Hwnd 'Window name' }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Name)
    Invoke-Button (Wait-Until 'enabled Save for window name' {
        $button = Find-Here $Hwnd 'Save'
        if ($button -and $button.Current.IsEnabled) { $button }
    })
}

function Open-CrossWindowPaneSearch([IntPtr]$Hwnd, [string]$Query, [string]$Match) {
    Use-Window $Hwnd
    $window = $Hwnd
    Press-Key 0x75 # F6, fixture Find pane shortcut
    $field = Wait-PickerEditor 'Find pane' "F6 for '$Query'"
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Query)
    $null = Wait-Until "one pane matching '$Match'" {
        $rows = @(Get-PickerRows)
        $rows.Count -eq 1 -and $rows[0] -like "*$Match*"
    }
}

function Wait-RemotePreview([IntPtr]$Hwnd, [string]$Marker, [long]$WindowId) {
    $text = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    $null = Wait-Until "preview from Window $WindowId containing '$Marker'" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($Hwnd)
        foreach ($node in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text)) {
            $name = $node.Current.Name
            if ($name.StartsWith('Preview of ') -and $name.Contains("Window $WindowId") -and $name.Contains($Marker)) {
                return $true
            }
        }
    }
}

function Open-WindowTextSearch([IntPtr]$Hwnd, [int]$LocalPane, [int]$RemotePane, [long]$RemoteWindow) {
    Use-Window $Hwnd
    $window = $Hwnd
    Press-Key 0x73 # F4, fixture Find in all panes shortcut
    $field = Wait-PickerEditor 'Find in all panes' 'the fixture text search shortcut'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('fleet needle')
    $null = Wait-Until 'local PowerShell text and hidden sibling SSH history in order' {
        $rows = @(Get-PickerRows)
        $rows.Count -eq 2 -and $rows[0] -like "Pane $LocalPane *1 match*local fleet needle*" -and
            $rows[1] -like "Pane $RemotePane *Window $RemoteWindow*1 match*old fleet needle*"
    }
}

function Choose-WindowPanePlacement([IntPtr]$Hwnd, [long]$WindowId, [string]$Label) {
    Use-Window $Hwnd
    $window = $Hwnd
    Press-Key 0x72 # F3, fixture Move pane to existing tab shortcut
    $field = Wait-PickerEditor 'Find destination pane' 'the fixture pane move shortcut'
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue("Window $WindowId $Label")
    $null = Wait-Until 'only the intended sibling pane matches' {
        $rows = @(Get-PickerRows)
        $rows.Count -eq 1 -and $rows[0] -like "*$Label*Window $WindowId*"
    }
    Press-Key 13
    $null = Wait-Until 'the native placement controls' { Find-Here $Hwnd 'Move right' }
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

function Expect-Focus([IntPtr]$Hwnd, [int]$Pane, [string]$Stage) {
    try {
        $null = Wait-Until "$Stage (pane $Pane, HWND $Hwnd)" {
            $active = @((Control @('list')).sessions | Where-Object active)
            $active.Count -eq 1 -and $active[0].id -eq $Pane -and
                [TabFixtureWindows]::GetForegroundWindow() -eq $Hwnd
        }
    } catch {
        $panes = (Control @('list')).sessions | Select-Object id,label,window_id,tab_id,active,visible,cols,rows
        throw "$Stage expected pane $Pane in HWND $Hwnd; foreground $([TabFixtureWindows]::GetForegroundWindow()); panes: $($panes | ConvertTo-Json -Compress)"
    }
}

function Shell-State([int]$Pane, [string]$Label, [IntPtr]$Hwnd = [IntPtr]::Zero, [switch]$Init) {
    $line = if ($Init) { '$global:WindowNonce=[guid]::NewGuid().ToString("N"); ' } else { '' }
    $line += "'$Label=' + " + '$PID + "/" + $global:WindowNonce'
    if ($Hwnd -eq [IntPtr]::Zero) { Send-Line $Pane $line } else { Type-In $Hwnd $line -Return }
    # The echoed source contains the label but cannot contain the evaluated marker.
    $pattern = "$Label=([0-9]+/[a-f0-9]{32})"
    try {
        $result = Control @('wait-for-text','--pane',"$Pane",'--timeout','15','--regex',$pattern)
    } catch {
        $view = Control @('get-text','--pane',"$Pane")
        throw "$_ Pane $Pane is $($view.cols) columns wide. Visible terminal text: $($view.text)"
    }
    return [regex]::Match($result.text, $pattern).Groups[1].Value
}

function Close-FixtureWindow([IntPtr]$Hwnd) {
    if ([TabFixtureWindows]::OwnedBy($app.Id) -notcontains $Hwnd) { throw "Refusing unowned HWND $Hwnd" }
    if (![FixtureMessages]::PostMessage($Hwnd, 0x10, [UIntPtr]::Zero, [IntPtr]::Zero)) {
        throw "WM_CLOSE failed for fixture HWND $Hwnd"
    }
}

function Expect-Group([int]$Count, [long]$OldWindow) {
    return Wait-Until 'both panes and their titled 3:5 split to appear in another window' {
        $all = @((Control @('list')).sessions)
        $remote = $all | Where-Object id -eq $ssh.id
        $local = $all | Where-Object id -eq $moved.id
        if ($all.Count -eq $Count -and $local -and $remote -and $remote.window_id -and
            $remote.window_id -ne $OldWindow -and $remote.window_id -eq $local.window_id -and
            $remote.tab_id -eq $ssh.tab_id -and $local.tab_id -eq $ssh.tab_id -and
            $remote.visible -and $local.visible -and $remote.connection_generation -eq $ssh.connection_generation -and
            $remote.tab_title -eq 'Cockpit tools' -and $local.tab_title -eq $remote.tab_title -and
            $remote.cols -gt 4 -and $local.cols -gt 4) {
            # Cell counts no longer reflect divider proportions when the fonts differ.
            $left = (Control @('inspect','--pane',"$($remote.id)")).pixel_width
            $right = (Control @('inspect','--pane',"$($local.id)")).pixel_width
            if ($left -gt 0 -and $right -gt 0 -and [Math]::Abs($left / ($left + $right) - 0.375) -le 0.04) {
                return $remote.window_id
            }
        }
    }
}

function Wait-DetachedWindow([IntPtr]$Source) {
    Wait-Until 'a second native window in the same process' {
        $handles = [TabFixtureWindows]::OwnedBy($app.Id)
        if ($handles.Count -eq 2 -and $handles -contains $Source) {
            $handles | Where-Object { $_ -ne $Source }
        }
    }
}

function Test-PaneInNewWindow {
    Use-Window $origin
    $window = $origin
    Press-Key 0x77 # F8 focuses the PowerShell half of the split.
    Expect-Focus $origin $moved.id 'Right PowerShell pane before detaching'
    Run-WindowCommand $origin 'Move pane to new window    F2'
    $child = Wait-DetachedWindow $origin
    Expect-Focus $child $moved.id 'The detached PowerShell pane receives focus'
    $null = Wait-Until 'only the chosen pane is in the new window' {
        $all = @((Control @('list')).sessions)
        $local = $all | Where-Object id -eq $moved.id
        $remote = $all | Where-Object id -eq $ssh.id
        $all.Count -eq 3 -and $local.window_id -ne $ssh.window_id -and $local.tab_id -ne $ssh.tab_id -and
            $remote.window_id -eq $ssh.window_id -and $remote.tab_id -eq $ssh.tab_id -and
            $remote.tab_title -ceq 'Cockpit tools' -and $remote.connection_generation -eq $ssh.connection_generation
    }
    if ((Shell-State $moved.id 'NW' $child) -ne $localIdentity) { throw 'Detaching restarted the PowerShell process' }
    Use-Window $origin
    Expect-Focus $origin $ssh.id 'The SSH sibling stays in its original tab'
    Type-In $origin 'ssh-stays/'
    Expect-Text $ssh.id 'ssh-stays/'

    Choose-WindowPanePlacement $child $ssh.window_id 'window-ssh'
    $window = $child
    Press-Key 39
    $null = Wait-Until 'the empty child closes after joining the original split' {
        [TabFixtureWindows]::OwnedBy($app.Id) -notcontains $child
    }
    Expect-Focus $origin $moved.id 'PowerShell returns to the original tab'
    $window = $origin
    Press-Key 0x78 # F9 focuses the SSH pane again.
    Expect-Focus $origin $ssh.id 'Left SSH pane before detaching'
    Press-Key 0x71 # F2 runs the action directly.
    $child = Wait-DetachedWindow $origin
    Expect-Focus $child $ssh.id 'The remapped action detaches the active SSH pane'
    $null = Wait-Until 'the SSH pane keeps its original channel while the two shells stay behind' {
        $all = @((Control @('list')).sessions)
        $remote = $all | Where-Object id -eq $ssh.id
        $all.Count -eq 3 -and $remote.window_id -ne $ssh.window_id -and
            $remote.connection_generation -eq $ssh.connection_generation -and
            @($all | Where-Object { $_.id -in $moved.id,$anchor.id -and $_.window_id -eq $ssh.window_id }).Count -eq 2
    }
    Expect-Text $ssh.id 'ssh-stays/'
    Type-In $child 'ssh-moved/'
    Expect-Text $ssh.id 'ssh-moved/'
    Close-FixtureWindow $child
    $null = Wait-Until 'closing the SSH window leaves both original PowerShell sessions' {
        $all = @((Control @('list')).sessions)
        $all.Count -eq 2 -and $moved.id -in $all.id -and $anchor.id -in $all.id
    }
    if ((Shell-State $moved.id 'RJ') -ne $localIdentity -or (Shell-State $anchor.id 'AN') -ne $anchorIdentity) {
        throw 'The original local shell identities changed'
    }
}

function Open-ProfileWindowPicker([IntPtr]$Source, [switch]$FromPalette) {
    $action = 'Open profile in new window' + [char]0x2026
    $window = $Source
    if ($FromPalette) {
        Run-WindowCommand $Source $action
    } else {
        Use-Window $Source
        Invoke-Button (Get-NativeMenuAction 'New tab' $action)
    }
    Wait-PickerEditor 'Search profiles for a new window' 'the new-window profile picker'
}

function Test-ProfileInNewWindow {
    $field = Open-ProfileWindowPicker $origin
    $window = $origin
    Press-Key 27
    $null = Wait-Until 'Escape dismisses the new-window profile picker' {
        !(Find-Here $origin 'Search profiles for a new window')
    }
    Expect-Focus $origin $ssh.id 'Escape keeps the original SSH pane'
    if ([TabFixtureWindows]::OwnedBy($app.Id).Count -ne 1) { throw 'Escape opened a window' }

    $field = Open-ProfileWindowPicker $origin
    $value = [System.Windows.Automation.ValuePattern]::Pattern
    $field.GetCurrentPattern($value).SetValue('powershell local')
    $null = Wait-Until 'the PowerShell profile appears in the new window picker' {
        Find-Here $origin ('Destination PowerShell ' + [char]0x00b7 + ' Local')
    }
    Press-Key 13
    $localWindow = Wait-DetachedWindow $origin
    $local = Wait-Until 'the new PowerShell pane is on the original control pipe' {
        (Control @('list')).sessions | Where-Object {
            $_.label -eq 'Destination PowerShell' -and $_.window_id -ne $ssh.window_id
        }
    }
    Expect-Focus $localWindow $local.id 'The PowerShell profile opens in the new window'
    Expect-Text $local.id 'PS '
    $null = Wait-Until 'the new window restores the saved PowerShell cell grid' {
        $grid = Control @('inspect','--pane',"$($local.id)")
        $grid.cols -eq 96 -and $grid.rows -eq 28
    }
    $localProof = Shell-State $local.id 'freshLocal' -Init
    if ($localProof -in $localIdentity,$anchorIdentity) { throw 'A source PowerShell process was reused' }

    $field = Open-ProfileWindowPicker $origin -FromPalette
    $target = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'window-ssh'
    $field.GetCurrentPattern($value).SetValue("$($target.port) $($target.user)")
    $null = Wait-Until 'the SSH profile matches its address' {
        Find-Here $origin ("window-ssh " + [char]0x00b7 + " $($target.user)@$($target.host):$($target.port)")
    }
    Press-Key 13
    $remoteWindow = Wait-Until 'a third native window in the same process' {
        $handles = [TabFixtureWindows]::OwnedBy($app.Id)
        if ($handles.Count -eq 3 -and $handles -contains $origin -and $handles -contains $localWindow) {
            $handles | Where-Object { $_ -notin @($origin,$localWindow) }
        }
    }
    $remote = Wait-Until 'the new SSH pane has its own window and its login is ready' {
        (Control @('list')).sessions | Where-Object {
            $_.label -eq 'window-ssh' -and $_.id -ne $ssh.id -and
                $_.window_id -notin @($ssh.window_id,$local.window_id) -and $_.connection_generation -gt 0
        }
    }
    Expect-Focus $remoteWindow $remote.id 'The SSH profile opens in the third window'
    Expect-Text $remote.id 'ready'
    Type-In $remoteWindow 'fresh-window/'
    Expect-Text $remote.id 'fresh-window/'
    if ((Control @('get-text','--pane',"$($ssh.id)")).text.Contains('fresh-window/')) {
        throw 'New-window input reached the original SSH channel'
    }

    Use-Window $origin
    Expect-Focus $origin $ssh.id 'Each new-window launch keeps the source selection'
    $original = (Control @('list')).sessions | Where-Object id -eq $ssh.id
    if ($original.tab_id -ne $ssh.tab_id -or $original.window_id -ne $ssh.window_id -or
        $original.connection_generation -ne $ssh.connection_generation -or
        (Shell-State $moved.id 'originalLocal') -ne $localIdentity) {
        throw 'A launch changed the original layout or a source process'
    }
    Type-In $origin 'source-window/'
    Expect-Text $ssh.id 'source-window/'
    Close-FixtureWindow $origin
    $null = Wait-Until 'both new windows outlive their source' {
        $panes = @((Control @('list')).sessions)
        $panes.Count -eq 2 -and $remote.id -in $panes.id -and $local.id -in $panes.id
    }
    if ((Shell-State $local.id 'stillLocal') -ne $localProof) { throw 'The new PowerShell process restarted' }
    Expect-Text $remote.id 'fresh-window/'
    Close-FixtureWindow $localWindow
    Close-FixtureWindow $remoteWindow
}

function Test-WorkspaceInNewWindow {
    Run-WindowCommand $origin ('Workspaces' + [char]0x2026)
    Invoke-Button (Wait-Until 'the broken workspace offers a new window' {
        Find-Here $origin 'Open workspace broken-copy in new window'
    })
    $null = Wait-Until 'workspace preflight shows the missing profile in the original window' {
        Find-Here $origin 'Unknown workspace profile missing-window-profile'
    }
    if ([TabFixtureWindows]::OwnedBy($app.Id).Count -ne 1 -or @((Control @('list')).sessions).Count -ne 3) {
        throw 'An invalid workspace created a window or started a pane'
    }
    if (!(Find-Here $origin 'Open workspace two-windows in new tabs')) {
        throw 'The existing append action is missing'
    }

    Invoke-Button (Wait-Until 'the saved workspace offers a new window' {
        Find-Here $origin 'Open workspace two-windows in new window'
    })
    $child = Wait-DetachedWindow $origin
    $copy = @(Wait-Until 'all three fresh panes and their two tabs are registered' {
        $panes = @((Control @('list')).sessions)
        $new = @($panes | Where-Object window_id -ne $ssh.window_id)
        if ($panes.Count -eq 6 -and $new.Count -eq 3 -and
            @($new.window_id | Sort-Object -Unique).Count -eq 1 -and
            @($new.tab_id | Sort-Object -Unique).Count -eq 2 -and
            !($new | Where-Object { $_.tab_id -in @($ssh.tab_id,$anchor.tab_id) })) { $new }
    })
    $copyAnchor = $copy | Where-Object label -eq 'Source PowerShell'
    $copyShell = $copy | Where-Object label -eq 'Moved PowerShell'
    $copySsh = $copy | Where-Object label -eq 'window-ssh'
    if (!$copyAnchor -or !$copyShell -or !$copySsh -or $copyShell.tab_id -ne $copySsh.tab_id -or
        $copySsh.tab_title -cne 'Cockpit tools') { throw 'The workspace copy lost its saved split or title' }
    Expect-Focus $child $copyAnchor.id 'The new window restores the saved active tab'
    Expect-Text $copyAnchor.id 'PS '
    if ((Shell-State $copyAnchor.id 'workspaceCopy' -Init) -eq $anchorIdentity) {
        throw 'The copied workspace reused the original PowerShell process'
    }
    $discovery = Get-Content -Raw -LiteralPath $manifest | ConvertFrom-Json
    $claims = @($discovery.workspace_windows | Where-Object window_id -in @($ssh.window_id,$copySsh.window_id))
    if ($claims.Count -ne 2 -or @($claims.workspace | Select-Object -Unique).Count -ne 1 -or
        !$claims[0].workspace) { throw 'The copy did not register the same workspace key' }

    Run-WindowCommand $child ('Workspaces' + [char]0x2026)
    Invoke-Button (Wait-Until 'the copy can select its own workspace' {
        Find-Here $child 'Switch to workspace two-windows'
    })
    Expect-Focus $child $copyAnchor.id 'Switch workspace from a registered copy stays in that window'
    $childSplit = Wait-Until 'the restored split header' { Find-Here $child 'Cockpit tools' }
    $childSplit.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Expect-Focus $child $copySsh.id 'The copy also restores the focused SSH split'
    Expect-Text $copySsh.id 'ready'
    $null = Wait-Until 'the copied split restores the saved asymmetric divider' {
        $panes = (Control @('list')).sessions
        $left = $panes | Where-Object id -eq $copySsh.id
        $right = $panes | Where-Object id -eq $copyShell.id
        $left.visible -and $right.visible -and $right.cols -gt $left.cols -and
            [Math]::Abs($left.cols / ($left.cols + $right.cols) - 0.375) -lt 0.04
    }
    Type-In $child 'workspace-copy/'
    Expect-Text $copySsh.id 'workspace-copy/'
    if ((Control @('get-text','--pane',"$($ssh.id)")).text.Contains('workspace-copy/')) {
        throw 'The copied workspace sent input to the original SSH channel'
    }

    Close-FixtureWindow $child
    $null = Wait-Until 'the original panes outlive the copied workspace' {
        @((Control @('list')).sessions).Count -eq 3
    }
    Use-Window $origin
    $sourceSplit = Wait-Until 'the original split header' { Find-Here $origin 'Cockpit tools' }
    $sourceSplit.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Expect-Focus $origin $ssh.id 'The original workspace remains usable'
    Type-In $origin 'original-workspace/'
    Expect-Text $ssh.id 'original-workspace/'
}

function Test-ManageWorkspace {
    $window = $origin
    $workspaceCommand = 'Workspaces' + [char]0x2026
    $managedName = 'Cockpit desk'
    $catalog = Join-Path $env:W11TERM_DATA_DIR 'workspaces.json'
    $value = [System.Windows.Automation.ValuePattern]::Pattern
    $originalClaim = Wait-Until 'the legacy workspace registers its original window' {
        (Get-Content -Raw -LiteralPath $manifest | ConvertFrom-Json).workspace_windows |
            Where-Object { $_.window_id -eq $ssh.window_id -and $_.workspace }
    }

    $field = Open-ProfileWindowPicker $origin -FromPalette
    $field.GetCurrentPattern($value).SetValue('Destination PowerShell')
    $null = Wait-Until 'the management window profile' {
        Find-Here $origin ('Destination PowerShell ' + [char]0x00b7 + ' Local')
    }
    Press-Key 13
    $manager = Wait-DetachedWindow $origin
    Run-WindowCommand $manager $workspaceCommand
    Invoke-Button (Wait-Until 'manage the existing saved layout' {
        Find-Here $manager 'Manage workspace two-windows'
    })

    $field = Wait-Until 'the rename input' { Find-Here $manager 'New workspace name' }
    $field.GetCurrentPattern($value).SetValue('broken-copy')
    Invoke-Button (Wait-Until 'rename is enabled' {
        $button = Find-Here $manager 'Rename workspace'
        if ($button -and $button.Current.IsEnabled) { $button }
    })
    $null = Wait-Until 'the existing name is refused' {
        Find-Here $manager 'Workspace broken-copy already exists'
    }
    $field = Find-Here $manager 'New workspace name'
    $field.GetCurrentPattern($value).SetValue($managedName)
    $null = Wait-Until 'the updated rename value reaches the editor' {
        (Find-Here $manager 'New workspace name').GetCurrentPattern($value).Current.Value -ceq $managedName
    }
    Invoke-Button (Find-Here $manager 'Rename workspace')
    $null = Wait-Until 'the list reflects the new name' {
        (Find-Here $manager "Switch to workspace $managedName") -and
            !(Find-Here $manager 'Switch to workspace two-windows')
    }
    $saved = (Get-Content -Raw -LiteralPath $catalog | ConvertFrom-Json).workspaces
    if (!$saved.$managedName -or !$saved.'broken-copy' -or $saved.'two-windows') {
        throw 'Renaming did not preserve both saved layouts correctly'
    }
    $claim = (Get-Content -Raw -LiteralPath $manifest | ConvertFrom-Json).workspace_windows |
        Where-Object window_id -eq $ssh.window_id
    if (!$claim -or $claim.workspace -cne $originalClaim.workspace) {
        throw 'Renaming changed the identity of the live workspace'
    }
    Invoke-Button (Find-Here $manager "Switch to workspace $managedName")
    Expect-Focus $origin $ssh.id 'Switching by the new name returns to the original SSH pane'
    if ([TabFixtureWindows]::OwnedBy($app.Id).Count -ne 2 -or @((Control @('list')).sessions).Count -ne 4) {
        throw 'Switching after rename reopened panes'
    }

    Run-WindowCommand $origin $workspaceCommand
    Invoke-Button (Wait-Until 'open management under the renamed workspace' {
        Find-Here $origin "Manage workspace $managedName"
    })
    $review = 'Remove saved workspace' + [char]0x2026
    Invoke-Button (Wait-Until 'review removal' { Find-Here $origin $review })
    $null = Wait-Until 'separate workspace removal confirmation' {
        Find-Here $origin "Remove saved workspace ${managedName}?"
    }
    Invoke-Button (Find-Here $origin 'Cancel')
    Invoke-Button (Wait-Until 'the editor is restored after canceling' { Find-Here $origin $review })
    $saved = (Get-Content -Raw -LiteralPath $catalog | ConvertFrom-Json).workspaces
    if (!$saved.$managedName) { throw 'Canceling confirmation removed the workspace' }
    Invoke-Button (Wait-Until 'confirm removal of the saved layout' { Find-Here $origin 'Remove workspace' })
    $null = Wait-Until 'the removed name is absent from the list' {
        (Find-Here $origin "Removed saved workspace $managedName") -and
            !(Find-Here $origin "Switch to workspace $managedName") -and
            (Find-Here $origin 'Switch to workspace broken-copy')
    }
    $saved = (Get-Content -Raw -LiteralPath $catalog | ConvertFrom-Json).workspaces
    if ($saved.$managedName -or !$saved.'broken-copy') { throw 'Removing the saved layout changed the wrong entries' }
    $split = Find-Here $origin 'Cockpit tools'
    $split.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Expect-Focus $origin $ssh.id 'Removing the layout leaves the original terminal usable'
    Type-In $origin 'after-management/'
    Expect-Text $ssh.id 'after-management/'
    if ((Shell-State $moved.id 'mgmt') -ne $localIdentity) {
        throw 'Workspace management restarted the original PowerShell process'
    }
    Close-FixtureWindow $manager
    Close-FixtureWindow $origin
}

function Invoke-WorkspaceButton([IntPtr]$Hwnd, [string]$Name) {
    Invoke-Button (Wait-Until "enabled workspace action '$Name'" {
        $button = Find-Here $Hwnd $Name
        if ($button -and $button.Current.IsEnabled) { $button }
    })
}

function Test-ExplicitControlWindow {
    $window = $origin
    $field = Open-ProfileWindowPicker $origin -FromPalette
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Destination PowerShell')
    $null = Wait-Until 'local profile for the window target' {
        Find-Here $origin ('Destination PowerShell ' + [char]0x00b7 + ' Local')
    }
    Press-Key 13
    $child = Wait-DetachedWindow $origin
    $initialChild = Wait-Until 'the new window has a local terminal' {
        (Control @('list')).sessions | Where-Object label -eq 'Destination PowerShell'
    }
    $targetId = $initialChild.window_id
    Set-FixtureWindowName $child 'Automation helper'
    $null = Wait-Until 'the named child is published before opening Settings' {
        (Control @('list-windows')).windows |
            Where-Object { $_.window_id -eq $targetId -and $_.name -ceq 'Automation helper' }
    }
    Run-WindowCommand $child 'Settings'
    $null = Control @('close-pane','--pane',"$($initialChild.id)")
    try {
        $null = Wait-Until 'window discovery includes the named utility-only window' {
            $windows = @((Control @('list-windows')).windows)
            $target = $windows | Where-Object window_id -eq $targetId
            $windows.Count -eq 2 -and $target.name -ceq 'Automation helper' -and
                $target.active_tab_title -ceq 'Settings' -and $target.tab_count -eq 1 -and $target.pane_count -eq 0
        }
    } catch {
        throw "$_ Windows: $(Control @('list-windows') | ConvertTo-Json -Depth 4 -Compress)"
    }
    Use-Window $origin
    Expect-Focus $origin $ssh.id 'The source SSH pane is foreground for CLI tests'

    $local = Control @('open','--window',"$targetId",'--profile','Destination PowerShell')
    if ($local.window_id -ne $targetId -or $local.active) {
        throw 'The background open did not target the utility window without selecting the tab'
    }
    $null = Wait-Until 'the background open keeps Settings selected' {
        $windows = (Control @('list-windows')).windows
        $target = $windows | Where-Object window_id -eq $targetId
        $source = $windows | Where-Object window_id -eq $ssh.window_id
        $target.active_tab_title -ceq 'Settings' -and $target.tab_count -eq 2 -and
            $target.pane_count -eq 1 -and !$target.active -and $source.active
    }
    Expect-Text $local.pane 'PS '
    $null = Shell-State $local.pane 'explicitLocal' -Init
    Expect-Focus $origin $ssh.id 'The background open preserves foreground and SSH focus'

    $remote = Control @('open','--window',"$targetId",'--profile','window-ssh','--activate')
    if ($remote.window_id -ne $targetId -or !$remote.active) {
        throw 'Activate did not select the new SSH tab in its target window'
    }
    $null = Wait-Until 'the specified background window selects its new terminal' {
        $windows = (Control @('list-windows')).windows
        $target = $windows | Where-Object window_id -eq $targetId
        $pane = (Control @('list')).sessions | Where-Object id -eq $remote.pane
        $target.active_tab_title -ceq 'window-ssh' -and $target.tab_count -eq 3 -and
            $target.pane_count -eq 2 -and !$target.active -and
            $pane.window_id -eq $targetId -and $pane.visible -and !$pane.active
    }
    Expect-Text $remote.pane 'ready'
    $null = Control @('send-text','--pane',"$($remote.pane)",'--no-paste','targeted-ssh/')
    Expect-Text $remote.pane 'targeted-ssh/'
    Expect-Focus $origin $ssh.id 'Target-local activation preserves desktop focus'
    Type-In $origin 'original-ssh/'
    Expect-Text $ssh.id 'original-ssh/'

    Close-FixtureWindow $child
    $null = Wait-Until 'the closed target disappears from window discovery' {
        $windows = @((Control @('list-windows')).windows)
        $windows.Count -eq 1 -and $windows[0].window_id -eq $ssh.window_id
    }
    $failed = & $Cli --pid $app.Id open --window "$targetId" --profile 'Destination PowerShell' | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $failed.ok -or $failed.error -cne "unknown window $targetId") {
        throw 'An explicit open against a closed window did not return the expected error'
    }
    if (@((Control @('list')).sessions).Count -ne 3) { throw 'A closed-window request fell back to the original window' }
    Close-FixtureWindow $origin
}

function Test-ControlProfileWindows {
    Use-Window $origin
    Expect-Focus $origin $ssh.id 'The source SSH pane is foreground before profile discovery'
    $saved = Control @('list-profiles')
    $remoteEntry = $saved.profiles | Where-Object name -ceq 'window-ssh'
    $localEntry = $saved.profiles | Where-Object name -ceq 'Destination PowerShell'
    $wsl = $saved.profiles | Where-Object name -ceq 'Fixture WSL'
    $target = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.'window-ssh'
    if ($saved.startup_profile -cne 'Destination PowerShell' -or
        ($saved.profiles[0..2].name -join ',') -cne 'Destination PowerShell,Fixture WSL,window-ssh' -or
        $remoteEntry.backend -cne 'ssh' -or !$remoteEntry.pinned -or
        $remoteEntry.host -cne $target.host -or $remoteEntry.user -cne $target.user -or $remoteEntry.port -ne $target.port -or
        $localEntry.backend -cne 'local' -or $null -ne $localEntry.wsl_distribution -or
        $wsl.backend -cne 'local' -or $wsl.wsl_distribution -cne 'Fixture') {
        throw "Unexpected saved profile listing: $($saved | ConvertTo-Json -Depth 5 -Compress)"
    }
    if ((($remoteEntry.PSObject.Properties.Name | Sort-Object) -join ',') -cne 'backend,host,name,pinned,port,user' -or
        (($localEntry.PSObject.Properties.Name | Sort-Object) -join ',') -cne 'backend,name,pinned,wsl_distribution') {
        throw 'Profile discovery returned fields beyond its public launch metadata'
    }
    $failed = & $Cli --pid $app.Id open --profile 'missing profile' --new-window | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $failed.ok -or $failed.error -cne 'unknown profile missing profile') {
        throw 'Opening a missing profile did not report the profile name'
    }
    $invalidGrid = & $Cli --pid $app.Id open --profile 'window-ssh' --new-window --cols 0 | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $invalidGrid.ok -or $invalidGrid.error -notlike '*columns*') {
        throw 'A new profile window accepted an invalid grid'
    }
    if (@((Control @('list-windows')).windows).Count -ne 1) { throw 'An invalid launch opened a window' }

    $local = Control @('open','--profile','Destination PowerShell','--new-window')
    $localWindow = Wait-DetachedWindow $origin
    $null = Wait-Until 'PowerShell restores its saved cell grid in a background window' {
        $grid = Control @('inspect','--pane',"$($local.pane)")
        $grid.cols -eq 96 -and $grid.rows -eq 28
    }
    Expect-Text $local.pane 'PS '
    $freshLocal = Shell-State $local.pane 'cliFreshLocal' -Init
    if (!$local.active -or $freshLocal -in $localIdentity,$anchorIdentity) {
        throw 'The new window did not create and select an independent PowerShell process'
    }
    Expect-Focus $origin $ssh.id 'Restoring saved profile geometry leaves desktop focus alone'

    $remote = Control @('open','--profile','window-ssh','--new-window','--cols','88','--rows','27')
    $remoteWindow = Wait-Until 'a third same-process HWND for SSH' {
        [TabFixtureWindows]::OwnedBy($app.Id) | Where-Object { $_ -notin @($origin,$localWindow) }
    }
    if ($remote.window_id -in @($ssh.window_id,$local.window_id) -or
        !$remote.active -or $remote.cols -ne 88 -or $remote.rows -ne 27) {
        throw "Unexpected SSH open reply: $($remote | ConvertTo-Json -Compress)"
    }
    $windowDetails = (Control @('list-windows')).windows | Where-Object window_id -eq $remote.window_id
    if ($windowDetails.pane_count -ne 1 -or $windowDetails.active -or
        $windowDetails.active_tab_title -cne 'window-ssh') {
        throw 'The SSH window was not ready with the named profile when the CLI returned'
    }
    $null = Wait-Until 'the SSH canvas keeps the requested grid' {
        $grid = Control @('inspect','--pane',"$($remote.pane)")
        $grid.cols -eq 88 -and $grid.rows -eq 27
    }
    Expect-Text $remote.pane 'ready'
    $null = Control @('send-text','--pane',"$($remote.pane)",'--no-paste','fresh-profile/')
    Expect-Text $remote.pane 'fresh-profile/'
    Expect-Focus $origin $ssh.id 'Creating both new profile windows preserves the original keyboard focus'
    Type-In $origin 'source-profile/'
    Expect-Text $ssh.id 'source-profile/'

    Close-FixtureWindow $origin
    $null = Wait-Until 'both new profile windows retain the same pipe after closing the source' {
        $panes = @((Control @('list')).sessions)
        $panes.Count -eq 2 -and $remote.pane -in $panes.id -and $local.pane -in $panes.id
    }
    Close-FixtureWindow $localWindow
    $null = Wait-Until 'the remaining SSH window still owns the control pipe' {
        $windows = @((Control @('list-windows')).windows)
        $windows.Count -eq 1 -and $windows[0].window_id -eq $remote.window_id
    }
    Close-FixtureWindow $remoteWindow
}

function Test-ControlWorkspace {
    $window = $origin
    $field = Open-ProfileWindowPicker $origin -FromPalette
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Destination PowerShell')
    $null = Wait-Until 'local profile to open in the destination window' {
        Find-Here $origin ('Destination PowerShell ' + [char]0x00b7 + ' Local')
    }
    Press-Key 13
    $child = Wait-DetachedWindow $origin
    $initialChild = Wait-Until 'the destination window has a local terminal' {
        (Control @('list')).sessions | Where-Object label -eq 'Destination PowerShell'
    }
    $targetId = $initialChild.window_id
    Use-Window $origin
    Expect-Focus $origin $ssh.id 'The source SSH pane is foreground before workspace restore'

    $catalog = @((Control @('list-workspaces')).workspaces)
    $mixed = $catalog | Where-Object name -ceq 'two-windows'
    if ($catalog.Count -ne 3 -or $mixed.tab_count -ne 2 -or $mixed.pane_count -ne 3) {
        throw "Unexpected workspace catalog: $($catalog | ConvertTo-Json -Compress)"
    }
    $failed = & $Cli --pid $app.Id open-workspace --window "$targetId" --name 'broken-copy' | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $failed.ok -or $failed.error -notlike '*missing-window-profile*') {
        throw 'Opening a workspace with a missing profile did not report the profile'
    }
    $target = (Control @('list-windows')).windows | Where-Object window_id -eq $targetId
    if ($target.tab_count -ne 1 -or $target.pane_count -ne 1) {
        throw 'The failed restore appended a partial layout'
    }

    $restored = Control @('open-workspace','--window',"$targetId",'--name','two-windows')
    $remote = $restored.tabs[0].panes[0]
    $local = $restored.tabs[0].panes[1]
    if ($restored.name -cne 'two-windows' -or $restored.window_id -ne $targetId -or $restored.active -or
        @($restored.tabs).Count -ne 2 -or @($restored.tabs[0].panes).Count -ne 2 -or
        @($restored.tabs[1].panes).Count -ne 1 -or $restored.workspace_active_pane -ne $remote) {
        throw "Unexpected workspace restore reply: $($restored | ConvertTo-Json -Depth 5 -Compress)"
    }
    $null = Wait-Until 'the destination keeps its selection while restoring the workspace' {
        $target = (Control @('list-windows')).windows | Where-Object window_id -eq $targetId
        $target.active_tab_title -ceq 'Destination PowerShell' -and $target.tab_count -eq 3 -and $target.pane_count -eq 4
    }
    $panes = @((Control @('list')).sessions | Where-Object {
        $_.window_id -eq $targetId -and $_.tab_id -eq $restored.tabs[0].tab_id
    })
    if ($panes.Count -ne 2 -or $remote -notin $panes.id -or $local -notin $panes.id) {
        throw 'The returned split pane IDs do not belong to the restored tab in the destination'
    }
    Expect-Text $remote 'ready'
    Expect-Text $local 'PS '
    $null = Shell-State $local 'workspaceCliLocal' -Init
    $null = Control @('send-text','--pane',"$remote",'--no-paste','targeted-workspace/')
    Expect-Text $remote 'targeted-workspace/'

    $selected = Control @('open-workspace','--window',"$targetId",'--name','Local Ops','--activate')
    if (!$selected.active -or $selected.workspace_active_pane -ne $selected.tabs[0].panes[0]) {
        throw 'Activation did not return the saved active pane'
    }
    $null = Wait-Until 'the restored local tab is selected in the background window' {
        $target = (Control @('list-windows')).windows | Where-Object window_id -eq $targetId
        $selectedPane = (Control @('list')).sessions | Where-Object id -eq $selected.workspace_active_pane
        $target.active_tab_title -ceq 'Local Ops shell' -and !$target.active -and $selectedPane.visible
    }
    Expect-Focus $origin $ssh.id 'Restoring and activating a workspace leaves desktop focus alone'
    Type-In $origin 'original-workspace/'
    Expect-Text $ssh.id 'original-workspace/'
    Close-FixtureWindow $child
    Close-FixtureWindow $origin
}

function Test-ControlWorkspaceWindow {
    Use-Window $origin
    $sourceSplit = Wait-Until 'original SSH split' { Find-Here $origin 'Cockpit tools' }
    $sourceSplit.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Expect-Focus $origin $ssh.id 'The source SSH pane is foreground before the CLI opens a window'

    $background = Control @('open-workspace','--new-window','--name','Background local')
    $backgroundWindow = Wait-DetachedWindow $origin
    Expect-Text $background.workspace_active_pane 'PS '
    Expect-Focus $origin $ssh.id 'The local workspace opens without activation'
    Close-FixtureWindow $backgroundWindow
    $null = Wait-Until 'closing an unactivated window leaves the original pipe running' {
        $handles = [TabFixtureWindows]::OwnedBy($app.Id)
        $handles.Count -eq 1 -and $handles -contains $origin -and @((Control @('list')).sessions).Count -eq 3
    }

    $failed = & $Cli --pid $app.Id open-workspace --new-window --name 'broken-copy' | ConvertFrom-Json
    if ($LASTEXITCODE -eq 0 -or $failed.ok -or $failed.error -notlike '*missing-window-profile*') {
        throw 'Opening an invalid workspace in a new window did not name the missing profile'
    }
    if (@((Control @('list-windows')).windows).Count -ne 1 -or @((Control @('list')).sessions).Count -ne 3) {
        throw 'An invalid workspace added a window or panes'
    }

    $restored = Control @('open-workspace','--new-window','--name','two-windows')
    $id = $restored.window_id
    $remote = $restored.tabs[0].panes[0]
    $shell = $restored.tabs[0].panes[1]
    $selected = $restored.tabs[1].panes[0]
    if ($id -eq $ssh.window_id -or !$restored.active -or $restored.workspace_active_pane -ne $selected -or
        @($restored.tabs).Count -ne 2 -or @($restored.tabs[0].panes).Count -ne 2 -or
        @($restored.tabs[1].panes).Count -ne 1) {
        throw "Unexpected new workspace reply: $($restored | ConvertTo-Json -Depth 5 -Compress)"
    }
    $child = Wait-DetachedWindow $origin
    $null = Wait-Until 'the background window lays out its native tabs without activation' {
        $tab = Find-Here $child 'Source PowerShell' ([System.Windows.Automation.ControlType]::TabItem)
        $tab -and $tab.Current.BoundingRectangle.Width -gt 0
    }
    Expect-Focus $origin $ssh.id 'The new workspace leaves keyboard focus on the source SSH pane'
    $windows = @((Control @('list-windows')).windows)
    $copy = $windows | Where-Object window_id -eq $id
    if ($windows.Count -ne 2 -or $copy.tab_count -ne 2 -or $copy.pane_count -ne 3 -or
        $copy.active -or $copy.active_tab_title -cne 'Source PowerShell') {
        throw 'The new window was not ready with the saved selection when the CLI returned'
    }
    $claims = @((Get-Content -Raw -LiteralPath $manifest | ConvertFrom-Json).workspace_windows |
        Where-Object window_id -in @($ssh.window_id,$id))
    if ($claims.Count -ne 2 -or @($claims.workspace | Select-Object -Unique).Count -ne 1 -or !$claims[0].workspace) {
        throw 'The new window did not register its workspace identity'
    }
    Expect-Text $selected 'PS '
    Expect-Text $shell 'PS '
    if ((Shell-State $selected 'workspaceWindowCli' -Init) -eq $anchorIdentity) {
        throw 'The CLI workspace reused the original PowerShell process'
    }
    Type-In $origin 'source-stays-focused/'
    Expect-Text $ssh.id 'source-stays-focused/'

    Close-FixtureWindow $origin
    $null = Wait-Until 'the new workspace retains all sessions and the shared control pipe' {
        $panes = @((Control @('list')).sessions)
        $panes.Count -eq 3 -and $remote -in $panes.id -and $shell -in $panes.id -and
            $selected -in $panes.id -and @($panes | Where-Object window_id -ne $id).Count -eq 0
    }
    Use-Window $child
    $childSplit = Wait-Until 'native split tab in the window opened from the CLI' { Find-Here $child 'Cockpit tools' }
    $childSplit.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $null = Wait-Until 'the restored SSH split is active and its window can take focus' {
        $pane = (Control @('list')).sessions | Where-Object id -eq $remote
        if ($pane.active -and $pane.visible) {
            Use-Window $child
            [TabFixtureWindows]::GetForegroundWindow() -eq $child
        }
    }
    Expect-Text $remote 'ready'
    Type-In $child 'new-window/'
    Expect-Text $remote 'new-window/'
    Close-FixtureWindow $child
}

function Test-RepairWorkspace {
    $window = $origin
    Run-WindowCommand $origin ('Workspaces' + [char]0x2026)
    Invoke-WorkspaceButton $origin 'Open workspace broken-copy in new window'
    $null = Wait-Until 'the saved layout is initially unable to resolve its profile' {
        Find-Here $origin 'Unknown workspace profile missing-window-profile'
    }
    Invoke-WorkspaceButton $origin 'Manage workspace broken-copy'
    Invoke-WorkspaceButton $origin ('Repair SSH profiles' + [char]0x2026)
    $selector = Wait-Until 'replacement is shown for the missing profile' {
        Find-Here $origin 'Replacement for missing-window-profile'
    }
    if ((Find-Here $origin 'Save replacements').Current.IsEnabled) {
        throw 'The repair offers to save before a replacement is selected'
    }
    $selector.SetFocus()
    $expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
    $selector.GetCurrentPattern($expand).Expand()
    $options = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles.'window-ssh'
    $option = 'window-ssh ' + [char]0x00b7 + " $($options.user)@$($options.host):$($options.port)"
    $select = [System.Windows.Automation.SelectionItemPattern]::Pattern
    $item = Wait-Until 'the saved SSH profile with its reviewed endpoint' {
        Find-Element $option ([System.Windows.Automation.ControlType]::ListItem) -Visible
    }
    $item.GetCurrentPattern($select).Select()
    $null = Wait-Until 'the repair is enabled after selection' {
        (Find-Here $origin 'Save replacements').Current.IsEnabled
    }
    $popup = (Find-Here $origin 'Replacement for missing-window-profile').GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
    Invoke-WorkspaceButton $origin 'Save replacements'
    $null = Wait-Until 'the repaired layout is saved' { Find-Here $origin 'Saved repaired layout broken-copy' }
    $catalog = Join-Path $env:W11TERM_DATA_DIR 'workspaces.json'
    $saved = (Get-Content -LiteralPath $catalog -Raw | ConvertFrom-Json).workspaces
    $restored = $saved.'broken-copy'
    $original = $saved.'two-windows'
    if (!$restored -or ($restored | ConvertTo-Json -Depth 12 -Compress) -cne
        ($original | ConvertTo-Json -Depth 12 -Compress) -or
        @((Control @('list')).sessions).Count -ne 3 -or [TabFixtureWindows]::OwnedBy($app.Id).Count -ne 1) {
        throw 'Repair changed the saved layout or opened sessions'
    }

    Invoke-WorkspaceButton $origin 'Open workspace broken-copy in new window'
    $child = Wait-DetachedWindow $origin
    $opened = @(Wait-Until 'the repaired layout opens both tabs and all three panes' {
        $new = @((Control @('list')).sessions | Where-Object window_id -ne $ssh.window_id)
        if ($new.Count -eq 3 -and @($new.tab_id | Sort-Object -Unique).Count -eq 2) { $new }
    })
    $repairedSsh = $opened | Where-Object label -eq 'window-ssh'
    $repairedLocal = $opened | Where-Object label -eq 'Moved PowerShell'
    if ($repairedSsh.tab_id -ne $repairedLocal.tab_id -or $repairedSsh.tab_title -cne 'Cockpit tools') {
        throw 'The repaired SSH pane lost its split or tab title'
    }
    Expect-Focus $child $repairedSsh.id 'The repaired SSH pane receives the saved focus'
    Expect-Text $repairedSsh.id 'ready'
    Expect-Text $repairedLocal.id 'PS '
    Type-In $child 'repaired-ssh/'
    Expect-Text $repairedSsh.id 'repaired-ssh/'
    Close-FixtureWindow $child
    Use-Window $origin
    (Find-Here $origin 'Cockpit tools').GetCurrentPattern($select).Select()
    Expect-Focus $origin $ssh.id 'The original SSH pane is still usable'
    Type-In $origin 'original-ssh/'
    Expect-Text $ssh.id 'original-ssh/'
    Close-FixtureWindow $origin
}

function Test-CopySavedWorkspace {
    $workspaceCommand = 'Workspaces' + [char]0x2026
    $value = [System.Windows.Automation.ValuePattern]::Pattern
    $select = [System.Windows.Automation.SelectionItemPattern]::Pattern
    $catalog = Join-Path $env:W11TERM_DATA_DIR 'workspaces.json'
    $original = (Get-Content -Raw -LiteralPath $catalog | ConvertFrom-Json).workspaces.'two-windows'
    $originalClaim = Wait-Until 'the original saved workspace claim' {
        (Get-Content -Raw -LiteralPath $manifest | ConvertFrom-Json).workspace_windows |
            Where-Object { $_.window_id -eq $ssh.window_id -and $_.workspace }
    }
    Run-WindowCommand $origin $workspaceCommand
    Invoke-WorkspaceButton $origin 'Manage workspace two-windows'
    Invoke-WorkspaceButton $origin 'Duplicate saved workspace'
    $field = Wait-Until 'the unused name is suggested for the saved copy' {
        $entry = Find-Here $origin 'Workspace copy name'
        if ($entry -and $entry.GetCurrentPattern($value).Current.Value -ceq 'two-windows copy 2') { $entry }
    }
    Invoke-WorkspaceButton $origin 'Cancel'
    Invoke-WorkspaceButton $origin 'Duplicate saved workspace'
    $field = Wait-Until 'the copy name field reopens' { Find-Here $origin 'Workspace copy name' }
    $field.GetCurrentPattern($value).SetValue('two-windows')
    $null = Wait-Until 'copying onto the original name is disabled' {
        $save = Find-Here $origin 'Save workspace copy'
        $save -and !$save.Current.IsEnabled
    }
    $field = Find-Here $origin 'Workspace copy name'
    $field.GetCurrentPattern($value).SetValue('two-windows copy')
    Invoke-WorkspaceButton $origin 'Save workspace copy'
    $null = Wait-Until 'an occupied destination name is refused' {
        Find-Here $origin 'Workspace two-windows copy already exists'
    }
    $copyName = 'Cockpit sandbox'
    $field = Find-Here $origin 'Workspace copy name'
    $field.GetCurrentPattern($value).SetValue($copyName)
    Invoke-WorkspaceButton $origin 'Save workspace copy'
    $null = Wait-Until 'the copy and original appear in the saved list' {
        (Find-Here $origin "Duplicated two-windows as $copyName") -and
            (Find-Here $origin "Switch to workspace $copyName") -and (Find-Here $origin 'Switch to workspace two-windows')
    }
    $stored = (Get-Content -Raw -LiteralPath $catalog | ConvertFrom-Json).workspaces
    $sourceAfter = $stored.'two-windows'
    if (($stored.$copyName | ConvertTo-Json -Depth 12 -Compress) -cne
            ($sourceAfter | ConvertTo-Json -Depth 12 -Compress) -or
        $sourceAfter.active_tab -ne $original.active_tab -or
        $sourceAfter.tabs[0].layout.ratio -ne $original.tabs[0].layout.ratio -or
        $stored.'two-windows copy'.tabs[0].layout.source.kind -cne 'demo' -or $stored.'two-windows copy 2') {
        throw 'The copied recipe, source recipe or existing destination changed unexpectedly'
    }
    $claims = @((Get-Content -Raw -LiteralPath $manifest | ConvertFrom-Json).workspace_windows)
    if ($claims.Count -ne 1 -or $claims[0].workspace -cne $originalClaim.workspace -or
        @((Control @('list')).sessions).Count -ne 3 -or [TabFixtureWindows]::OwnedBy($app.Id).Count -ne 1) {
        throw 'Saving a workspace copy opened panes or changed the original window registration'
    }

    Invoke-WorkspaceButton $origin "Open workspace $copyName in new window"
    $copied = Wait-DetachedWindow $origin
    $panes = @(Wait-Until 'all panes of the duplicate are open' {
        $new = @((Control @('list')).sessions | Where-Object window_id -ne $ssh.window_id)
        if ($new.Count -eq 3) { $new }
    })
    $copySsh = $panes | Where-Object label -eq 'window-ssh'
    $copyAnchor = $panes | Where-Object label -eq 'Source PowerShell'
    Expect-Focus $copied $copyAnchor.id 'The copy restores the saved active tab'
    $claims = @(Wait-Until 'both saved workspace windows are registered' {
        $records = @((Get-Content -Raw -LiteralPath $manifest | ConvertFrom-Json).workspace_windows)
        if ($records.Count -eq 2) { $records }
    })
    $copyClaim = $claims | Where-Object window_id -eq $copySsh.window_id
    $sourceClaim = $claims | Where-Object window_id -eq $ssh.window_id
    if (!$copyClaim.workspace -or $copyClaim.workspace -ceq $originalClaim.workspace -or
        $sourceClaim.workspace -cne $originalClaim.workspace) {
        throw 'The copy shares the original workspace identity'
    }
    (Find-Here $copied 'Cockpit tools').GetCurrentPattern($select).Select()
    Expect-Focus $copied $copySsh.id 'The copied split has its own SSH pane'
    Expect-Text $copySsh.id 'ready'
    Type-In $copied 'copy-recipe/'
    Expect-Text $copySsh.id 'copy-recipe/'
    Use-Window $origin
    (Find-Here $origin 'Cockpit tools').GetCurrentPattern($select).Select()
    Expect-Focus $origin $ssh.id 'The original remains available after duplication'
    Run-WindowCommand $copied $workspaceCommand
    Invoke-WorkspaceButton $copied 'Switch to workspace two-windows'
    Expect-Focus $origin $ssh.id 'Switching from the copy reaches the original window'
    Type-In $origin 'original-recipe/'
    Expect-Text $ssh.id 'original-recipe/'
    Close-FixtureWindow $copied
    Close-FixtureWindow $origin
}

function Test-WorkspaceReplacement {
    $window = $origin
    $workspaceCommand = 'Workspaces' + [char]0x2026
    $name = 'Shared desk'
    $value = [System.Windows.Automation.ValuePattern]::Pattern
    $catalog = Join-Path $env:W11TERM_DATA_DIR 'workspaces.json'
    $separator = " $([char]0x00b7) "
    $field = Open-ProfileWindowPicker $origin -FromPalette
    $field.GetCurrentPattern($value).SetValue('Destination PowerShell')
    $null = Wait-Until 'the second window profile' {
        Find-Here $origin "Destination PowerShell${separator}Local"
    }
    Press-Key 13
    $other = Wait-DetachedWindow $origin

    Run-WindowCommand $origin $workspaceCommand
    (Wait-Until 'source workspace name field' { Find-Here $origin 'Workspace name' }).
        GetCurrentPattern($value).SetValue($name)
    Run-WindowCommand $other $workspaceCommand
    (Wait-Until 'second window workspace name field' { Find-Here $other 'Workspace name' }).
        GetCurrentPattern($value).SetValue($name)
    Invoke-WorkspaceButton $other 'Save workspace'
    $null = Wait-Until 'the other window saves the name first' { Find-Here $other "Saved $name" }
    $saved = (Get-Content -Raw -LiteralPath $catalog | ConvertFrom-Json).workspaces.$name |
        ConvertTo-Json -Depth 12 -Compress

    Invoke-WorkspaceButton $origin 'Save workspace'
    $null = Wait-Until 'stale Save is refused' {
        Find-Here $origin "Workspace $name already exists. Review it before replacing it."
    }
    Invoke-WorkspaceButton $origin 'Update saved workspace'
    $null = Wait-Until 'existing and proposed layouts are presented separately' {
        (Find-Here $origin "Saved layout${separator}1 tab, 1 pane") -and
            (Find-Here $origin "Replacement layout${separator}2 tabs, 3 panes")
    }
    Invoke-WorkspaceButton $origin 'Cancel'
    $null = Wait-Until 'cancel returns to the workspace list' {
        (Find-Here $origin 'Update saved workspace') -and !(Find-Here $origin 'Replace saved workspace')
    }
    $afterCancel = (Get-Content -Raw -LiteralPath $catalog | ConvertFrom-Json).workspaces.$name |
        ConvertTo-Json -Depth 12 -Compress
    if ($afterCancel -cne $saved) { throw 'Creating an occupied name or canceling review changed the saved layout' }

    Invoke-WorkspaceButton $origin 'Update saved workspace'
    $null = Wait-Until 'the original window starts a second review' {
        Find-Here $origin 'Replace saved workspace'
    }
    Run-WindowCommand $other "New tab with Destination PowerShell${separator}Local"
    Run-WindowCommand $other $workspaceCommand
    Invoke-WorkspaceButton $other 'Update saved workspace'
    $null = Wait-Until 'the other window reviews a different layout' {
        Find-Here $other "Replacement layout${separator}2 tabs, 2 panes"
    }
    Invoke-WorkspaceButton $other 'Replace saved workspace'
    $null = Wait-Until 'the other window updates the catalog' { Find-Here $other "Updated $name" }

    Invoke-WorkspaceButton $origin 'Replace saved workspace'
    $null = Wait-Until 'stale confirmation is refused' {
        Find-Here $origin "Workspace $name changed in another window. Review it again."
    }
    Invoke-WorkspaceButton $origin 'Update saved workspace'
    $null = Wait-Until 'the current saved layout is shown before replacing it' {
        (Find-Here $origin "Saved layout${separator}2 tabs, 2 panes") -and
            (Find-Here $origin "Replacement layout${separator}2 tabs, 3 panes")
    }
    Run-WindowCommand $origin "New tab with Destination PowerShell${separator}Local"
    $null = Wait-Until 'an unreviewed shell opens in the source window' {
        (Control @('list')).sessions |
            Where-Object { $_.window_id -eq $ssh.window_id -and $_.label -eq 'Destination PowerShell' }
    }
    $workspaceTab = Find-Here $origin 'Workspaces'
    $workspaceTab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Invoke-WorkspaceButton $origin 'Replace saved workspace'
    $null = Wait-Until 'a reviewed replacement is saved' { Find-Here $origin "Updated $name" }
    $result = (Get-Content -Raw -LiteralPath $catalog | ConvertFrom-Json).workspaces.$name
    if ($result.tabs.Count -ne 2 -or $result.tabs[0].title -cne 'Cockpit tools' -or
        $result.tabs[0].layout.type -cne 'split' -or $result.tabs[1].layout.type -cne 'leaf') {
        throw 'The reviewed replacement did not preserve the expected split'
    }

    Use-Window $origin
    $sourceTab = Find-Here $origin 'Cockpit tools'
    $sourceTab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Expect-Focus $origin $ssh.id 'The replacement leaves the original SSH session running'
    Type-In $origin 'after-review/'
    Expect-Text $ssh.id 'after-review/'
    if ((Shell-State $moved.id 'review') -ne $localIdentity) {
        throw 'Updating the workspace restarted the PowerShell session'
    }
    Close-FixtureWindow $other
    Close-FixtureWindow $origin
}

function Test-MoveAllTerminalTabs {
    Use-Window $origin
    $window = $origin
    Expect-Focus $origin $ssh.id 'The SSH pane is focused before moving tabs'
    Type-In $origin 'before-bulk/'
    Expect-Text $ssh.id 'before-bulk/'
    Run-WindowCommand $origin 'Pin tab'
    Run-WindowCommand $origin 'Settings'
    $select = [System.Windows.Automation.SelectionItemPattern]::Pattern
    $sourceTab = Wait-Until 'the original pinned split tab' { Find-Here $origin 'Pinned: Cockpit tools' }
    $sourceTab.GetCurrentPattern($select).Select()
    Expect-Focus $origin $ssh.id 'The terminal tab is selected again'

    $field = Open-ProfileWindowPicker $origin -FromPalette
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('Destination PowerShell')
    $null = Wait-Until 'the PowerShell destination profile' {
        Find-Here $origin ('Destination PowerShell ' + [char]0x00b7 + ' Local')
    }
    Press-Key 13
    $destination = Wait-DetachedWindow $origin
    $local = Wait-Until 'the existing destination shell' {
        (Control @('list')).sessions | Where-Object label -eq 'Destination PowerShell'
    }
    Expect-Focus $destination $local.id 'The new destination window owns its shell'
    Expect-Text $local.id 'PS '
    $destinationIdentity = Shell-State $local.id 'dest1' -Init
    # Rename the destination to prove the palette keeps targeting its numeric window ID.
    Set-FixtureWindowName $destination 'Consolidated tools'
    $separator = " $([char]0x00b7) "
    $destinationTitle = "Consolidated tools${separator}Destination PowerShell"
    $null = Wait-Until 'the named destination is published' {
        [TabFixtureWindows]::Title($destination) -ceq "w11term${separator}$destinationTitle"
    }

    Run-WindowCommand $origin "Move all terminal tabs to Window $($local.window_id)${separator}$destinationTitle"
    Expect-Focus $destination $ssh.id 'The moved split keeps its focused SSH pane'
    $null = Expect-Group 4 $ssh.window_id
    $null = Wait-Until 'all original pane IDs are in the destination with their original tabs' {
        $panes = @((Control @('list')).sessions)
        $currentAnchor = $panes | Where-Object id -eq $anchor.id
        $currentLocal = $panes | Where-Object id -eq $local.id
        $panes.Count -eq 4 -and @($panes | Where-Object window_id -ne $local.window_id).Count -eq 0 -and
            $currentAnchor.tab_id -eq $anchor.tab_id -and $currentLocal.tab_id -eq $local.tab_id
    }
    $null = Wait-Until 'Settings remains open in the original window' {
        (Find-Here $origin 'Settings') -and !(Find-Here $origin 'Pinned: Cockpit tools') -and
            !(Find-Here $origin 'Source PowerShell')
    }
    Expect-Text $ssh.id 'before-bulk/'
    Type-In $destination 'after-bulk/'
    Expect-Text $ssh.id 'after-bulk/'
    if ((Shell-State $moved.id 'moved2') -ne $localIdentity -or
        (Shell-State $anchor.id 'anchor2') -ne $anchorIdentity -or
        (Shell-State $local.id 'dest2') -ne $destinationIdentity) {
        throw 'Moving tabs restarted a PowerShell process'
    }
    $split = Find-Here $destination 'Pinned: Cockpit tools'
    $preexisting = Find-Here $destination 'Destination PowerShell'
    $single = Find-Here $destination 'Source PowerShell'
    if (!($split.Current.BoundingRectangle.Left -lt $preexisting.Current.BoundingRectangle.Left -and
          $preexisting.Current.BoundingRectangle.Left -lt $single.Current.BoundingRectangle.Left)) {
        throw 'The pinned split or existing tab order changed incorrectly'
    }

    $prefix = "w11term$separator"
    $title = Wait-Until 'the original window title' {
        $native = [TabFixtureWindows]::Title($origin)
        if ($native.StartsWith($prefix)) { $native.Substring($prefix.Length) }
    }
    $window = $destination
    $action = "Move all terminal tabs to Window $($ssh.window_id)${separator}$title"
    Invoke-Button (Get-NativeMenuAction 'Tab menu' $action -Submenu 'Move all terminal tabs to another window')
    Expect-Focus $origin $ssh.id 'Moving back preserves the focused split'
    $null = Wait-Until 'an emptied window closes after all four panes are reclaimed' {
        $panes = @((Control @('list')).sessions)
        $panes.Count -eq 4 -and @($panes | Where-Object window_id -ne $ssh.window_id).Count -eq 0 -and
            [TabFixtureWindows]::OwnedBy($app.Id) -notcontains $destination
    }
    Type-In $origin 'after-return/'
    Expect-Text $ssh.id 'after-return/'
    if ((Shell-State $local.id 'dest3') -ne $destinationIdentity) {
        throw 'The destination shell did not survive its old window closing'
    }
}

function Test-DuplicateInNewWindow {
    Use-Window $origin
    $window = $origin
    Expect-Focus $origin $ssh.id 'The source SSH pane starts selected'
    Type-In $origin 'source-layout/'
    Expect-Text $ssh.id 'source-layout/'
    Press-Key 0x77 # F8 focuses the right split before capturing the layout.
    Expect-Focus $origin $moved.id 'PowerShell is the source layout focus'
    Run-WindowCommand $origin 'Set tab color to Blue'
    Run-WindowCommand $origin 'Pin tab'
    Run-WindowCommand $origin 'Enable read-only pane input'
    Press-Key 0x73 # F4 zooms the source PowerShell pane.
    $null = Wait-Until 'the source pane has the requested input lock and zoom' {
        $rows = (Control @('list')).sessions
        $local = $rows | Where-Object id -eq $moved.id
        $remote = $rows | Where-Object id -eq $ssh.id
        $local.active -and $local.read_only -and !$remote.visible
    }
    $anchorTab = Wait-Until 'the other tab in the source window' { Find-Here $origin 'Source PowerShell' }
    $anchorTab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Expect-Focus $origin $anchor.id 'The duplicate action targets a background tab'
    Invoke-Button (Open-Context 'Pinned: Cockpit tools (Blue tab)' 'Duplicate tab layout in new window')

    $child = Wait-DetachedWindow $origin
    $copied = Wait-Until 'the new window has two fresh panes in its only tab' {
        $all = @((Control @('list')).sessions)
        $new = @($all | Where-Object window_id -ne $ssh.window_id)
        if ($all.Count -eq 5 -and $new.Count -eq 2 -and
            @($new.window_id | Sort-Object -Unique).Count -eq 1 -and
            @($new.tab_id | Sort-Object -Unique).Count -eq 1 -and
            @($new | Where-Object { $_.tab_title -cne 'Cockpit tools' -or $_.tab_id -eq $ssh.tab_id }).Count -eq 0) { $new }
    }
    $copySsh = $copied | Where-Object label -eq 'window-ssh'
    $copyShell = $copied | Where-Object id -ne $copySsh.id
    if (!$copyShell -or !$copySsh -or !$copyShell.read_only -or $copySsh.visible) {
        throw 'The copy lost its SSH and PowerShell layouts, input lock, or zoom'
    }
    Expect-Focus $child $copyShell.id 'The copy selects the remembered PowerShell pane'
    $null = Wait-Until 'the duplicate keeps its color and starts unpinned' {
        Find-Here $child 'Cockpit tools (Blue tab)'
    }
    Expect-Text $copySsh.id 'ready'
    Expect-Text $copyShell.id 'PS '
    if ((Control @('get-text','--pane',"$($copySsh.id)")).text.Contains('source-layout/')) {
        throw 'The duplicate inherited source scrollback'
    }

    Run-WindowCommand $child 'Disable read-only pane input'
    if ((Shell-State $copyShell.id 'duplicateLocal' -Init) -eq $localIdentity) {
        throw 'The copy reused the source PowerShell process'
    }
    $window = $child
    Press-Key 0x73 # Restore the copied split and confirm its asymmetric divider.
    $null = Wait-Until 'the copied split keeps its proportions' {
        $rows = (Control @('list')).sessions
        $local = $rows | Where-Object id -eq $copyShell.id
        $remote = $rows | Where-Object id -eq $copySsh.id
        $local.visible -and $remote.visible -and $local.cols -gt $remote.cols -and
            [Math]::Abs($remote.cols / ($local.cols + $remote.cols) - 0.375) -lt 0.04
    }
    Press-Key 0x78
    Expect-Focus $child $copySsh.id 'The left copy pane is its own SSH session'
    Type-In $child 'copy-window/'
    Expect-Text $copySsh.id 'copy-window/'
    Close-FixtureWindow $child
    $null = Wait-Until 'closing the copy leaves the three original panes' {
        @((Control @('list')).sessions).Count -eq 3
    }
    Use-Window $origin
    Expect-Focus $origin $anchor.id 'The source window keeps its original selection'
    $original = (Control @('list')).sessions | Where-Object id -eq $ssh.id
    if ($original.window_id -ne $ssh.window_id -or $original.tab_id -ne $ssh.tab_id -or
        $original.connection_generation -ne $ssh.connection_generation) {
        throw 'The source SSH connection changed after duplication'
    }
    Expect-Text $ssh.id 'source-layout/'
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--workspace', 'two-windows') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    # Retain the process handle before any native close so Windows PowerShell can read its exit code.
    $processHandle = $app.Handle
    $origin = Wait-Until 'the original terminal HWND' {
        $handles = [TabFixtureWindows]::OwnedBy($app.Id)
        if ($handles.Count -eq 1) { $handles[0] }
    }
    $manifest = Get-ControlManifestPath $app.Id
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
    if ($FirstClose -eq 'PaneWindow') {
        Test-PaneInNewWindow
        return
    }
    if ($FirstClose -eq 'ProfileWindow') {
        Test-ProfileInNewWindow
        if (!$app.WaitForExit(10000) -or (Test-Path -LiteralPath $manifest)) {
            throw 'Closing the last profile window did not release the process and its control pipe'
        }
        return
    }
    if ($FirstClose -eq 'DuplicateWindow') {
        Test-DuplicateInNewWindow
        Close-FixtureWindow $origin
        if (!$app.WaitForExit(10000) -or (Test-Path -LiteralPath $manifest)) {
            throw 'Closing the source did not release the process and its control pipe'
        }
        return
    }
    if ($FirstClose -eq 'WorkspaceWindow') {
        Test-WorkspaceInNewWindow
        Close-FixtureWindow $origin
        if (!$app.WaitForExit(10000) -or (Test-Path -LiteralPath $manifest)) {
            throw 'Closing the last workspace window did not release the process and control pipe'
        }
        return
    }
    if ($FirstClose -eq 'ManageWorkspace') {
        Test-ManageWorkspace
        if (!$app.WaitForExit(10000) -or (Test-Path -LiteralPath $manifest)) {
            throw 'Workspace management did not leave the windows able to close normally'
        }
        return
    }
    if ($FirstClose -eq 'ReplaceWorkspace') {
        Test-WorkspaceReplacement
        if (!$app.WaitForExit(10000) -or (Test-Path -LiteralPath $manifest)) {
            throw 'The workspace review windows did not close normally'
        }
        return
    }
    if ($FirstClose -eq 'CopyWorkspace') {
        Test-CopySavedWorkspace
        if (!$app.WaitForExit(10000) -or (Test-Path -LiteralPath $manifest)) {
            throw 'The original and duplicated workspace windows did not close normally'
        }
        return
    }
    if ($FirstClose -eq 'RepairWorkspace') {
        Test-RepairWorkspace
        if (!$app.WaitForExit(10000) -or (Test-Path -LiteralPath $manifest)) {
            throw 'The workspace repair windows did not close normally'
        }
        return
    }
    if ($FirstClose -in @('CliWindows','CliWorkspace','CliWorkspaceWindow','CliProfiles')) {
        switch ($FirstClose) {
            'CliWindows' { Test-ExplicitControlWindow }
            'CliWorkspace' { Test-ControlWorkspace }
            'CliWorkspaceWindow' { Test-ControlWorkspaceWindow }
            'CliProfiles' { Test-ControlProfileWindows }
        }
        if (!$app.WaitForExit(10000)) {
            throw "The CLI window fixture process did not exit. Remaining HWNDs: $([TabFixtureWindows]::OwnedBy($app.Id) -join ', ')"
        }
        [uint32]$code = 0
        if (![TabFixtureWindows]::GetExitCodeProcess($processHandle, [ref]$code) -or $code -ne 0) {
            throw "The CLI window fixture exited with code $code"
        }
        if (Test-Path -LiteralPath $manifest) {
            throw 'The CLI window fixture left a stale control manifest'
        }
        return
    }
    if ($FirstClose -eq 'AllTabs') {
        Test-MoveAllTerminalTabs
        Close-FixtureWindow $origin
        if (!$app.WaitForExit(10000) -or (Test-Path -LiteralPath $manifest)) {
            throw 'The last window did not release the process and control pipe'
        }
        return
    }
    Write-Output "Moving split from source $origin"
    if ($FirstClose -eq 'Context') {
        Use-Window $origin
        $window = $origin
        Expect-Focus $origin $anchor.id 'The source shell is active before opening the background tab menu'
        Press-Key 0x79 # F10 enters copy mode on the source shell.
        $null = Wait-Until 'copy mode in the source shell before moving the background tab' { Copy-ModeOpen }
        Invoke-Button (Open-Context 'Cockpit tools' 'Move tab to new window')
    } else {
        Run-WindowCommand $origin 'Move tab to new window'
    }
    $destination = Wait-DetachedWindow $origin
    Write-Output "Source $origin and destination $destination are both live"
    $newOwner = Expect-Group 3 $ssh.window_id
    $sourceAfter = (Control @('list')).sessions | Where-Object id -eq $anchor.id
    if ($sourceAfter.window_id -ne $anchor.window_id -or $sourceAfter.tab_id -ne $anchor.tab_id) {
        throw 'The source tab identity changed'
    }
    Use-Window $destination
    Expect-Focus $destination $ssh.id 'Destination initially keeps the SSH pane focused'
    Type-In $destination 'ssh-probe/'
    Expect-Text $ssh.id 'ssh-probe/'
    $window = $destination
    Press-Key 0x77 # F8 focuses the right PowerShell pane
    Expect-Focus $destination $moved.id 'F8 selects the destination PowerShell pane'
    if ((Shell-State $moved.id 'movedAfter' $destination) -ne $localIdentity) { throw 'Moved PowerShell restarted' }
    Use-Window $origin
    Expect-Focus $origin $anchor.id 'Returning to the source selects its PowerShell pane'
    if ($FirstClose -eq 'Context') {
        $window = $origin
        $null = Wait-Until 'the source shell is still in copy mode' { Copy-ModeOpen }
        Press-Key 27
        $null = Wait-Until 'copy mode closes on Escape' { !(Copy-ModeOpen) }
    }
    if ((Shell-State $anchor.id 'sourceAfter' $origin) -ne $anchorIdentity) { throw 'Source PowerShell restarted' }
    Use-Window $destination
    Expect-Focus $destination $moved.id 'Returning to the destination restores its PowerShell focus'
    $window = $destination
    Press-Key 0x78 # F9 focuses the left SSH pane
    Expect-Focus $destination $ssh.id 'F9 selects the destination SSH pane'

    if ($FirstClose -in 'Search','PaneSearch','TextSearch') {
        Run-WindowCommand $destination ('New tab with Destination PowerShell ' + [char]0x00b7 + ' Local')
        $helper = Wait-Until 'the detached split to become a background tab' {
            (Control @('list')).sessions | Where-Object {
                $_.label -eq 'Destination PowerShell' -and $_.window_id -eq $newOwner -and $_.active
            }
        }
        Expect-Text $helper.id 'PS '
        $null = [TabFixtureWindows]::ShowWindowAsync($destination, 7) # SW_SHOWMINNOACTIVE
        $null = Wait-Until 'the detached window to minimize' { [TabFixtureWindows]::IsIconic($destination) }
    }
    if ($FirstClose -eq 'PaneMove') {
        Run-WindowCommand $origin ('New tab with Destination PowerShell ' + [char]0x00b7 + ' Local')
        $temporary = Wait-Until 'the temporary destination pane' {
            (Control @('list')).sessions | Where-Object { $_.label -eq 'Destination PowerShell' -and $_.window_id -eq $anchor.window_id }
        }
        Use-Window $destination
        $window = $destination
        Press-Key 0x77 # F8 focuses PowerShell in the split.
        Expect-Focus $destination $moved.id 'PowerShell is the source of the transfer'
        Choose-WindowPanePlacement $destination $anchor.window_id 'Destination PowerShell'
        $null = Control @('close-pane','--pane',"$($temporary.id)")
        Press-Key 39
        $null = Wait-Until 'the stale destination is rejected' {
            Find-Here $destination "The pane could not move. It is still available in this window.  $([char]0x00b7)  Dismiss"
        }
        Expect-Focus $destination $moved.id 'Rejecting the move keeps the source pane'
        $stillSplit = @((Control @('list')).sessions | Where-Object { $_.id -in $ssh.id,$moved.id })
        if ($stillSplit.Count -ne 2 -or @($stillSplit | Where-Object { $_.window_id -eq $newOwner -and $_.tab_id -eq $ssh.tab_id }).Count -ne 2) {
            throw 'Rejecting the destination changed the original split'
        }

        Choose-WindowPanePlacement $destination $anchor.window_id 'Source PowerShell'
        Press-Key 39 # Right of the original window's anchor pane.
        Expect-Focus $origin $moved.id 'PowerShell appears beside the original shell and receives focus'
        $null = Wait-Until 'the moved pane joins the target tab while SSH remains in the other window' {
            $all = @((Control @('list')).sessions)
            $newLocal = $all | Where-Object id -eq $moved.id
            $remote = $all | Where-Object id -eq $ssh.id
            $newLocal.window_id -eq $anchor.window_id -and $newLocal.tab_id -eq $anchor.tab_id -and
                $remote.window_id -eq $newOwner -and $remote.tab_id -eq $ssh.tab_id -and
                $remote.connection_generation -eq $ssh.connection_generation
        }
        if ((Shell-State $moved.id 'MV' $origin) -ne $localIdentity -or
            (Shell-State $anchor.id 'AN') -ne $anchorIdentity) { throw 'The pane move restarted a local shell' }
        Type-In $destination 'split-stays/'
        Expect-Text $ssh.id 'split-stays/'

        Choose-WindowPanePlacement $destination $anchor.window_id 'Moved PowerShell'
        Press-Key 38 # Above the previously moved PowerShell pane.
        $null = Wait-Until 'the empty source HWND closes after the last pane is claimed' {
            $handles = [TabFixtureWindows]::OwnedBy($app.Id)
            $handles.Count -eq 1 -and $handles[0] -eq $origin
        }
        Expect-Focus $origin $ssh.id 'The original SSH channel has focus in the destination tab'
        $all = @((Control @('list')).sessions)
        if ($all.Count -ne 3 -or @($all | Where-Object { $_.window_id -eq $anchor.window_id -and $_.tab_id -eq $anchor.tab_id }).Count -ne 3) {
            throw 'The last pane did not join the selected tab'
        }
        Type-In $origin 'only-pane-moved/'
        Expect-Text $ssh.id 'only-pane-moved/'
        $last = $origin
    } elseif ($FirstClose -eq 'TextSearch') {
        $lineBreak = [string][char]13 + [char]10
        $lines = @('', 'old fleet needle') + @(1..70 | ForEach-Object { "scrollback row $_" }) + @('ssh latest', '')
        $wire = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($lines -join $lineBreak))
        $null = Control @('send-text','--pane',"$($ssh.id)",'--no-paste','--base64',$wire)
        Expect-Text $ssh.id 'ssh latest'
        Send-Line $anchor.id 'Write-Output ("local fleet" + " needle")'
        Expect-Text $anchor.id 'local fleet needle'
        $before = (Control @('get-text','--pane',"$($ssh.id)")).text
        if ($before.Contains('old fleet needle')) { throw 'SSH match must start outside the viewport' }

        Open-WindowTextSearch $origin $anchor.id $ssh.id $newOwner
        Expect-Focus $origin $anchor.id 'Previewing a remote match leaves the source pane focused'
        $window = $origin
        Press-Key 27
        $null = Wait-Until 'Escape closes the cross-window text search' { !(Find-Here $origin 'Find in all panes') }
        Expect-Focus $origin $anchor.id 'Escape keeps the original pane'
        if ((Control @('get-text','--pane',"$($ssh.id)")).text -cne $before -or
            ![TabFixtureWindows]::IsIconic($destination)) { throw 'Preview changed the sibling view' }

        Open-WindowTextSearch $origin $anchor.id $ssh.id $newOwner
        Press-Key 40
        Press-Key 13
        Expect-Focus $destination $ssh.id 'Reveal restores the other window and selects the SSH pane'
        $window = $destination
        $find = Wait-PickerEditor 'Find in scrollback' 'revealing the remote historical match'
        if ($find.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -cne 'fleet needle') {
            throw 'The destination Find bar lost the query'
        }
        $status = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'find-status')
        $null = Wait-Until 'the destination Find bar shows the selected occurrence' {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($destination)
            $node = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $status)
            $node -and $node.Current.Name -eq '1 / 1'
        }
        Press-Key 27
        $null = Wait-Until 'the destination reveals the historical line in the viewport' {
            $document = Find-Here $destination ("Terminal $([char]0x00b7) window-ssh")
            if ($document) {
                $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).
                    DocumentRange.GetText(-1).Contains('old fleet needle')
            }
        }

        Open-WindowTextSearch $origin $anchor.id $ssh.id $newOwner
        Close-FixtureWindow $destination
        $null = Wait-Until 'the matched sibling window closes' { [TabFixtureWindows]::OwnedBy($app.Id) -notcontains $destination }
        $window = $origin
        Press-Key 40
        Press-Key 13
        $null = Wait-Until 'the stale match reports that it is unavailable' {
            Find-Here $origin 'The match changed or the pane is unavailable. Refresh to search again.'
        }
        Expect-Focus $origin $anchor.id 'A stale sibling match keeps the source focus'
        Press-Key 27
        $last = $origin
    } elseif ($FirstClose -eq 'PaneSearch') {
        $null = Control @('send-text','--pane',"$($ssh.id)",'--no-paste','preview-before')
        Expect-Text $ssh.id 'preview-before'
        $grid = Control @('get-text','--pane',"$($ssh.id)")
        Open-CrossWindowPaneSearch $origin "Window $newOwner test@127.0.0.1" "Window $newOwner"
        Wait-RemotePreview $origin 'preview-before' $newOwner
        Expect-Focus $origin $anchor.id 'Preview leaves focus on the original PowerShell pane'
        $null = Control @('send-text','--pane',"$($ssh.id)",'--no-paste','-live/')
        Wait-RemotePreview $origin 'preview-before-live/' $newOwner
        $current = Control @('get-text','--pane',"$($ssh.id)")
        if ($current.cols -ne $grid.cols -or $current.rows -ne $grid.rows -or
            ![TabFixtureWindows]::IsIconic($destination)) { throw 'Preview changed the hidden pane or its window' }
        $window = $origin
        Press-Key 27
        $null = Wait-Until 'Escape closes Find pane' { !(Find-Here $origin 'Find pane') }
        Expect-Focus $origin $anchor.id 'Escape keeps the original focus'
        $newLine = [Convert]::ToBase64String([byte[]]@(13,10))
        $null = Control @('send-text','--pane',"$($ssh.id)",'--no-paste','--base64',$newLine)

        Open-CrossWindowPaneSearch $origin "Window $newOwner Moved PowerShell" 'Tab 1 / pane 2'
        Press-Key 13
        Expect-Focus $destination $moved.id 'Enter restores the other window and selects the exact split pane'
        if ((Shell-State $moved.id 'afterPaneSearch' $destination) -ne $localIdentity) {
            throw 'The pane search did not reach the original PowerShell process'
        }
        Open-CrossWindowPaneSearch $destination 'window-ssh' 'Tab 1 / pane 1'
        $window = $destination
        Press-Key 13
        Expect-Focus $destination $ssh.id 'Searching locally still selects the original SSH pane'
        $null = Wait-Until 'the local picker is dismissed before typing' { !(Find-Here $destination 'Find pane') }
        Type-In $destination 'focused-remote/'
        Expect-Text $ssh.id 'focused-remote/'

        Open-CrossWindowPaneSearch $origin "Window $newOwner Destination PowerShell" 'Tab 2 / pane 1'
        $null = Control @('close-pane','--pane',"$($helper.id)")
        $null = Wait-Until 'the closed sibling pane is removed with the destination window still open' {
            (Find-Here $origin 'No matching panes') -and
                ([TabFixtureWindows]::OwnedBy($app.Id) -contains $destination)
        }
        $window = $origin
        Press-Key 27
        Expect-Focus $origin $anchor.id 'Source stays focused when the sibling result is removed'
        Close-FixtureWindow $destination
        $null = Wait-Until 'the sibling HWND closes' { [TabFixtureWindows]::OwnedBy($app.Id) -notcontains $destination }
        $last = $origin
    } elseif ($FirstClose -eq 'WindowSearch') {
        $separator = " $([char]0xb7) "
        Set-FixtureWindowName $destination 'Production Fleet'
        $null = Wait-Until 'the name dialog closes after saving' {
            [TabFixtureWindows]::Title($destination) -ceq "w11term${separator}Production Fleet${separator}Cockpit tools"
        }
        Run-WindowCommand $destination ('New tab with Destination PowerShell' + $separator + 'Local')
        $helper = Wait-Until 'the destination has a separate active PowerShell tab' {
            (Control @('list')).sessions | Where-Object {
                $_.label -eq 'Destination PowerShell' -and $_.window_id -eq $newOwner -and $_.active
            }
        }
        Expect-Text $helper.id 'PS '
        $background = (Control @('list')).sessions | Where-Object id -eq $ssh.id
        Use-Window $origin
        $null = [TabFixtureWindows]::ShowWindowAsync($destination, 7)
        $null = Wait-Until 'inactive destination window minimizes' { [TabFixtureWindows]::IsIconic($destination) }
        $result = "Window ${newOwner}${separator}Production Fleet    Destination PowerShell"
        Open-WindowSearch $origin 'fleet destination' $result
        $window = $origin
        Press-Key 27
        $null = Wait-Until 'Escape dismisses Find window' { !(Find-Here $origin 'Search windows') }
        Expect-Focus $origin $anchor.id 'Escape leaves the original window active'
        if (![TabFixtureWindows]::IsIconic($destination)) { throw 'Escape restored the destination window' }

        Open-WindowSearch $origin 'Production Fleet' $result
        Press-Key 13
        Expect-Focus $destination $helper.id 'Find window restores the last active PowerShell tab'
        if ([TabFixtureWindows]::IsIconic($destination)) { throw 'The destination did not restore' }
        $hidden = (Control @('list')).sessions | Where-Object id -eq $ssh.id
        if ($hidden.visible -or $hidden.tab_id -ne $background.tab_id -or
            $hidden.connection_generation -ne $background.connection_generation) {
            throw 'Window activation switched to or changed the background SSH tab'
        }

        Open-WindowSearch $origin "Window $newOwner" $result
        $null = Control @('close-pane','--pane',"$($helper.id)")
        $null = Wait-Until 'the open window picker shows the new active tab' {
            Find-Here $origin "Window ${newOwner}${separator}Production Fleet    Cockpit tools"
        }
        Close-FixtureWindow $destination
        $null = Wait-Until 'closed windows disappear from the existing picker' {
            Find-Here $origin 'No matching windows'
        }
        $window = $origin
        Press-Key 27
        Expect-Focus $origin $anchor.id 'Closing a listed window leaves source focus intact'
        $last = $origin
    } elseif ($FirstClose -eq 'WindowName') {
        $separator = " $([char]0xb7) "
        $name = 'Production Fleet'
        $rename = 'Rename window' + [char]0x2026
        $defaultTitle = "w11term${separator}Cockpit tools"
        $valuePattern = [System.Windows.Automation.ValuePattern]::Pattern
        Set-FixtureWindowName $destination $name
        $null = Wait-Until 'native window title includes name and active tab' {
            [TabFixtureWindows]::Title($destination) -ceq "w11term${separator}${name}${separator}Cockpit tools"
        }
        if ([TabFixtureWindows]::Title($origin) -cne "w11term${separator}Source PowerShell") {
            throw 'Renaming the detached window changed the source window title'
        }
        $tabResult = "Cockpit tools    Window ${newOwner}${separator}${name}${separator}Tab 1"
        Open-TabSearch $origin 'fleet cockpit' $tabResult
        $window = $origin
        Press-Key 27
        $null = Wait-Until 'tab search closes before finding a pane by window name' {
            !(Find-Here $origin 'Search tabs')
        }
        Press-Key 0x75 # F6, fixture Find pane shortcut
        $search = Wait-PickerEditor 'Find pane' 'search by window name'
        $search.GetCurrentPattern($valuePattern).SetValue('production fleet')
        $null = Wait-Until 'both live panes match the window name' {
            $rows = @(Get-PickerRows)
            $matching = @($rows | Where-Object { $_.Contains("Window ${newOwner}${separator}${name}") })
            $rows.Count -eq 2 -and $matching.Count -eq 2
        }
        Press-Key 13
        Expect-Focus $destination $ssh.id 'A named pane result focuses the original live SSH channel'

        Use-Window $origin
        $window = $origin
        Press-Key 0x76 # F7, fixture command palette
        $search = Wait-PickerEditor 'Search commands' 'the named move destination'
        $search.GetCurrentPattern($valuePattern).SetValue($name)
        $null = Wait-Until 'the move command shows the named destination and active tab' {
            $entry = Find-Here $origin "Move tab to existing window ${newOwner}${separator}${name}${separator}Cockpit tools"
            $entry -and $entry.Current.IsEnabled
        }
        Press-Key 27
        $null = Wait-Until 'the move destination preview closes' { !(Find-Here $origin 'Search commands') }

        Run-WindowCommand $destination $rename
        $field = Wait-Until 'existing window name loads in the editor' {
            $entry = Find-Here $destination 'Window name'
            if ($entry -and $entry.GetCurrentPattern($valuePattern).Current.Value -ceq $name) { $entry }
        }
        Invoke-Button (Wait-Until 'Use default window name' { Find-Here $destination 'Use default' })
        $null = Wait-Until 'native title returns to the active tab title' {
            [TabFixtureWindows]::Title($destination) -ceq $defaultTitle
        }
        Open-TabSearch $origin "Window $newOwner cockpit" "Cockpit tools    Window ${newOwner}${separator}Tab 1"
        $window = $origin
        Press-Key 13
        Expect-Focus $destination $ssh.id 'Clearing the name preserves numeric window routing'
        Close-FixtureWindow $destination
        $last = $origin
    } elseif ($FirstClose -eq 'Search') {
        $join = [char]0x00b7
        $splitQuery = "Window $newOwner cockpit"
        $splitResult = "Cockpit tools    Window $newOwner $join Tab 1"
        Open-TabSearch $origin $splitQuery $splitResult
        $window = $origin
        Press-Key 27
        $null = Wait-Until 'Escape closes Find tab' { !(Find-Here $origin 'Search tabs') }
        Expect-Focus $origin $anchor.id 'Escape leaves the source tab and window focused'
        if (![TabFixtureWindows]::IsIconic($destination)) { throw 'Escape restored the destination' }

        Open-TabSearch $origin $splitQuery $splitResult
        Press-Key 13
        Expect-Focus $destination $ssh.id 'Enter restores the other HWND and selects its background split'
        if ([TabFixtureWindows]::IsIconic($destination)) { throw 'Destination still minimized after search' }
        Type-In $destination 'searched-window/'
        Expect-Text $ssh.id 'searched-window/'

        $sourceNumber = $anchor.window_id
        Open-TabSearch $destination "Window $sourceNumber source" "Source PowerShell    Window $sourceNumber $join Tab 1"
        $window = $destination
        Press-Key 13
        Expect-Focus $origin $anchor.id 'Find tab also brings the original HWND forward'

        $helperResult = "Destination PowerShell    Window $newOwner $join Tab 2"
        Open-TabSearch $origin "Window $newOwner destination" $helperResult
        $null = Control @('close-pane','--pane',"$($helper.id)")
        $null = Wait-Until 'a closed tab disappears while its window stays open' {
            (Find-Here $origin 'No matching tabs') -and
                ([TabFixtureWindows]::OwnedBy($app.Id) -contains $destination)
        }
        $window = $origin
        Press-Key 27
        $null = Wait-Until 'the picker closes after the result disappears' { !(Find-Here $origin 'Search tabs') }

        Open-TabSearch $origin $splitQuery $splitResult
        Close-FixtureWindow $destination
        $null = Wait-Until 'closed sibling tabs disappear from the open search' {
            Find-Here $origin 'No matching tabs'
        }
        if (Find-Here $origin $splitResult) { throw 'The closed tab remained actionable in Find tab' }
        $window = $origin
        Press-Key 27
        Expect-Focus $origin $anchor.id 'Source remains focused after the sibling closes'
        if ((Shell-State $anchor.id 'afterSearch') -ne $anchorIdentity) { throw 'Search restarted the source shell' }
        $last = $origin
    } elseif ($FirstClose -in 'Existing','Context') {
        $baselineHeight = Wait-Until 'SSH cells before pane zoom' { Get-PaneCellHeight $ssh.id }
        Run-WindowCommand $destination 'Increase pane text size'
        $zoomedHeight = Wait-Until 'SSH cells enlarged before moving to the existing window' {
            $height = Get-PaneCellHeight $ssh.id
            if ($height -gt $baselineHeight) { $height }
        }
        Write-Output "Moving the live split back into existing window $origin"
        $target = "$($anchor.window_id) $([char]0x00b7) Source PowerShell"
        if ($FirstClose -eq 'Context') {
            Run-WindowCommand $destination ('New tab with Destination PowerShell ' + [char]0x00b7 + ' Local')
            $helper = Wait-Until 'a separate selected shell in the detached window' {
                (Control @('list')).sessions | Where-Object {
                    $_.label -eq 'Destination PowerShell' -and $_.window_id -eq $newOwner -and $_.active
                }
            }
            Expect-Text $helper.id 'PS '
            $helperIdentity = Shell-State $helper.id 'helperBefore' -Init
            $window = $destination
            $submenu = Open-Context 'Cockpit tools' 'Move tab to existing window'
            $submenu.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
            Invoke-Button (Wait-Until 'original window in the tab context menu' {
                Find-Element "Window $target" ([System.Windows.Automation.ControlType]::MenuItem) -Visible
            })
            $returned = Expect-Group 4 $newOwner
            Use-Window $destination
            Expect-Focus $destination $helper.id 'The detached window keeps its selected shell'
            if ((Shell-State $helper.id 'helperAfter' $destination) -ne $helperIdentity) {
                throw 'Moving the background split restarted the selected shell'
            }
            Close-FixtureWindow $destination
        } else {
            Run-WindowCommand $destination "Move tab to existing window $target"
            $returned = Expect-Group 3 $newOwner
        }
        if ($returned -ne $anchor.window_id) { throw 'Split moved to a different window ID' }
        $last = $origin
        $null = Wait-Until 'the original HWND to remain after the empty source closes' {
            $handles = [TabFixtureWindows]::OwnedBy($app.Id)
            $handles.Count -eq 1 -and $handles[0] -eq $origin
        }
        if ($FirstClose -eq 'Context') { Use-Window $origin }
        Expect-Focus $origin $ssh.id 'Existing destination receives the moved tab and keyboard focus'
        $null = Wait-Until 'the moved pane still renders at its enlarged text size' {
            $grid = Control @('inspect','--pane',"$($ssh.id)")
            if ($grid.rows -gt 0 -and $grid.pixel_height / $grid.rows -eq $zoomedHeight) {
                $png = Control @('screenshot','--pane',"$($ssh.id)",(Join-Path $State 'moved-zoom.png'))
                $png.height -eq $grid.pixel_height
            }
        }
        Expect-Text $ssh.id 'ssh-probe/'
        Type-In $origin 'existing-window/'
        Expect-Text $ssh.id 'existing-window/'
        $kept = (Control @('list')).sessions | Where-Object id -eq $anchor.id
        if ($kept.tab_id -ne $anchor.tab_id -or $kept.window_id -ne $anchor.window_id -or $kept.visible) {
            throw 'The destination changed or lost its former tab'
        }
        if ((Shell-State $anchor.id 'existingAnchor') -ne $anchorIdentity) { throw 'Destination PowerShell restarted' }
        $window = $origin
        Press-Key 0x77 # F8 follows the moved split to its new window
        Expect-Focus $origin $moved.id 'Existing destination can focus the moved PowerShell'
        if ((Shell-State $moved.id 'existingMoved' $origin) -ne $localIdentity) { throw 'Moved PowerShell restarted' }
    } elseif ($FirstClose -eq 'Origin') {
        Write-Output "Closing original window $origin first"
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
        Run-WindowCommand $destination 'Move tab to new window'
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
