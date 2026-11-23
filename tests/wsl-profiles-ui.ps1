param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')
$keymap.shortcuts.keyboard_hints = 'F12'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, ($keymap | ConvertTo-Json))

$hostKey = 'Software\w11term-wsl-fixture-' + (Split-Path -Leaf $State)
$env:W11TERM_TEST_WSL_FIXTURE = $State
$launcher = Join-Path $State 'wsl.exe'
$leftName = 'QA Linux ' + [char]0x96ea
$rightName = 'Build Linux'
$leftLabel = "WSL: $leftName"
$rightLabel = "WSL: $rightName"
$menu = [System.Windows.Automation.ControlType]::MenuItem
$edit = [System.Windows.Automation.ControlType]::Edit
$value = [System.Windows.Automation.ValuePattern]::Pattern
$children = [Collections.Generic.List[string]]::new()
$volatile = [Microsoft.Win32.RegistryOptions]::Volatile
$readWrite = [Microsoft.Win32.RegistryKeyPermissionCheck]::ReadWriteSubTree
$root = $null
$app = $null

function Register-Distro([string]$Id, [string]$Name, [bool]$Modern = $false) {
    $child = $root.CreateSubKey($Id, $readWrite, $volatile)
    $children.Add($Id)
    try {
        $child.SetValue('DistributionName', $Name, [Microsoft.Win32.RegistryValueKind]::String)
        if ($Modern) { $child.SetValue('Modern', 1, [Microsoft.Win32.RegistryValueKind]::DWord) }
    } finally {
        $child.Dispose()
    }
}
function Click([string]$Name, $Type = $button) {
    Invoke-Button (Wait-Until "enabled '$Name' control" {
        $control = Find-Element $Name $Type
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}
function Wait-Launch([int]$Pane, [string]$Name, [string]$Directory = '~') {
    Wait-Text $Pane "wsl-fixture ready $Name"
    Wait-Text $Pane "wsl-fixture directory $Directory"
}
function Publish-Report([int]$Pane, [string]$Kind) {
    $text = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("report-$Kind" + [char]13))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$text)
    Wait-Text $Pane "wsl-fixture published $Kind"
}
function Find-HintText([string]$Pattern) {
    $guiRoot = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $texts = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
    $guiRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, $texts) |
        Where-Object { $_.Current.Name -match $Pattern } | Select-Object -First 1
}
function Reveal-Hint([int]$Pane, [string]$Kind, [string]$Diagnostic, [string]$ErrorPattern) {
    Publish-Report $Pane $Kind
    Wait-Text $Pane $Diagnostic
    Press-Key 0x7b # F12 opens hints
    $null = Wait-Until 'hint labels on the WSL pane' { Find-HintText '^Type a hint label' }
    Press-Key 0x41
    $null = Wait-Until 'reveal action attached to the WSL diagnostic' { Find-HintText 'R reveal in File Explorer' }
    Press-Key 0x52
    $null = Wait-Until 'retryable result for the WSL path' { Find-HintText $ErrorPattern }
    Press-Key 27
    $null = Wait-Until 'hints dismissed' { !(Find-HintText '^(Type a hint label|Enter copy|Reveal failed|Opening File Explorer)') }
}

try {
    # The runtime uses the real registry reader against this unique volatile
    # test key. The test launcher confirms that ConPTY passed all arguments.
    $root = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($hostKey, $readWrite, $volatile)
    Register-Distro ([guid]::NewGuid().ToString('B')) $leftName $true
    Register-Distro ([guid]::NewGuid().ToString('B')) $rightName
    Register-Distro ([guid]::NewGuid().ToString('B')) 'docker-desktop'
    Register-Distro 'not-a-distro-subkey' 'Registry metadata'
    Add-Type -OutputAssembly $launcher -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
public static class WslFixture {
    [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int kind);
    [DllImport("kernel32.dll")] static extern bool GetConsoleMode(IntPtr handle, out uint mode);
    [DllImport("kernel32.dll")] static extern bool SetConsoleMode(IntPtr handle, uint mode);
    public static int Main(string[] args) {
        Console.OutputEncoding = new UTF8Encoding(false);
        if (args.Length == 9 && args[0] == "--distribution" && args[1] == "QA Linux \u96ea" &&
            args[2] == "--cd" && args[3] == "/mnt/c/work tree" && args[4] == "--exec" &&
            args[5] == "/usr/bin/wslpath" && args[6] == "-a" && args[7] == "-w" &&
            args[8] == "./../src/fixture.rs") {
            Console.WriteLine(Path.Combine(Environment.GetEnvironmentVariable("W11TERM_TEST_WSL_FIXTURE"), "selected-wsl-file.rs"));
            return 0;
        }
        if (args.Length == 7 && args[0] == "--distribution" && args[1] == "QA Linux \u96ea" &&
            args[2] == "--exec" && args[3] == "/usr/bin/wslpath" && args[4] == "-a" &&
            args[5] == "-w" && args[6] == "/home/dev/foreign.rs") {
            Console.WriteLine(@"\\wsl.localhost\Other distro\home\dev\foreign.rs");
            return 0;
        }
        if (args.Length != 4 || args[0] != "--distribution" || args[2] != "--cd") {
            Console.WriteLine("wsl-fixture got unexpected arguments");
            return 2;
        }
        IntPtr output = GetStdHandle(-11);
        uint mode;
        if (!GetConsoleMode(output, out mode) || !SetConsoleMode(output, mode | 4)) {
            Console.WriteLine("wsl-fixture could not enable VT output");
            return 3;
        }
        Console.WriteLine("wsl-fixture ready " + args[1]);
        Console.WriteLine("wsl-fixture directory " + args[3]);
        string line;
        while ((line = Console.ReadLine()) != null) {
            switch (line) {
                case "report-linux":
                    Console.Write("\u001b]7;file:///mnt/c/work%20tree\u0007");
                    break;
                case "report-foreign":
                    Console.Write("\u001b]9;9;\\\\wsl.localhost\\Other distro\\home\u0007");
                    break;
                case "report-share":
                    Console.Write("\u001b]9;9;\"\\\\wsl$\\" + args[1] + "\\home\\100% # notes\"\u001b\\");
                    break;
                case "report-hint-relative":
                    Console.Write("\u001b[2J\u001b[H");
                    Console.WriteLine("../src/fixture.rs:4:2");
                    break;
                case "report-hint-foreign":
                    Console.Write("\u001b[2J\u001b[H");
                    Console.WriteLine("/home/dev/foreign.rs:4:2");
                    break;
                default: continue;
            }
            Console.WriteLine("wsl-fixture published " + line.Substring(7));
        }
        return 0;
    }
}
'@
    $catalog = '{"profiles":{}}'
    [IO.File]::WriteAllText($profiles, $catalog)
    $quotedProfiles = '"' + $profiles + '"'
    $listing = Join-Path $State 'list.txt'
    $listed = Start-Process -FilePath $Gui -ArgumentList @('--list-profiles','--profiles',$quotedProfiles) -WindowStyle Hidden -PassThru -Wait -RedirectStandardOutput $listing
    try {
        if ($listed.ExitCode -ne 0) { throw "Profile listing failed with exit code $($listed.ExitCode)" }
    } finally { $listed.Dispose() }
    $profilesText = [IO.File]::ReadAllText($listing)
    if (!$profilesText.Contains($leftLabel) -or !$profilesText.Contains($rightLabel) -or
        $profilesText.Contains('docker-desktop') -or $profilesText.Contains('Registry metadata')) {
        throw 'WSL profile discovery did not return the registered user distributions'
    }

    $app = Start-Process -FilePath $Gui -ArgumentList @('--demo','--cols','132','--profiles',$quotedProfiles) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    Click 'New tab'
    Click ($leftLabel + ' ' + [char]0xb7 + ' Local') $menu
    $left = Wait-Until 'WSL pane opened from the new-tab menu' {
        Sessions | Where-Object { $_.active -and $_.label -ceq $leftLabel }
    }
    Wait-Launch $left.id $leftName
    Publish-Report $left.id 'linux'
    $explorerAction = 'Open current folder in File Explorer'
    $search = Open-Palette $explorerAction
    $search.GetCurrentPattern($value).SetValue($explorerAction)
    $null = Wait-Until 'Explorer opens reported folders for named WSL panes' {
        $row = Find-Element $explorerAction $button
        $row -and $row.Current.IsEnabled
    }
    Press-Key 27
    $null = Wait-Until 'command palette dismissed' { !(Find-Element 'Search commands') }
    $selectedFile = [regex]::Escape((Join-Path $State 'selected-wsl-file.rs'))
    Reveal-Hint $left.id 'hint-relative' '../src/fixture.rs:4:2' ('^Reveal failed: Cannot find ' + $selectedFile)
    Reveal-Hint $left.id 'hint-foreign' '/home/dev/foreign.rs:4:2' '^Reveal failed: wslpath must return a local drive'
    Run-Palette 'New tab in current directory'
    $newTab = Wait-Until 'WSL tab opens in the reported Linux directory' {
        Sessions | Where-Object { $_.active -and $_.label -ceq $leftLabel -and $_.tab_id -ne $left.tab_id }
    }
    Wait-Launch $newTab.id $leftName '/mnt/c/work tree'
    Press-Key 0x79 # F10 closes the extra tab.
    Wait-Focus $left.id

    Run-Palette "Split right with $rightLabel"
    $right = Wait-Until 'different WSL distro split in the same tab' {
        Sessions | Where-Object { $_.active -and $_.label -ceq $rightLabel -and $_.tab_id -eq $left.tab_id }
    }
    Wait-Launch $right.id $rightName
    Publish-Report $right.id 'foreign'
    $action = 'Split below in current directory'
    $search = Open-Palette $action
    $search.GetCurrentPattern($value).SetValue($action)
    $null = Wait-Until 'a report for another distribution leaves the action disabled' {
        $row = Find-Element $action $button
        $row -and !$row.Current.IsEnabled
    }
    Press-Key 27
    $null = Wait-Until 'command palette dismissed' { !(Find-Element 'Search commands') }
    $search = Open-Palette $explorerAction
    $search.GetCurrentPattern($value).SetValue($explorerAction)
    $null = Wait-Until 'Explorer rejects a folder reported for another WSL distribution' {
        $row = Find-Element $explorerAction $button
        $row -and !$row.Current.IsEnabled
    }
    Press-Key 27
    $null = Wait-Until 'command palette dismissed' { !(Find-Element 'Search commands') }
    Publish-Report $right.id 'share'
    Run-Palette $action
    $directory = '/home/100% # notes'
    $child = Wait-Until 'WSL split uses a report for its own distribution' {
        Sessions | Where-Object { $_.active -and $_.label -ceq $rightLabel -and $_.id -ne $right.id -and $_.tab_id -eq $right.tab_id }
    }
    Wait-Launch $child.id $rightName $directory

    Click 'Tab menu'
    $panes = Wait-Until 'Panes submenu' { Find-Element 'Panes' $menu }
    $panes.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    Click 'Close pane    Ctrl+Alt+Shift+W' $menu
    $null = Wait-Until 'the directory split closes alone' { !(Sessions | Where-Object id -eq $child.id) }
    Click 'Tab menu'
    Click 'Reopen last closed pane' $menu
    $reopened = Wait-Until 'fresh WSL pane at the same layout position' {
        Sessions | Where-Object { $_.active -and $_.id -ne $right.id -and $_.label -ceq $rightLabel -and $_.tab_id -eq $right.tab_id }
    }
    Wait-Launch $reopened.id $rightName $directory

    Run-Palette 'Duplicate tab layout'
    $duplicate = Wait-Until 'duplicated WSL layout opens a new tab' {
        Sessions | Where-Object { $_.active -and $_.label -ceq $rightLabel -and $_.tab_id -ne $right.tab_id }
    }
    Wait-Launch $duplicate.id $rightName $directory
    Press-Key 0x79
    Wait-Focus $reopened.id

    Run-Palette ('Workspaces' + [char]0x2026)
    $name = Wait-Until 'workspace name field' { Find-Element 'Workspace name' $edit }
    $name.GetCurrentPattern($value).SetValue('wsl-split')
    Click 'Save workspace'
    $null = Wait-Until 'workspace saved' { Find-Element 'Saved wsl-split' }
    Click 'Open workspace wsl-split in new tabs'
    $restoredLeft = Wait-Until 'restored first WSL pane' {
        Sessions | Where-Object { $_.label -ceq $leftLabel -and $_.tab_id -ne $left.tab_id }
    }
    $restoredRight = Wait-Until 'restored second WSL pane and focus' {
        Sessions | Where-Object { $_.active -and $_.label -ceq $rightLabel -and $_.tab_id -eq $restoredLeft.tab_id }
    }
    Wait-Launch $restoredLeft.id $leftName
    Wait-Launch $restoredRight.id $rightName $directory
    if ([IO.File]::ReadAllText($profiles) -cne $catalog) { throw 'Discovering and opening WSL changed the profile catalog' }
} finally {
    if ($app) {
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
    if ($root) {
        foreach ($child in $children) { $root.DeleteSubKey($child, $false) }
        $root.Dispose()
        [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKey($hostKey, $false)
    }
}
