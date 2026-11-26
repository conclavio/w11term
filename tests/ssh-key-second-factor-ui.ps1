[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$button = [System.Windows.Automation.ControlType]::Button
$edit = [System.Windows.Automation.ControlType]::Edit

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh', '--cols', '90', '--rows', '24') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object active).id
    $null = Wait-Until 'SSH key accepted and waiting for an additional factor' {
        $question = ((Control @('list')).sessions | Where-Object id -eq $pane).prompt.question
        $question.kind -eq 'authentication' -and $question.name -like 'Verify sign-in as pilot+character@127.0.0.1:*'
    }
    $field = Wait-Until 'masked password field for the second factor' {
        $element = Find-Element 'Password' $edit -Visible
        if ($element -and $element.Current.IsPassword) { $element }
    }
    $field.SetFocus()
    $null = Wait-Until 'password field to receive focus' { $field.Current.HasKeyboardFocus }
    $target = Wait-Until 'WinUI input window for the password' {
        $focused = [FixtureMessages]::FocusedWindow($window, $app.Id)
        if ($focused -ne [IntPtr]::Zero) { $focused }
    }
    foreach ($character in 'fixture-password'.ToCharArray()) {
        if (![FixtureMessages]::PostKey($target, 0x102, [uint32]$character)) {
            throw 'Could not type the test second factor'
        }
    }
    Invoke-Button (Wait-Until 'Sign in button' { Find-Element 'Sign in' $button -Visible })
    $null = Control @('wait-ssh-connected','--pane',"$pane",'--timeout','15')
    $terminal = Control @('wait-for-text','--pane',"$pane",'--timeout','15','mfa-fixture-ready')
    if ($terminal.text -match 'fixture-password') { throw 'The password reached the terminal' }
} finally {
    Close-FixtureApp $app
}
