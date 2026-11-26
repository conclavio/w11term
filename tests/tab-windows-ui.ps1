# The Rust loopback fixture starts one SSH session, two real PowerShell children and one GUI.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State,
      [Parameter(Mandatory)][ValidateSet('Origin','Destination','Existing','AllTabs','Context','Search','PaneSearch','TextSearch','PaneMove','PaneWindow','WindowName','WindowSearch','ProfileWindow','DuplicateWindow','WorkspaceWindow')][string]$FirstClose)
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
    Press-Key 0x76 # F7, fixture command palette
    $field = Wait-Until 'focused command palette in the requested HWND' {
        $element = Find-Here $Hwnd 'Search commands'
        if ($element -and $element.Current.HasKeyboardFocus) { $element }
    }
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($Action)
    $null = Wait-Until "enabled $Action action" {
        $item = Find-Here $Hwnd $Action
        if ($item -and $item.Current.IsEnabled) { $item }
    }
    Press-Key 13
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
