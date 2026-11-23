param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$env:W11TERM_TEST_REMOTE_COPY_FILE = Join-Path $State 'remote-clipboard.txt'
$focusLabel = 'Limit OSC 52 to the foreground pane'
$allowLabel = 'Allow remote clipboard copy (OSC 52)'

function Send-Osc52([int]$Pane, [string]$Token) {
    # Space requests beyond the process-wide clipboard rate limit so a denied write
    # cannot pass the test merely because another request used its time slot.
    Start-Sleep -Milliseconds 280
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Token))
    $wire = "$([char]27)]52;c;$encoded$([char]7)`r`nseen-$Token"
    $packet = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($wire))
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',$packet)
    Wait-Text $Pane "seen-$Token"
}

function Clipboard-Equals([string]$Token) {
    (Test-Path -LiteralPath $env:W11TERM_TEST_REMOTE_COPY_FILE) -and
        [IO.File]::ReadAllText($env:W11TERM_TEST_REMOTE_COPY_FILE) -ceq $Token
}

function Expect-Clipboard([string]$Token) {
    $null = Wait-Until "remote clipboard value '$Token'" { Clipboard-Equals $Token }
}

function Expect-NoChange([string]$Token) {
    # Allow a queued write time to run after the terminal frame has been published.
    Start-Sleep -Milliseconds 100
    if (!(Clipboard-Equals $Token)) { throw "A disallowed OSC 52 request replaced '$Token'" }
}

function Toggle-Setting([string]$Name, [string]$From, [string]$Property, [bool]$Value) {
    $byName = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::NameProperty, $Name)
    $toggleable = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::IsTogglePatternAvailableProperty, $true)
    $condition = [System.Windows.Automation.AndCondition]::new($byName, $toggleable)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $toggle = Wait-Until "setting '$Name' currently $From" {
        $item = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
        if ($item) {
            $pattern = $item.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
            if ($pattern.Current.ToggleState -eq $From) { $pattern }
        }
    }
    $toggle.Toggle()
    $null = Wait-Until "persisted setting '$Name'" {
        $state = Get-Content -LiteralPath $env:W11TERM_SETTINGS_PATH -Raw | ConvertFrom-Json
        $state.interaction -and $state.interaction.$Property -eq $Value
    }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','132','--rows','36') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'foreground terminal window' { [FixtureMessages]::GetForegroundWindow() -eq $window }
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $first = @(Sessions)[0].id
    Wait-Text $first 'ready'
    $second = (Control @('open','--profile','palette-ssh')).pane
    Wait-Focus $first
    Wait-Text $second 'ready'
    Send-Osc52 $second 'legacy-background'
    Expect-Clipboard 'legacy-background'

    Run-Palette 'Settings'
    Toggle-Setting $focusLabel 'Off' 'osc52ForegroundOnly' $true
    Send-Osc52 $second 'settings-background'
    Expect-NoChange 'legacy-background'

    Press-Key 0x77 # F8 selects the first SSH tab
    Wait-Focus $first
    Send-Osc52 $first 'active-ssh'
    Expect-Clipboard 'active-ssh'
    Send-Osc52 $second 'inactive-ssh'
    Expect-NoChange 'active-ssh'

    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'focused local split' {
        $pane = Sessions | Where-Object { $_.active -and $_.label -eq 'Palette PowerShell' }
        if ($pane) { $pane.id }
    }
    Wait-Text $local 'PS '
    Send-Osc52 $first 'visible-unfocused-ssh'
    Expect-NoChange 'active-ssh'
    Press-Key 0x73 # F4 selects the left SSH split
    Wait-Focus $first
    Send-Osc52 $first 'selected-ssh-split'
    Expect-Clipboard 'selected-ssh-split'

    Run-Palette 'Settings'
    Toggle-Setting $allowLabel 'On' 'osc52' $false
    Press-Key 0x77
    Wait-Focus $first
    Send-Osc52 $first 'master-off'
    Expect-NoChange 'selected-ssh-split'
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
