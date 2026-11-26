[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class TitlePixels {
    [StructLayout(LayoutKind.Sequential)] public struct Rect {
        public int Left, Top, Right, Bottom;
    }
    [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr window, IntPtr target, uint flags);
}
'@
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit
$menu = [System.Windows.Automation.ControlType]::MenuItem
$tab = [System.Windows.Automation.ControlType]::TabItem
$toggle = [System.Windows.Automation.TogglePattern]::Pattern

function Click([string]$Name, [System.Windows.Automation.ControlType]$Type = $button) {
    try {
        Invoke-Button (Wait-Until "enabled '$Name'" {
            $element = Find-Element $Name $Type -Visible
            if ($element -and $element.Current.IsEnabled) { $element }
        })
    } catch {
        $element = Find-Element $Name $Type
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        throw "$_ Element=$($element.Current.BoundingRectangle) Offscreen=$($element.Current.IsOffscreen) Enabled=$($element.Current.IsEnabled) Window=$($root.Current.BoundingRectangle)"
    }
}

function Get-TitleInk($Element) {
    $bounds = $Element.Current.BoundingRectangle
    $origin = New-Object TitlePixels+Rect
    $previous = [TitlePixels]::SetThreadDpiAwarenessContext([IntPtr](-4))
    try {
        if (![TitlePixels]::GetWindowRect($window, [ref]$origin)) { throw 'Could not read physical window bounds' }
        $bitmap = [Drawing.Bitmap]::new(($origin.Right-$origin.Left), ($origin.Bottom-$origin.Top))
        try {
            $graphics = [Drawing.Graphics]::FromImage($bitmap)
            try {
                $hdc = $graphics.GetHdc()
                try {
                    if (![TitlePixels]::PrintWindow($window, $hdc, 2)) { throw 'Could not capture the title bar' }
                } finally { $graphics.ReleaseHdc($hdc) }
            } finally { $graphics.Dispose() }
            $left = [int][Math]::Floor($bounds.Left - $origin.Left + $bounds.Width * 0.12)
            $right = [int][Math]::Ceiling($bounds.Right - $origin.Left - $bounds.Width * 0.12)
            $top = [int][Math]::Floor($bounds.Top - $origin.Top + $bounds.Height * 0.12)
            $bottom = [int][Math]::Ceiling($bounds.Bottom - $origin.Top - $bounds.Height * 0.12)
            $minX = $right
            $minY = $bottom
            $maxX = $left-1
            $maxY = $top-1
            for ($x = $left; $x -lt $right; $x++) {
                for ($y = $top; $y -lt $bottom; $y++) {
                    $pixel = $bitmap.GetPixel($x, $y)
                    if ($pixel.R -lt 160 -or $pixel.G -lt 160 -or $pixel.B -lt 160) { continue }
                    $minX = [Math]::Min($minX, $x)
                    $maxX = [Math]::Max($maxX, $x)
                    $minY = [Math]::Min($minY, $y)
                    $maxY = [Math]::Max($maxY, $y)
                }
            }
            return @{ Width = $maxX-$minX+1; Height = $maxY-$minY+1; ButtonWidth = $bounds.Width }
        } finally { $bitmap.Dispose() }
    } finally { $null = [TitlePixels]::SetThreadDpiAwarenessContext($previous) }
}
function Title-Files {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $id = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'w11term-title-files')
    return $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $id)
}

function Wait-FilesToggle([string]$Expected) {
    return Wait-Until "title Files toggle in state $Expected" {
        $item = Title-Files
        if ($item -and $item.Current.IsEnabled -and !$item.Current.IsOffscreen) {
            $state = $item.GetCurrentPattern($toggle)
            if ($state.Current.ToggleState -eq $Expected) { $state }
        }
    }
}

function Wait-CompactTitle([string]$Description) {
    $null = Wait-Until $Description {
        (Find-Element 'Terminal tools' $button -Visible) -and
            (Find-Element 'Attention inbox, 0 unread' $button -Visible) -and
            !(Find-Element 'Pane layout' $button -Visible)
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh', '--cols', '110', '--rows', '28') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control manifest' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    $newTab = Wait-Until 'visible New tab toolbar button' { Find-Element 'New tab' $button -Visible }
    $ink = Get-TitleInk $newTab
    # The plus uses equally long strokes. Native button padding used to cut off
    # one arm, leaving a narrow, visibly asymmetric glyph at 150% scaling.
    if ($ink.Height -lt $ink.ButtonWidth * 0.30 -or
        $ink.Width -lt $ink.Height * 0.80 -or $ink.Width -gt $ink.Height * 1.20) {
        throw "The title-bar plus is clipped. Ink=$($ink.Width)x$($ink.Height) button=$($ink.ButtonWidth)"
    }
    (Wait-FilesToggle 'Off').Toggle()
    $null = Wait-FilesToggle 'On'
    $null = Wait-Until 'remote folder listing from title bar' { Find-Element 'title-tools.txt' }
    Invoke-Button (Get-NativeMenuAction 'Tab menu' 'SFTP files' -Submenu 'Panes')
    $null = Wait-FilesToggle 'Off'
    $null = Wait-Until 'remote file drawer closed' { !(Find-Element 'title-tools.txt') }

    Click 'Find in terminal scrollback'
    $null = Wait-Until 'terminal Find opened from title bar' { Find-Element 'Find in scrollback' $edit }
    Press-Key 0x1b
    $null = Wait-Until 'terminal Find closed' { !(Find-Element 'Find in scrollback' $edit) }
    Click 'Command palette'
    $null = Wait-Until 'command palette opened from title bar' { Find-Element 'Search commands' $edit }
    Press-Key 0x1b
    $null = Wait-Until 'command palette closed' { !(Find-Element 'Search commands' $edit) }

    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $bounds = $root.Current.BoundingRectangle
    $resize = $root.GetCurrentPattern([System.Windows.Automation.TransformPattern]::Pattern)
    $scale = [FixtureMessages]::GetDpiForWindow($window) / 96.0
    $compactWidth = [Math]::Round(680 * $scale)
    $wideSsh = Control @('inspect','--pane',"$remote")
    $resize.Resize($compactWidth, $bounds.Height)
    # Tab activation remembers the last measured SSH grid. Set it while SSH is
    # still active so its later restoration is itself the transition to compact.
    $compactSsh = Wait-Until 'the compact SSH grid has been measured' {
        $measured = Control @('inspect','--pane',"$remote")
        if ($measured.pixel_width -gt 0 -and $measured.cols -lt $wideSsh.cols) { $measured }
    }

    Click 'New tab'
    Click ('Palette PowerShell ' + [char]0xb7 + ' Local') $menu
    $local = Wait-Until 'PowerShell tab' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $local
    $resize.Resize($bounds.Width, $bounds.Height)
    $null = Wait-Until 'SFTP toggle disabled for PowerShell' {
        $item = Title-Files
        $item -and !$item.Current.IsEnabled -and $item.GetCurrentPattern($toggle).Current.ToggleState -eq 'Off'
    }
    Click 'Pane layout'
    Click 'Split right    Ctrl+Alt+Shift+Right' $menu
    $split = Wait-Until 'second PowerShell pane from the layout menu' {
        ((Control @('list')).sessions | Where-Object { $_.label -eq 'Palette PowerShell' -and $_.id -ne $local }).id
    }
    Wait-Focus $split
    $null = Control @('wait-for-text','--pane',"$split",'--timeout','15','PS ')

    $zoom = 'Zoom pane    Ctrl+Alt+Shift+Z'
    Invoke-NativeMenuAction 'Pane layout' $zoom
    $null = Wait-Until 'the toolbar zooms the focused local pane' {
        $visible = @((Control @('list')).sessions | Where-Object visible)
        $visible.Count -eq 1 -and $visible[0].id -eq $split -and $visible[0].active
    }
    Invoke-NativeMenuAction 'Pane layout' 'Restore panes    Ctrl+Alt+Shift+Z'
    $null = Wait-Until 'the toolbar restores both original local panes' {
        $visible = @((Control @('list')).sessions | Where-Object visible)
        $visible.Count -eq 2 -and $local -in $visible.id -and $split -in $visible.id
    }

    $rename = 'Rename focused pane' + [char]0x2026
    $value = [System.Windows.Automation.ValuePattern]::Pattern
    Invoke-NativeMenuAction 'Pane layout' $rename
    $name = Wait-Until 'pane name editor from the toolbar' { Find-Element 'Pane name' $edit -Visible }
    $name.GetCurrentPattern($value).SetValue('Discard this name')
    Click 'Cancel'
    $null = Wait-Until 'canceled pane name editor closes' { !(Find-Element 'Pane name' $edit -Visible) }

    Invoke-NativeMenuAction 'Pane layout' $rename
    $name = Wait-Until 'pane name editor reopens after cancel' { Find-Element 'Pane name' $edit -Visible }
    $field = $name.GetCurrentPattern($value)
    if ($field.Current.Value) { throw 'The discarded pane name appeared in the reopened editor' }
    $field.SetValue('Build output')
    Click 'Save'
    $null = Wait-Until 'only the focused PowerShell pane is named by the toolbar' {
        $sessions = (Control @('list')).sessions
        $named = @($sessions | Where-Object pane_name -ceq 'Build output')
        $named.Count -eq 1 -and $named[0].id -eq $split -and $named[0].active -and
            @($sessions).Count -eq 3
    }

    $resize.Resize($compactWidth, $bounds.Height)
    Wait-CompactTitle 'Terminal tools and Attention remain visible in the compact title bar'
    $localFiles = Get-NativeMenuAction 'Terminal tools' 'Show SFTP files' -AllowDisabled
    if ($localFiles.Current.IsEnabled) { throw 'The compact menu offered SFTP for PowerShell' }
    Click 'Find in scrollback    Ctrl+Shift+F' $menu
    $null = Wait-PickerEditor 'Find in scrollback' 'Find from Terminal tools'
    Press-Key 0x1b
    $null = Wait-Until 'compact Find closes' { !(Find-Element 'Find in scrollback' $edit) }
    Invoke-Button (Get-NativeMenuAction 'Terminal tools' $zoom -Submenu 'Pane layout')
    $null = Wait-Until 'zoom from the compact Pane layout submenu' {
        $visible = @((Control @('list')).sessions | Where-Object visible)
        $visible.Count -eq 1 -and $visible[0].id -eq $split
    }
    Invoke-Button (Get-NativeMenuAction 'Terminal tools' 'Restore panes    Ctrl+Alt+Shift+Z' -Submenu 'Pane layout')
    $null = Wait-Until 'both local panes restored from compact tools' {
        @((Control @('list')).sessions | Where-Object visible).Count -eq 2
    }
    Invoke-NativeMenuAction 'Terminal tools' ('Command palette' + [char]0x2026 + '    F7')
    $null = Wait-PickerEditor 'Search commands' 'Command palette from Terminal tools'
    Press-Key 0x1b
    $null = Wait-Until 'compact command palette closes' { !(Find-Element 'Search commands' $edit) }

    $resize.Resize($bounds.Width, $bounds.Height)
    $null = Wait-Until 'PowerShell shows the full toolbar before switching to SSH' {
        (Find-Element 'Pane layout' $button -Visible) -and !(Find-Element 'Terminal tools' $button -Visible)
    }
    $remoteTab = Wait-Until 'the original SSH tab remains visible' { Find-Element 'palette-ssh' $tab -Visible }
    $remoteTab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Wait-Focus $remote
    Wait-CompactTitle 'selecting the SSH profile restores its compact toolbar'
    $null = Wait-Until 'the SSH profile restores the compact cell grid' {
        $restored = Control @('inspect','--pane',"$remote")
        $restored.cols -eq $compactSsh.cols -and $restored.rows -eq $compactSsh.rows
    }
    Invoke-NativeMenuAction 'Terminal tools' 'Show SFTP files'
    $null = Wait-Until 'compact SSH files open the original drawer' { Find-Element 'title-tools.txt' }
    Invoke-NativeMenuAction 'Terminal tools' 'Hide SFTP files'
    $null = Wait-Until 'compact SSH files close the same drawer' { !(Find-Element 'title-tools.txt') }
    Invoke-NativeMenuAction 'Tab menu' 'Find in scrollback    Ctrl+Shift+F'
    $null = Wait-PickerEditor 'Find in scrollback' 'Find from More'
    Press-Key 0x1b
    $null = Wait-Until 'Find from More closes' { !(Find-Element 'Find in scrollback' $edit) }
    $resize.Resize($bounds.Width, $bounds.Height)
    $null = Wait-FilesToggle 'Off'
    $null = Wait-Until 'a wider title bar restores the individual tools' {
        (Find-Element 'Pane layout' $button -Visible) -and !(Find-Element 'Terminal tools' $button -Visible)
    }
    Click 'Tab menu'
    Click 'Settings' $menu
    $null = Wait-Until 'Settings opened from More' {
        (Find-Element 'Settings' $tab) -and (Find-Element 'Profiles')
    }
} catch {
    $observed = if ($window) { [System.Windows.Automation.AutomationElement]::FromHandle($window) }
    $geometry = if ($observed) { $observed.Current.BoundingRectangle }
    throw "$_ at $($_.ScriptStackTrace). Window bounds=$geometry"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
