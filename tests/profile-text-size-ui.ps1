[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$edit = [System.Windows.Automation.ControlType]::Edit
$button = [System.Windows.Automation.ControlType]::Button
$valuePattern = [System.Windows.Automation.ValuePattern]::Pattern
$localName = 'Palette PowerShell'

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name' button" {
        $entry = Find-Element $Name $button
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
}
function Edit-TextSize([string]$Name, [string]$EditButton, [string]$SaveButton, [int]$Size) {
    Run-Palette 'Settings'
    Click $EditButton
    $field = Wait-Until "text size editor for '$Name'" {
        $profile = Find-Element 'Profile name' $edit
        if ($profile -and $profile.GetCurrentPattern($valuePattern).Current.Value -ceq $Name) {
            Find-Element 'Terminal text size' $edit
        }
    }
    if ($field.GetCurrentPattern($valuePattern).Current.Value -cne '') {
        throw "Profile '$Name' should inherit text size before the edit"
    }
    $field.SetFocus()
    $null = Wait-Until "text size focused for '$Name'" { $field.Current.HasKeyboardFocus }
    $field.GetCurrentPattern($valuePattern).SetValue([string]$Size)
    (Find-Element 'Profile name' $edit).SetFocus()
    $null = Wait-Until "text size '$Size' retained after leaving the field" {
        $entry = Find-Element 'Terminal text size' $edit
        $entry -and !$entry.Current.HasKeyboardFocus -and
            $entry.GetCurrentPattern($valuePattern).Current.Value -ceq [string]$Size
    }
    Click $SaveButton
    if ($SaveButton -eq 'Save profile changes') {
        $null = Wait-Until "SSH profile '$Name' save completed" { Find-Element 'Changes saved.' }
    } else {
        $null = Wait-Until "local profile '$Name' save returned to Settings" {
            !(Find-Element 'Terminal text size' $edit) -and (Find-Element $EditButton $button)
        }
    }
    $saved = ([IO.File]::ReadAllText($profiles) | ConvertFrom-Json).profiles.$Name
    if ($saved.font_size -cne $Size) {
        throw "Profile '$Name' saved '$($saved.font_size)' instead of $Size"
    }
}
function Measure-Grid([int]$Pane) { Control @('inspect','--pane',"$Pane") }
function CellHeight($Grid) { $Grid.pixel_height / $Grid.rows }
function Check-Png([int]$Pane, [string]$Name, $Grid) {
    $png = Control @('screenshot','--pane',"$Pane",(Join-Path $State "$Name.png"))
    if ($png.width -ne $Grid.pixel_width -or $png.height -ne $Grid.pixel_height) {
        throw "PNG '$Name' used $($png.width)x$($png.height) instead of $($Grid.pixel_width)x$($Grid.pixel_height)"
    }
}

$launch = @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh', '--cols', '90', '--rows', '24')
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control manifest' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $original = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$original",'--timeout','15','ready')
    $inherited = Wait-Until 'measured default SSH cells' {
        $grid = Measure-Grid $original
        # Startup can clamp the grid to the work area. The initial request has no DPI.
        if ($grid.dpi -gt 0 -and $grid.pixel_height -gt 0) { $grid }
    }
    Edit-TextSize 'palette-ssh' 'Edit profile palette-ssh' 'Save profile changes' 12
    Edit-TextSize $localName "Edit local profile $localName" 'Save profile' 24
    $originalAfterSave = Measure-Grid $original
    if ((CellHeight $originalAfterSave) -ne (CellHeight $inherited)) {
        throw "Saving the SSH profile changed existing cell height from $(CellHeight $inherited) to $(CellHeight $originalAfterSave)"
    }

    $remote = (Control @('open','--profile','palette-ssh','--activate')).pane
    Wait-Focus $remote
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    Run-Palette "Split right with $localName"
    $local = Wait-Until 'PowerShell split with saved text size' {
        ((Control @('list')).sessions | Where-Object label -eq $localName).id
    }
    Wait-Focus $local
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    $measured = Wait-Until 'both profile text sizes measured in the split' {
        $ssh = Measure-Grid $remote
        $powershell = Measure-Grid $local
        if ($ssh.dpi -eq $inherited.dpi -and $powershell.dpi -eq $inherited.dpi -and
            (CellHeight $ssh) -lt (CellHeight $inherited) -and
            (CellHeight $powershell) -gt (CellHeight $inherited)) {
            @{ ssh = $ssh; local = $powershell }
        }
    }
    Check-Png $remote 'profile-ssh' $measured.ssh
    Check-Png $local 'profile-local' $measured.local

    Run-Palette 'Increase pane text size'
    $null = Wait-Until 'temporary text size above the PowerShell profile size' {
        (CellHeight (Measure-Grid $local)) -gt (CellHeight $measured.local)
    }
    Run-Palette 'Reset pane text size'
    $null = Wait-Until 'reset restores the saved PowerShell profile size' {
        (CellHeight (Measure-Grid $local)) -eq (CellHeight $measured.local)
    }
    if ((CellHeight (Measure-Grid $remote)) -ne (CellHeight $measured.ssh)) {
        throw 'Changing the PowerShell text size resized the SSH cells'
    }
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    Close-FixtureApp $app
}

# Launch in a fresh process so the test covers the persisted startup size.
$app = Start-Process -FilePath $Gui -ArgumentList $launch -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'restart.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'restarted control manifest' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = (Control @('list')).sessions[0].id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
    $null = Wait-Until 'saved profile text size used by the startup pane' {
        $grid = Measure-Grid $pane
        $grid.dpi -eq $measured.ssh.dpi -and (CellHeight $grid) -eq (CellHeight $measured.ssh)
    }
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    Close-FixtureApp $app
}
