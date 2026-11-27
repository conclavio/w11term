$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$keymap = @{ shortcuts = @{
    command_palette='F7'; select_tab_1='F8'; select_tab_2='F9'; close_tab='F10'
    zoom_pane='F6'; focus_left='F4'; focus_right='F2'
} }
[IO.File]::WriteAllText($env:W11TERM_SETTINGS_PATH, ($keymap | ConvertTo-Json))
$button = [System.Windows.Automation.ControlType]::Button

function Sessions { (Control @('list')).sessions }
function Type-Text([string]$Text) { foreach ($char in $Text.ToCharArray()) { Post-Key 0x102 ([uint32]$char) } }
function Type-Line([string]$Text) { Type-Text $Text; Press-Key 13 }
function Wait-Text([int]$Pane, [string]$Text) { $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15',$Text) }
function Wait-DiagnosticArtifact([string]$Title, [string]$ExpectedFile, [string]$StopReason = '') {
    $text = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Text)
    $observed = @{ Summary = '(missing)' }
    try {
        $null = Wait-Until "diagnostics activity shows '$Title' for the selected file" {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            foreach ($item in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $text)) {
                $summary = $item.Current.Name
                $lines = $summary -split '\r\n|\n|\r'
                if ($lines[0] -cne $Title) { continue }
                $observed.Summary = $summary
                if ($lines.Count -ge 2 -and (!$StopReason -or $lines -ccontains $StopReason) -and
                    [FixtureDialog]::SameFile($ExpectedFile, $lines[-1])) { return $true }
            }
        }
    } catch {
        throw "$_ Expected file [$ExpectedFile]. Last activity [$($observed.Summary)]."
    }
}
function Send-Remote([int]$Pane, [string]$Text) {
    $bytes = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Text))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$bytes)
    Wait-Text $Pane $Text
}
function Identity([int]$Pane, [string]$Marker, [string]$Setup) {
    # Only evaluated PowerShell output can match the numerical PID and value.
    Type-Line ($Setup + "'$Marker=' + " + '$PID + "/" + $global:DupValue')
    $pattern = "$Marker=([0-9]+)/([a-z]+)"
    $screen = (Control @('wait-for-text','--pane',"$Pane",'--timeout','15','--regex',$pattern)).text
    return [regex]::Match($screen, $pattern).Groups
}
function Read-PaneHistory([int]$Pane) {
    $file = Join-Path $State ([guid]::NewGuid().ToString() + '.txt')
    $null = Control @('save-text','--pane',"$Pane",$file)
    return [IO.File]::ReadAllText($file)
}
function Check-History([int]$Pane, [string]$Own, [string]$Other) {
    $history = Read-PaneHistory $Pane
    if (!$history.Contains($Own) -or $history.Contains($Other)) {
        throw "Pane $Pane history did not isolate '$Own' from '$Other'"
    }
}
function Wait-Zoom([int]$PowerShellPane, [int]$SshPane) {
    $null = Wait-Until "zoomed PowerShell pane $PowerShellPane" {
        $all = @(Sessions)
        $local = $all | Where-Object id -eq $PowerShellPane
        $ssh = $all | Where-Object id -eq $SshPane
        $local.active -and $local.visible -and $local.cols -gt $sourceShell.cols -and $ssh -and !$ssh.visible
    }
}
function Wait-Split([int]$PowerShellPane, [int]$SshPane) {
    $null = Wait-Until 'both split panes to take the exact original cell boxes' {
        $all = @(Sessions)
        $local = $all | Where-Object id -eq $PowerShellPane
        $ssh = $all | Where-Object id -eq $SshPane
        $local.visible -and $ssh.visible -and $local.cols -eq $sourceShell.cols -and
            $local.rows -eq $sourceShell.rows -and $ssh.cols -eq $sourceSsh.cols -and $ssh.rows -eq $sourceSsh.rows
    }
}
