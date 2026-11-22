param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$localName = 'Palette PowerShell'
$catalog = Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json
$catalog.profiles.'palette-ssh' | Add-Member -Force -NotePropertyName scrollback_lines -NotePropertyValue 7
# Omission exercises a catalog written before the scrollback setting existed.
$catalog.profiles.$localName.PSObject.Properties.Remove('scrollback_lines')
[IO.File]::WriteAllText($profiles, ($catalog | ConvertTo-Json -Depth 20))
$button = [System.Windows.Automation.ControlType]::Button

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $entry = Find-Element $Name $button
        if ($entry -and $entry.Current.IsEnabled) { $entry }
    })
}
function Send-Command([int]$Pane, [string]$Command) {
    $bytes = [Text.Encoding]::UTF8.GetBytes($Command + [char]13)
    $null = Control @('send-text','--pane',"$Pane",'--no-paste','--base64',[Convert]::ToBase64String($bytes))
}
function Wait-PromptAfter([int]$Pane, [string]$Marker) {
    $pattern = [regex]::Escape($Marker) + '[ ]*\r?\nPS fixture>'
    $null = Control @('wait-for-text','--pane',"$Pane",'--regex','--timeout','15',$pattern)
}
function Open-LocalHistory {
    $pane = (Control @('open','--profile',$localName,'--activate')).pane
    Wait-Focus $pane
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','PS ')
    # The checkout path can exceed the pane width in PowerShell's default prompt.
    Send-Command $pane 'function global:prompt { "PS fixture> " }; "short-prompt-" + "ready"'
    Wait-PromptAfter $pane 'short-prompt-ready'
    # Form each marker at runtime so that the echoed command cannot satisfy a query.
    Send-Command $pane '0..179 | % { "local-row-{0:D3}" -f $_ }; "local-history-" + "ready"'
    Wait-PromptAfter $pane 'local-history-ready'
    return $pane
}
function Row-Numbers([string]$Text, [string]$Prefix) {
    foreach ($match in [regex]::Matches($Text, ($Prefix + '-row-(\d{3})'))) { [int]$match.Groups[1].Value }
}
function Expect-Find([int]$Pane, [string]$Term, [int]$Count) {
    $reply = (Control @('find-text','--pane',"$Pane",$Term)).panes
    if ($reply.Count -ne 1 -or $reply[0].pane -ne $Pane -or $reply[0].count -ne $Count -or $reply[0].truncated) {
        throw "Unexpected search result for '$Term' in pane $Pane. $($reply | ConvertTo-Json -Depth 4 -Compress)"
    }
}
function Check-History([int]$Pane, [string]$Prefix, $Limit) {
    $screen = (Control @('get-text','--pane',"$Pane")).text
    $live = @(Row-Numbers $screen $Prefix)
    if (!$live.Count -or $live[0] -le 7 -or $live[-1] -ne 179) {
        throw "Expected numbered output to fill and overflow pane $Pane. Visible rows: $live"
    }
    $path = Join-Path $State "history-$Pane.txt"
    $null = Control @('save-text','--pane',"$Pane",$path)
    $retained = @(Row-Numbers ([IO.File]::ReadAllText($path)) $Prefix)
    # Derive the boundary from the live screen. Hosted runners can have a different DPI.
    $first = if ($null -eq $Limit) { 0 } else { [Math]::Max(0, $live[0] - $Limit) }
    if (($retained -join ',') -cne (($first..179) -join ',')) {
        throw "Unexpected exported history for pane $Pane. First live row $($live[0]), expected oldest $first, exported rows: $retained"
    }
    Expect-Find $Pane ("$Prefix-row-{0:D3}" -f $first) 1
    if ($first -gt 0) { Expect-Find $Pane ("$Prefix-row-{0:D3}" -f ($first - 1)) 0 }
    if ((Control @('get-text','--pane',"$Pane")).text -cne $screen) { throw "Inspecting history moved pane $Pane" }
    return $first
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh','--cols','100','--rows','24') -PassThru -WindowStyle Hidden -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'fixture control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
    $sshPane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$sshPane",'--timeout','15','ssh-history-ready')
    $original = Open-LocalHistory
    Run-Palette 'Settings'
    Click "Edit local profile $localName"
    $field = Wait-Until 'editable scrollback limit' {
        Find-Element 'Scrollback lines' ([System.Windows.Automation.ControlType]::Edit)
    }
    $value = $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    if ($value.Current.Value -cne '10000') { throw "Expected default 10000, got '$($value.Current.Value)'" }
    $value.SetValue('0')
    Click 'Save profile'
    $null = Wait-Until 'zero saved distinctly from an omitted limit' {
        $profile = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles.$localName
        $profile.PSObject.Properties['scrollback_lines'] -and $profile.scrollback_lines -ceq 0
    }
    $zero = Open-LocalHistory
    if ($zero -eq $original) { throw 'Expected a fresh local pane after editing the profile' }
    Send-Command $original '"old-pane-still-" + "live"'
    Wait-PromptAfter $original 'old-pane-still-live'
    $null = Check-History $sshPane 'ssh' 7
    $null = Check-History $original 'local' $null
    $null = Check-History $zero 'local' 0
    Wait-Focus $zero
    foreach ($c in '"zero-focus-" + "ok"'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $null = Control @('wait-for-text','--pane',"$zero",'--timeout','15','zero-focus-ok')

    Press-Key 0x77 # F8 selects the SSH tab in the fixture's keymap.
    Wait-Focus $sshPane
    Run-Palette 'Find in scrollback    Ctrl+Shift+F'
    $field = Wait-Until 'focused SSH scrollback search' {
        $edit = Find-Element 'Find in scrollback' ([System.Windows.Automation.ControlType]::Edit)
        if ($edit -and $edit.Current.HasKeyboardFocus) { $edit }
    }
    # The peer prints two more lines on every PTY resize. A delayed measurement
    # on tab activation can evict the oldest row even after the tab gains focus.
    # Wait for a fresh grid acknowledgement before taking the history baseline.
    $grid = (Control @('list')).sessions | Where-Object id -eq $sshPane
    $shown = (Control @('get-text','--pane',"$sshPane")).text
    $cols = $grid.cols
    do {
        $cols--
        $ack = "ssh-grid-${cols}x$($grid.rows)"
    } while ($shown.Contains($ack))
    $null = Control @('resize','--pane',"$sshPane",'--cols',"$cols",'--rows',"$($grid.rows)")
    $null = Control @('wait-for-text','--pane',"$sshPane",'--timeout','15',$ack)
    $beforeScroll = Join-Path $State 'before-scroll.txt'
    $null = Control @('save-text','--pane',"$sshPane",$beforeScroll)
    $before = [IO.File]::ReadAllText($beforeScroll)
    $oldest = 'ssh-row-{0:D3}' -f (@(Row-Numbers $before 'ssh')[0])
    $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($oldest)
    $statusId = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'find-status')
    $null = Wait-Until "one local Find match for $oldest" {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $status = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $statusId)
        $status -and $status.Current.Name -ceq '1 / 1'
    }
    Press-Key 27
    $null = Wait-Until 'Find dismissed' { !(Find-Element 'Find in scrollback' ([System.Windows.Automation.ControlType]::Edit)) }
    $null = Wait-Until "oldest retained SSH row $oldest in the visible viewport" {
        $doc = Find-Element ('Terminal ' + [char]0xb7 + ' palette-ssh')
        if ($doc) { $doc.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern).DocumentRange.GetText(-1).Contains($oldest) }
    }
    if ((Control @('get-text','--pane',"$sshPane")).text.Contains($oldest)) { throw 'get-text returned the scrolled viewport' }
    $scrolled = Join-Path $State 'scrolled.txt'
    $null = Control @('save-text','--pane',"$sshPane",$scrolled)
    if ([IO.File]::ReadAllText($scrolled) -cne $before) {
        throw 'Scrolling to the oldest row changed the exported SSH history'
    }
    foreach ($c in 'history-focus-ok'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$sshPane",'--timeout','15','history-focus-ok')
} catch {
    $failure = $_
    if ($sshPane -and $statusId) {
        $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $status = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $statusId)
        # The accessible terminal document is unmounted while native Find owns input.
        $edit = Find-Element 'Find in scrollback' ([System.Windows.Automation.ControlType]::Edit)
        if ($edit) { $query = $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value }
        $now = Join-Path $State 'failed-history.txt'
        $null = Control @('save-text','--pane',"$sshPane",$now)
        $nowText = [IO.File]::ReadAllText($now)
        $current = (Control @('list')).sessions | Where-Object id -eq $sshPane
        throw "$failure. Pane $sshPane active=$($current.active), $($current.cols)x$($current.rows). Find query '$query', status '$($status.Current.Name)'. Original SSH history: '$before'. Current SSH history: '$nowText'."
    }
    throw
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
