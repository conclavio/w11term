param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

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
function Wait-Launch([int]$Pane, [string]$Name) {
    Wait-Text $Pane "wsl-fixture ready $Name"
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
using System.Text;
public static class WslFixture {
    public static int Main(string[] args) {
        Console.OutputEncoding = new UTF8Encoding(false);
        if (args.Length != 4 || args[0] != "--distribution" || args[2] != "--cd" || args[3] != "~") {
            Console.WriteLine("wsl-fixture got unexpected arguments");
            return 2;
        }
        Console.WriteLine("wsl-fixture ready " + args[1]);
        while (Console.ReadLine() != null) { }
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
    Run-Palette "Split right with $rightLabel"
    $right = Wait-Until 'different WSL distro split in the same tab' {
        Sessions | Where-Object { $_.active -and $_.label -ceq $rightLabel -and $_.tab_id -eq $left.tab_id }
    }
    Wait-Launch $right.id $rightName

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
    Wait-Launch $restoredRight.id $rightName
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
