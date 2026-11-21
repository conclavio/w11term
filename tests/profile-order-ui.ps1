# Runs twice against one temporary profile catalog and the fixture's loopback SSH listener.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$profiles = Join-Path $State 'profiles.json'
$sshName = 'palette-ssh'
$localName = 'Palette PowerShell'
$ssh = (Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json).profiles.$sshName
$sshLabel = "$sshName    $($ssh.user)@$($ssh.host):$($ssh.port)"
$localLabel = "$localName " + [char]0xb7 + ' Local'
$button = [System.Windows.Automation.ControlType]::Button
$menuItem = [System.Windows.Automation.ControlType]::MenuItem

function Click([string]$Name, [System.Windows.Automation.ControlType]$Type) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $Type
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}
function Settings {
    Click 'Tab menu' $button
    Click 'Settings' $menuItem
}
function Wait-SettingsOrder([string]$First, [string]$Second) {
    $null = Wait-Until "Settings to show '$First', '$Second', and disabled end controls" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $condition = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $button)
        $rows = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)
        $up = @($rows | Where-Object { $_.Current.Name -clike 'Move * up' })
        $down = @($rows | Where-Object { $_.Current.Name -clike 'Move * down' })
        # Windows can also offer discovered local shells after these saved profiles.
        if ($up.Count -lt 2 -or $up.Count -ne $down.Count -or
            $up[0].Current.Name -cne "Move $First up" -or $up[1].Current.Name -cne "Move $Second up") { return $false }
        for ($i = 0; $i -lt $up.Count; $i++) {
            if ($up[$i].Current.IsEnabled -ne ($i -gt 0) -or
                $down[$i].Current.IsEnabled -ne ($i -lt $down.Count - 1)) { return $false }
        }
        $up[0].Current.BoundingRectangle.Top -lt $up[1].Current.BoundingRectangle.Top
    }
}
function Launch-FromMenu([string]$First, [string]$Last, [string]$Choice) {
    Click 'New tab' $button
    $null = Wait-Until "the + menu to display '$First' above '$Last'" {
        $top = Find-Element $First $menuItem
        $bottom = Find-Element $Last $menuItem
        $top -and $bottom -and $top.Current.BoundingRectangle.Height -gt 0 -and
            $bottom.Current.BoundingRectangle.Height -gt 0 -and
            $top.Current.BoundingRectangle.Top -lt $bottom.Current.BoundingRectangle.Top
    }
    Click $Choice $menuItem
}
function Active-Pane([string]$Name) {
    Wait-Until "active pane for '$Name'" {
        $match = @((Control @('list')).sessions | Where-Object { $_.active -and $_.label -ceq $Name })
        if ($match.Count -eq 1) { $match[0].id }
    }
}

for ($launch = 1; $launch -le 2; $launch++) {
    # Only the saved catalog is shared between launches.
    $env:W11TERM_DATA_DIR = Join-Path $State "data-$launch"
    $env:W11TERM_SETTINGS_PATH = Join-Path $State "appearance-$launch.json"
    $app = Start-Process -FilePath $Gui -ArgumentList @('--demo','--profiles',('"' + $profiles + '"')) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
    try {
        $window = Wait-InputWindow
        $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
        if ($launch -eq 1) {
            Launch-FromMenu $sshLabel $localLabel $localLabel
            $local = Active-Pane $localName
            $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
            Settings
            Wait-SettingsOrder $sshName $localName
            Click "Move $localName up" $button
            Wait-SettingsOrder $localName $sshName

            # Keep this row's original action while another writer restores SSH first.
            $stale = Find-Element "Move $localName down" $button
            $catalog = Get-Content -Raw -LiteralPath $profiles | ConvertFrom-Json
            $catalog.profile_order = @($sshName, $localName) + @(
                $catalog.profile_order | Where-Object { $_ -cne $sshName -and $_ -cne $localName }
            )
            $external = $catalog | ConvertTo-Json -Depth 16
            [IO.File]::WriteAllText($profiles, $external)
            Invoke-Button $stale
            $errorText = "Could not move ${localName}: Profile order changed on disk; refresh before moving. Review the current order and try again."
            $null = Wait-Until 'the stale move to report the competing catalog edit' {
                Find-Element $errorText ([System.Windows.Automation.ControlType]::Text)
            }
            Wait-SettingsOrder $sshName $localName
            if ([IO.File]::ReadAllText($profiles) -cne $external) { throw 'The stale move rewrote the catalog' }
            Click "Move $sshName down" $button
        } else {
            Settings
        }
        Wait-SettingsOrder $localName $sshName
        if ($launch -eq 1) {
            Launch-FromMenu $localLabel $sshLabel $sshLabel
            $remote = Active-Pane $sshName
            $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
            foreach ($c in 'ordered-ssh'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
            $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','ordered-ssh')
        } else {
            Launch-FromMenu $localLabel $sshLabel $localLabel
            $local = Active-Pane $localName
            $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
            foreach ($c in "'ordered-local-' + (40+2)".ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
            Press-Key 13
            $null = Control @('wait-for-text','--pane',"$local",'--timeout','10','ordered-local-42')
        }
    } finally {
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
}
