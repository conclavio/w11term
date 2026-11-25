# Run by the native loopback SSH test with SSH and PowerShell in separate tabs.
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'

function Read-ShellPid([int]$Pane, [string]$Tag) {
    # Require a numeric shell result. The echoed command also contains the tag.
    $command = "'$Tag=' + " + '$PID' + [char]13
    $bytes = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($command))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$bytes)
    $pattern = "$Tag=([0-9]+)"
    $text = (Control @('wait-for-text','--pane',"$Pane",'--regex','--timeout','15',$pattern)).text
    return [int]([regex]::Match($text, $pattern).Groups[1].Value)
}

function Open-DestinationPicker {
    Press-Key 0x76 # F7, Move pane to existing tab
    return Wait-Until 'destination picker to accept keyboard input' {
        $element = Find-Element 'Find destination pane'
        if ($element -and $element.Current.HasKeyboardFocus) { return $element }
    }
}

function Choose-Destination {
    $field = Open-DestinationPicker
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue('PowerShell')
    $null = Wait-Until 'PowerShell as the destination' {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $buttons = [System.Windows.Automation.PropertyCondition]::new(
            [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
            [System.Windows.Automation.ControlType]::Button)
        $rows = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $buttons) |
            ForEach-Object { $_.Current.Name } | Where-Object { $_ -match 'Tab \d+ / pane \d+' })
        $rows.Count -eq 1 -and $rows[0] -match 'PowerShell'
    }
    Press-Key 13
    return Wait-Until 'move placement choices' { Find-Element 'Move left' }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--workspace', 'move') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-Until 'fixture window' { $app.Refresh(); if ($app.MainWindowHandle -ne [IntPtr]::Zero) { $app.MainWindowHandle } }
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $panes = (Control @('list')).sessions
    $ssh = $panes | Where-Object label -eq 'move-ssh'
    $shell = $panes | Where-Object label -eq 'Windows PowerShell'
    if ($panes.Count -ne 2 -or !$ssh -or !$shell -or $ssh.tab_id -eq $shell.tab_id) {
        throw 'Expected separate SSH and PowerShell tabs'
    }
    Wait-Focus $ssh.id
    $live = Control @('wait-for-text','--pane',"$($ssh.id)",'--timeout','15','move-live-screen')
    if ($live.text.Contains('move-retained-row')) { throw 'Fixture history marker is still on screen' }
    $generation = ((Control @('list')).sessions | Where-Object id -eq $ssh.id).connection_generation
    $null = Control @('wait-for-text','--pane',"$($shell.id)",'--regex','--timeout','15','PS [^\n]*>')
    $shellPid = Read-ShellPid $shell.id 'before-move'
    if ($shellPid -le 0 -or $generation -le 0) { throw 'Both connections must be live before moving' }

    $null = Open-DestinationPicker
    Press-Key 27
    $null = Wait-Until 'destination search cancellation' { -not (Find-Element 'Find destination pane') }
    Wait-Focus $ssh.id
    $null = Choose-Destination
    Press-Key 27
    $null = Wait-Until 'placement cancellation' { -not (Find-Element 'Move left') }
    Wait-Focus $ssh.id
    $cancelled = (Control @('list')).sessions
    if (($cancelled | Where-Object id -eq $ssh.id).tab_id -ne $ssh.tab_id -or
        ($cancelled | Where-Object id -eq $shell.id).tab_id -ne $shell.tab_id) {
        throw 'Cancelling placement changed the original tabs'
    }
    $fullWidth = ($cancelled | Where-Object id -eq $ssh.id).cols
    Invoke-Button (Choose-Destination)
    $splitGrid = Wait-Until 'original pane IDs and measured split grids in the destination tab' {
        $current = (Control @('list')).sessions
        $moved = $current | Where-Object id -eq $ssh.id
        $destination = $current | Where-Object id -eq $shell.id
        if ($current.Count -eq 2 -and $moved.visible -and $destination.visible -and
            $moved.active -and $moved.tab_id -eq $shell.tab_id -and $destination.tab_id -eq $shell.tab_id -and
            $moved.connection_generation -eq $generation -and $moved.cols -lt $fullWidth -and
            [Math]::Abs($moved.cols - $destination.cols) -le 1 -and $moved.rows -eq $destination.rows) {
            return $moved
        }
    }
    # Group membership is published before WinUI finishes measuring the new
    # canvases. Copy mode is specific to a grid and exits on a later resize.
    $null = Control @('wait-for-text','--pane',"$($ssh.id)",'--timeout','10',"ssh-grid-$($splitGrid.cols)x$($splitGrid.rows)")
    foreach ($letter in 'typed-ssh'.ToCharArray()) { Post-Key 0x102 ([uint32]$letter) }
    $null = Control @('wait-for-text','--pane',"$($ssh.id)",'--timeout','10','typed-ssh')

    Press-Key 0x77 # F8, Keyboard copy mode
    $dot = ' ' + [char]0xb7 + ' '
    $null = Wait-Until 'copy mode in the moved pane' { Copy-ModeOpen }
    Press-Key 0x47 # g, go to oldest retained row
    try {
        $null = Wait-Until 'original scrollback in the moved SSH pane' {
            $document = Find-Element ("Terminal${dot}move-ssh")
            if ($document) {
                $range = $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange
                $range.GetText(-1).Contains('move-retained-row')
            }
        }
    } catch {
        $panes = ((Control @('list')).sessions | Select-Object id,active,visible,cols,rows,tab_id | ConvertTo-Json -Compress)
        $bannerVisible = Copy-ModeOpen
        $document = Find-Element ("Terminal${dot}move-ssh")
        $viewport = if ($document) {
            $document.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(400)
        } else { '(no document)' }
        throw "$_ Copy mode visible: $bannerVisible. Panes: $panes. First displayed cells: $viewport"
    }
    Press-Key 27
    $null = Wait-Until 'copy mode to release input' { -not (Copy-ModeOpen) }
    Press-Key 0x78 # F9, Focus right. SSH was placed left of PowerShell.
    Wait-Focus $shell.id
    $after = Read-ShellPid $shell.id 'after-move'
    if ($after -ne $shellPid) { throw "PowerShell PID changed from $shellPid to $after" }
    $null = Control @('send-text','--pane',"$($ssh.id)",'--no-paste','/cli-ssh')
    $null = Control @('wait-for-text','--pane',"$($ssh.id)",'--timeout','10','/cli-ssh')
    if ((Control @('get-text','--pane',"$($shell.id)")).text.Contains('typed-ssh')) {
        throw 'SSH input also reached PowerShell'
    }
    Wait-Focus $shell.id
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
