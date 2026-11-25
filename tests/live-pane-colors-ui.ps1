param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/palette-pixels.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, '{"shortcuts":{"command_palette":"F7","split_right":"F8","focus_left":"F6"}}')
$profiles = Join-Path $State 'profiles.json'
$catalog = [IO.File]::ReadAllText($profiles)
$peer = [IO.File]::ReadAllText((Join-Path $State 'peer-root'))
$initial = Join-Path $peer 'copy-history.vt'
$deferred = Join-Path $peer 'deferred-output.vt'
$gate = Join-Path $peer 'hold-shell-output'
$bg = '#001155'
$menu = [System.Windows.Automation.ControlType]::MenuItem
$button = [System.Windows.Automation.ControlType]::Button

# The SSH server sends this from its shell-request handler. Index 1 is set
# explicitly to its original dark value; equality alone cannot convey ownership.
[IO.File]::WriteAllText($initial, $sample + $esc + ']4;1;' + $dark[2] + $bell + $esc + ']11;' + $bg + $bell + $esc + ']2;remote-owned' + $bell)
[IO.File]::WriteAllText($deferred, $esc + ']104;1' + $bell + $esc + ']111' + $bell + $esc + ']2;remote-released' + $bell)
[IO.File]::WriteAllText($gate, '')

function Wait-Title([int]$Pane, [string]$Title) {
    Wait-Until "pane $Pane committed $Title" {
        $frame = Control @('get-text','--pane',"$Pane")
        if ($frame.title -ceq $Title) { $frame }
    }
}
function Ssh-Panes {
    $panes = @((Control @('list')).sessions | Sort-Object id)
    $panes | Select-Object id,tab_id,window_id,connection_generation,cols,rows | ConvertTo-Json -Compress
}
function Expect-UnchangedTerminals {
    Wait-Focus $source
    if ((Ssh-Panes) -cne $identities) { throw 'Switching colors changed an SSH pane, connection or grid' }
    $s = (Control @('list')).sessions | Where-Object id -eq $source
    if (!$s.read_only) { throw 'Changing colors cleared the source input lock' }
    if ((Control @('get-text','--pane',"$source")).text -cne $sourceText -or
        (Control @('get-text','--pane',"$neighbor")).text -cne $neighborText) { throw 'Changing colors altered existing cell text' }
}
function Expand-Submenu([string]$Name) {
    $item = Wait-Until "$Name submenu" { Find-Element $Name $menu }
    $item.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','100','--rows','20') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $source = ((Control @('list')).sessions | Where-Object active).id
    $null = Wait-Title $source 'remote-owned'
    # Only the original channel receives the remote palette overrides and resets.
    [IO.File]::WriteAllText($initial, $sample)
    [IO.File]::WriteAllText($deferred, '')
    Press-Key 0x77
    $neighbor = Wait-Until 'second SSH channel in the same tab' {
        $sessions = @((Control @('list')).sessions)
        $right = $sessions | Where-Object id -ne $source
        if ($sessions.Count -eq 2 -and $right.active -and $sessions[0].tab_id -eq $sessions[1].tab_id) { $right.id }
    }
    $null = Control @('wait-for-text','--pane',"$neighbor",'--timeout','15','palette-witness')
    Press-Key 0x75
    Wait-Focus $source
    Run-Palette 'Enable read-only pane input'
    $null = Wait-Until 'locked SSH source' { ((Control @('list')).sessions | Where-Object id -eq $source).read_only }
    $identities = Ssh-Panes
    $start = Control @('get-text','--pane',"$source")
    $sourceText = $start.text
    $neighborText = (Control @('get-text','--pane',"$neighbor")).text
    Check-Pixels $source 'initial-owned' @($dark[0],$bg,$dark[2])
    Check-Pixels $neighbor 'initial-sibling' $dark

    Run-Palette 'Set pane color scheme to One Half Light'
    $null = Wait-Until 'published new palette without shell output' {
        (Control @('get-text','--pane',"$source")).generation -gt $start.generation
    }
    Check-Pixels $source 'switched-owned' @($light[0],$bg,$dark[2])
    Check-Pixels $neighbor 'untouched-sibling' $dark
    Expect-UnchangedTerminals

    [IO.File]::Delete($gate)
    $reset = Wait-Title $source 'remote-released'
    Check-Pixels $source 'released-to-live-scheme' $light
    Expect-UnchangedTerminals

    Invoke-Button (Wait-Until 'native tab menu' { Find-Element 'Tab menu' $button })
    Expand-Submenu 'Panes'
    Expand-Submenu 'Color scheme'
    $null = Wait-Until 'menu shows the current pane scheme' { Find-Element 'One Half Light (current)' $menu }
    Invoke-Button (Wait-Until 'default option' { Find-Element 'w11term (default)' $menu })
    $null = Wait-Until 'menu scheme update committed' {
        (Control @('get-text','--pane',"$source")).generation -gt $reset.generation
    }
    Check-Pixels $source 'second-live-scheme' $legacy
    Expect-UnchangedTerminals
    if (@([IO.File]::ReadAllLines((Join-Path $peer 'pty-trace'))).Count -ne 2) { throw 'A color change opened another SSH shell' }
    if ([IO.File]::ReadAllText($profiles) -cne $catalog) { throw 'Temporary pane scheme modified the saved profile catalog' }

    $fresh = (Control @('open','--profile','palette-ssh','--activate')).pane
    $null = Control @('wait-for-text','--pane',"$fresh",'--timeout','15','palette-witness')
    Wait-Focus $fresh
    if ($fresh -in $source,$neighbor) { throw 'Expected an independent pane from the saved profile' }
    Check-Pixels $fresh 'fresh-profile' $dark
    Check-Pixels $source 'preserved-live-scheme' $legacy
    Check-Pixels $neighbor 'preserved-sibling' $dark
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
