param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$launcher = Join-Path $State 'wsl.exe'
$env:W11TERM_TEST_PANE_DROP = Join-Path $State 'drop.json'
function Drop-Paths([int]$Pane, [string[]]$Paths) {
    [IO.File]::WriteAllText($env:W11TERM_TEST_PANE_DROP, (ConvertTo-Json -InputObject $Paths -Compress))
    $target = Wait-Until "file drop target for pane $Pane" { Find-Element "Drop paths on pane $Pane" $button }
    Invoke-Button $target
}
function Screen([int]$Pane) { (Control @('get-text','--pane',"$Pane")).text }

Add-Type -OutputAssembly $launcher -OutputType ConsoleApplication -TypeDefinition @'
using System;
using System.Text;
public static class WslFixture {
    public static int Main(string[] args) {
        Console.OutputEncoding = new UTF8Encoding(false);
        if (args.Length < 3 || args[0] != "--distribution" || args[1] != "DropLinux") return 2;
        if (args.Length == 6 && args[2] == "--exec" && args[3] == "/usr/bin/wslpath" &&
                args[4] == "-u" && args[5] == @"C:\a b\O'Brien") {
            Console.WriteLine("/custom/c/a b/O'Brien");
            return 0;
        }
        if (args.Length != 4 || args[2] != "--cd" || args[3] != "~") return 3;
        Console.WriteLine("wsl-drop-ready");
        string line;
        while ((line = Console.ReadLine()) != null) Console.WriteLine("wsl-drop=" + line);
        return 0;
    }
}
'@

$catalog = @{ profiles = @{
    'Drop PowerShell' = @{ backend = 'local'; executable = 'powershell.exe'; args = @('-NoProfile', '-NoLogo') }
    'Drop WSL' = @{ backend = 'local'; executable = $launcher; args = @('--distribution','DropLinux','--cd','~') }
} }
[IO.File]::WriteAllText($profiles, ($catalog | ConvertTo-Json -Depth 6))
$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--cols','140','--profile','"Drop PowerShell"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $psPane = (Sessions | Where-Object label -eq 'Drop PowerShell').id
    Wait-Text $psPane 'PS '
    Type-Line 'function ShowDrop { (''drop''+''-ps='')+([string]::Join(''|'', [string[]]$args)) }'
    Type-Line 'function prompt { ''> '' }'
    Type-Line "'ps-drop-' + 'ready'"
    Wait-Text $psPane 'ps-drop-ready'
    Type-Text 'ShowDrop '
    Wait-Text $psPane '> ShowDrop'
    Run-Palette 'Split right with Drop WSL'
    $wslPane = Wait-Until 'WSL pane opened in the same tab' {
        (Sessions | Where-Object { $_.label -eq 'Drop WSL' -and $_.active }).id
    }
    Wait-Text $wslPane 'wsl-drop-ready'

    Drop-Paths $psPane @('C:\With space', ("C:\O'Brien" + [char]0x2019 + 's'))
    Wait-Focus $psPane
    Wait-Text $psPane "ShowDrop 'C:\With space'"
    if ((Screen $wslPane) -match 'With space') { throw 'The PowerShell drop reached the WSL pane' }
    if ((Screen $psPane).Contains('drop-ps=')) { throw 'PowerShell drop submitted the input' }
    Press-Key 13
    Wait-Text $psPane ("drop-ps=C:\With space|C:\O'Brien" + [char]0x2019 + 's')

    Run-Palette 'Enable read-only pane input'
    $null = Wait-Until 'PowerShell drops disabled in read-only mode' {
        !(Find-Element "Drop paths on pane $psPane" $button)
    }

    Drop-Paths $wslPane @("C:\a b\O'Brien", '\\wsl.localhost\DropLinux\home\me')
    Wait-Focus $wslPane
    $quotedWsl = "'/custom/c/a b/O'\''Brien' '/home/me'"
    Wait-Text $wslPane $quotedWsl
    if ((Screen $wslPane).Contains('wsl-drop=')) { throw 'WSL drop submitted the input' }
    Press-Key 13
    Wait-Text $wslPane ("wsl-drop=" + $quotedWsl)
} catch {
    if ($psPane) { Write-Output ("PowerShell screen:" + [Environment]::NewLine + (Screen $psPane)) }
    if ($wslPane) { Write-Output ("WSL screen:" + [Environment]::NewLine + (Screen $wslPane)) }
    throw
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
