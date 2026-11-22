param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/palette-pixels.ps1')
. (Join-Path $PSScriptRoot 'support/profile-scheme-editor.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$catalog = [IO.File]::ReadAllText($profiles) | ConvertFrom-Json
# A second saved profile for the same pinned peer predates the scheme setting.
$catalog.profiles | Add-Member NoteProperty 'legacy-dark' $catalog.profiles.'palette-ssh'
[IO.File]::WriteAllText($profiles, ($catalog | ConvertTo-Json -Depth 20))
function Send-To([int]$Pane, [string]$Text) {
    $encoded = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($Text))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$encoded)
}
function Paint([int]$Pane) {
    Send-To $Pane $sample
    $null = Control @('wait-for-text','--pane',"$Pane",'--timeout','15','palette-witness')
}
function Apply-Remote([int]$Pane, [string]$Sequence, [string]$Title) {
    $previous = (Control @('get-text','--pane',"$Pane")).text
    Send-To $Pane ($Sequence + $esc + ']2;' + $Title + $bell)
    $frame = Wait-Until "$Title palette change committed" {
        $result = Control @('get-text','--pane',"$Pane")
        if ($result.title -ceq $Title) { $result }
    }
    if ($frame.text -cne $previous) { throw "$Title changed the existing cell text" }
}

for ($launch=1; $launch -le 2; $launch++) {
    $failure = $null
    $log = Join-Path $State "gui-$launch.log"
    $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','86','--rows','15') -WindowStyle Hidden -PassThru -RedirectStandardError $log
    try {
        $window = Wait-InputWindow
        if ($launch -eq 1) {
            $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
            $root.GetCurrentPattern([System.Windows.Automation.TransformPattern]::Pattern).Resize(900,422)
        }
        $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
        $ssh = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
        $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','ready')
        Paint $ssh
        if ($launch -eq 1) {
            Check-Pixels $ssh 'before-edit' $legacy
            Edit-Scheme 'palette-ssh' 'Edit profile' 'One Half Light' 'Save profile changes'
            Edit-Scheme 'Palette PowerShell' 'Edit local profile' 'One Half Dark' 'Save profile'
            if (Saved-Scheme 'legacy-dark') { throw 'Editing another profile changed the legacy profile' }
            Check-Pixels $ssh 'existing-after-save' $legacy
            continue
        }
        Run-Palette 'Split right with legacy-dark'
        $neighbor = Wait-Until 'default scheme in adjacent SSH pane' {
            $sessions = @((Control @('list')).sessions)
            $pane = $sessions | Where-Object label -eq 'legacy-dark'
            if ($sessions.Count -eq 2 -and $pane.active -and $sessions[0].tab_id -eq $sessions[1].tab_id) { $pane.id }
        }
        $null = Control @('wait-for-text','--pane',"$neighbor",'--timeout','15','ready')
        Paint $neighbor
        Check-Pixels $ssh 'saved-light' $light
        Check-Pixels $neighbor 'legacy-split' $legacy

        Apply-Remote $ssh ($esc + ']4;1;#00ffff' + $bell + $esc + ']11;#001155' + $bell) 'changed-colors'
        Check-Pixels $ssh 'osc-overrides' @($light[0],'#001155','#00ffff')
        Apply-Remote $ssh ($esc + ']104' + $bell) 'indexed-only'
        Check-Pixels $ssh 'indexed-reset' @($light[0],'#001155',$light[2])
        Apply-Remote $ssh ($esc + ']111' + $bell + $esc + '[?5h') 'light-reversed'
        Check-Pixels $ssh 'reversed' @($light[1],$light[0],$light[2])
        Check-Pixels $neighbor 'unchanged-neighbor' $legacy
        Apply-Remote $ssh ($esc + '[?5l') 'light-restored'
        Check-Pixels $ssh 'restored' $light
        Wait-Focus $neighbor

        $local = (Control @('open','--profile','Palette PowerShell','--activate')).pane
        $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
        $wire = [Convert]::ToBase64String([Text.Encoding]::ASCII.GetBytes($sample))
        Send-To $local ("function global:prompt { 'PS fixture> ' }; [Console]::Write([Text.Encoding]::ASCII.GetString([Convert]::FromBase64String('$wire')))" + [char]13)
        $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','--regex','palette-witness *\r?\nPS fixture>')
        Check-Pixels $local 'saved-local-dark' $dark
    } catch {
        $failure = $_
        try {
            $diskState = foreach ($entry in 'palette-ssh','Palette PowerShell') {
                $value = Saved-Scheme $entry
                if (!$value) { $value = '(default/omitted)' }
                "$entry='$value'"
            }
        } catch { $diskState = "Profile catalog unavailable: $_" }
        try { $uiState = Editor-State } catch { $uiState = "UI diagnostics unavailable: $_" }
    } finally {
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
    if ($failure) {
        try { $details = [IO.File]::ReadAllText($log) } catch { $details = "GUI log unavailable: $_" }
        throw "Launch $launch failed: $failure at $($failure.ScriptStackTrace). Saved schemes: $($diskState -join ', '). $uiState. GUI: $details"
    }
}
