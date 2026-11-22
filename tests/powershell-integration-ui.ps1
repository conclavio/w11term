[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$labels = @('Palette PowerShell')
if ($pwsh = Get-Command pwsh.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1) {
    $catalog = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
    $catalog.profiles | Add-Member NoteProperty 'Fixture PowerShell 7' @{
        backend='local'; executable=$pwsh.Source; args=@('-NoLogo','-NoProfile')
    }
    [IO.File]::WriteAllText($profiles, ($catalog | ConvertTo-Json -Depth 12))
    $labels += 'Fixture PowerShell 7'
}

function Click([string]$Name) {
    try {
        $item = Wait-Until $Name { Find-Element $Name ([System.Windows.Automation.ControlType]::Button) }
        Invoke-Button $item
    } catch {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $visible = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition) |
            ForEach-Object { "$($_.Current.LocalizedControlType)=$($_.Current.Name)" } | Select-Object -First 65
        throw "$_  Looking for '$Name'. Current controls: $($visible -join ', ')"
    }
}

function Set-Integration([string]$Label) {
    Run-Palette 'Settings'
    Click "Edit local profile $Label"
    $name = 'Enable PowerShell integration'
    $box = Wait-Until 'interactive PowerShell profile checkbox' {
        Find-Element $name ([System.Windows.Automation.ControlType]::CheckBox)
    }
    $toggle = $box.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
    if ($toggle.Current.ToggleState -ne 'Off') { throw "Expected integration to default off for $Label" }
    $toggle.Toggle()
    $null = Wait-Until 'checked integration setting' {
        (Find-Element $name).GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Current.ToggleState -eq 'On'
    }
    Click 'Save profile'
    $null = Wait-Until 'saved PowerShell integration' {
        (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles.$Label.powershell_integration -eq $true
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    foreach ($label in $labels) {
        Set-Integration $label
        $pane = (Control @('open','--profile',$label,'--activate')).pane
        Wait-Focus $pane
        $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','PS ')
        Run-Palette 'Notify when command finishes'
        $capture = Join-Path $State "shell-$pane.vt"
        $null = Control @('record','--pane',"$pane",$capture)

        # Type an ordinary line. The shell supplies every semantic marker.
        $line = "Write-Output ('shell-output-' + (40 + 2)); cmd.exe /d /c exit 17"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($line + [char]13))
        $null = Control @('send-text','--pane',"$pane",'--no-paste','--base64',$encoded)
        $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','shell-output-42')
        try {
            $null = Wait-Until "$label to report exit 17" {
                $journal = (Control @('events','--pane',"$pane")).journal
                $events = @($journal.events | Where-Object kind -ceq 'command-finished')
                if ($events.Count -gt 1) { throw "Duplicate completion in $label" }
                if ($events.Count -eq 1 -and $events[0].data.exit_code -ne 17) {
                    throw "Incorrect exit code in ${label}: $($events[0].data.exit_code)"
                }
                $events.Count -eq 1
            }
        } catch {
            $null = Control @('record','--pane',"$pane",'--stop')
            $bytes = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($capture))
            $markers = [regex]::Matches($bytes, ([char]27 + '\]133;[A-D](?:;-?\d+)?')) |
                ForEach-Object { $_.Value.Substring(2) }
            $screen = (Control @('get-text','--pane',"$pane")).text.TrimEnd()
            throw "$_  Observed OSC markers: [$($markers -join ', ')]. Pane: $screen"
        }
        $null = Control @('record','--pane',"$pane",'--stop')
        Run-Palette 'Command history'
        $selected = "Selected, $line " + [char]0xb7 + ' exit 17'
        $null = Wait-Until "$label command text and exit status in history" { Find-Element $selected }
        $null = Wait-Until "$label command output preview" {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            $text = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
            $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text) |
                Where-Object { $_.Current.Name.Trim() -ceq 'shell-output-42' } | Select-Object -First 1
        }
        Click 'Return to pane'
        Wait-Focus $pane
        Write-Output "Verified $label"
    }
    $null = Control @('send-text','--pane',"$remote",'--no-paste','neighbor-ok')
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','neighbor-ok')
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
