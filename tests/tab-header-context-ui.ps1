param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$tab = [System.Windows.Automation.ControlType]::TabItem
$menu = [System.Windows.Automation.ControlType]::MenuItem
$edit = [System.Windows.Automation.ControlType]::Edit
$button = [System.Windows.Automation.ControlType]::Button
$value = [System.Windows.Automation.ValuePattern]::Pattern
$config = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
foreach ($label in 'Left shell','Keep shell','Right shell','Pinned shell') {
    $config.profiles | Add-Member -NotePropertyName $label -NotePropertyValue $config.profiles.'Palette PowerShell'
}
[IO.File]::WriteAllText($profiles, ($config | ConvertTo-Json -Depth 8))

. (Join-Path $PSScriptRoot 'support/tab-context.ps1')

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile',
    'palette-ssh', '--cols', '115', '--rows', '26') -WindowStyle Normal -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $ssh = (Wait-Until 'SSH session' { (Control @('list')).sessions | Where-Object label -eq 'palette-ssh' }).id
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','15','ready')
    $shell = (Control @('open','--profile','Palette PowerShell')).pane
    Wait-Focus $ssh
    $renamed = 'Background shell'
    Invoke-Button (Open-Context 'Palette PowerShell' ('Rename tab' + [char]0x2026))
    $field = Wait-Until 'tab name editor' { Find-Element 'Tab name' $edit -Visible }
    $field.GetCurrentPattern($value).SetValue($renamed)
    Invoke-Button (Wait-Until 'Save the background tab name' { Find-Element 'Save' $button -Visible })
    $null = Wait-Until 'only the background shell was renamed' {
        $sessions = (Control @('list')).sessions
        ($sessions | Where-Object id -eq $shell).tab_title -ceq $renamed -and
            !($sessions | Where-Object id -eq $ssh).tab_title -and
            !(Find-Element 'Tab name' $edit -Visible)
    }
    Wait-Focus $ssh
    Invoke-Button (Open-Context $renamed 'Pin tab')
    $pinned = "Pinned: $renamed"
    $null = Wait-Until 'the background tab is pinned' { Find-Element $pinned $tab -Visible }
    Wait-Focus $ssh
    $close = Open-Context $pinned 'Close tab'
    if ($close.Current.IsEnabled) { throw 'Pinned tab could be closed' }
    Invoke-Button (Wait-Until 'Unpin tab' { Find-Element 'Unpin tab' $menu -Visible })
    $null = Wait-Until 'the background tab was unpinned' { Find-Element $renamed $tab -Visible }
    Wait-Focus $ssh
    $colors = Open-Context $renamed 'Tab color'
    $colors.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
    Invoke-Button (Wait-Until 'blue tab color' { Find-Element 'Set tab color to Blue' $menu -Visible })
    $colored = "$renamed (Blue tab)"
    $null = Wait-Until 'the background tab changed color' { Find-Element $colored $tab -Visible }
    Wait-Focus $ssh
    foreach ($ch in 'still-in-ssh'.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','12','still-in-ssh')

    Invoke-Button (Open-Context $colored 'Duplicate tab layout')
    $copy = Wait-Until 'a new local session from the clicked tab is active' {
        $session = (Control @('list')).sessions | Where-Object {
            $_.id -ne $shell -and $_.id -ne $ssh -and $_.active -and $_.tab_title -ceq $renamed
        }
        if ($session) { $session.id }
    }
    $null = Control @('wait-for-text','--pane',"$copy",'--timeout','15','PS ')
    $null = Control @('close-pane','--pane',"$copy")
    $sshHeader = Wait-Until 'SSH tab header' { Find-Element 'palette-ssh' $tab -Visible }
    $sshHeader.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Wait-Focus $ssh
    Invoke-Button (Open-Context $colored 'Close tab')
    $null = Wait-Until 'the clicked shell closed and the original SSH remains active' {
        $sessions = @((Control @('list')).sessions)
        $sessions.Count -eq 1 -and $sessions[0].id -eq $ssh -and $sessions[0].active
    }

    $left = (Control @('open','--profile','Left shell')).pane
    $anchor = (Control @('open','--profile','Keep shell')).pane
    $right = (Control @('open','--profile','Right shell')).pane
    $saved = (Control @('open','--profile','Pinned shell')).pane
    Invoke-Button (Open-Context 'Pinned shell' 'Pin tab')
    $null = Wait-Until 'the protected shell is pinned' { Find-Element 'Pinned: Pinned shell' $tab -Visible }
    Invoke-Button (Open-Context 'Keep shell' 'Close tabs to the right')
    $null = Wait-Until 'only the tab to the right of the clicked tab closes' {
        $sessions = @((Control @('list')).sessions)
        $sessions.Count -eq 4 -and $ssh -in $sessions.id -and $left -in $sessions.id -and
            $anchor -in $sessions.id -and $saved -in $sessions.id -and $right -notin $sessions.id
    }
    Wait-Focus $ssh
    foreach ($ch in '-after-close-right'.ToCharArray()) { Post-Key 0x102 ([uint32]$ch) }
    $null = Control @('wait-for-text','--pane',"$ssh",'--timeout','12','still-in-ssh-after-close-right')

    $right = (Control @('open','--profile','Right shell','--activate')).pane
    Wait-Focus $right
    Invoke-Button (Open-Context 'Keep shell' 'Close other tabs')
    $null = Wait-Until 'Close other tabs preserves the clicked anchor and the separate pin' {
        $sessions = @((Control @('list')).sessions)
        $sessions.Count -eq 2 -and $anchor -in $sessions.id -and $saved -in $sessions.id
    }
    Wait-Focus $anchor
    $null = Control @('wait-for-text','--pane',"$anchor",'--timeout','15','PS ')
} finally {
    Close-FixtureApp $app
}
