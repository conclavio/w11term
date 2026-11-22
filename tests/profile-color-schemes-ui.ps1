param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
. (Join-Path $PSScriptRoot 'support/palette-pixels.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$catalog = [IO.File]::ReadAllText($profiles) | ConvertFrom-Json
# A second saved profile for the same pinned peer predates the scheme setting.
$catalog.profiles | Add-Member NoteProperty 'legacy-dark' $catalog.profiles.'palette-ssh'
[IO.File]::WriteAllText($profiles, ($catalog | ConvertTo-Json -Depth 20))
$combo = [System.Windows.Automation.ControlType]::ComboBox
$listItem = [System.Windows.Automation.ControlType]::ListItem
$button = [System.Windows.Automation.ControlType]::Button
$selection = [System.Windows.Automation.SelectionPattern]::Pattern
$expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern

function Editor-Scroll($Element) {
    $walker = [System.Windows.Automation.TreeWalker]::RawViewWalker
    while ($Element -and $Element.Current.ProcessId -eq $app.Id) {
        $pattern = $null
        if ($Element.TryGetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern,[ref]$pattern) -and $pattern.Current.VerticallyScrollable) {
            return $pattern
        }
        $Element = $walker.GetParent($Element)
    }
}
function Editor-State {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
    $focusName = if ($focused -and $focused.Current.ProcessId -eq $app.Id) { $focused.Current.Name } else { '(outside fixture)' }
    $field = Find-Element 'Terminal color scheme' $combo
    $saveState = foreach ($label in 'Save profile changes','Save profile') {
        $control = Find-Element $label $button
        if ($control) {
            $view = Editor-Scroll $control
            "$label focused=$($control.Current.HasKeyboardFocus) offscreen=$($control.Current.IsOffscreen) bounds=$($control.Current.BoundingRectangle) scroll=$($view.Current.VerticalScrollPercent)"
        }
    }
    $chosen = if ($field) {
        $field.GetCurrentPattern($selection).Current.GetSelection() | ForEach-Object { $_.Current.Name }
    }
    $byText = [System.Windows.Automation.PropertyCondition]::new([System.Windows.Automation.AutomationElement]::ControlTypeProperty,[System.Windows.Automation.ControlType]::Text)
    $labels = @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$byText) | ForEach-Object { $_.Current.Name })
    $scroll = Editor-Scroll $field
    if ($scroll) {
        # SSH validation and save feedback live above the profile fields.
        $scroll.SetScrollPercent(-1,0)
        $null = Wait-Until 'profile feedback at the top of the editor' { $scroll.Current.VerticalScrollPercent -lt 1 }
        $labels += @($root.FindAll([System.Windows.Automation.TreeScope]::Descendants,$byText) | ForEach-Object { $_.Current.Name })
    }
    $labels = $labels | Where-Object { $_ } | Select-Object -Unique
    "Focus '$focusName', scheme '$chosen', window $($root.Current.BoundingRectangle), $saveState, editor text including validation: $($labels -join ' | ')"
}

function Click([string]$Label) {
    Invoke-Button (Wait-Until "button '$Label'" {
        $element = Find-Element $Label $button
        if ($element -and $element.Current.IsEnabled) { $element }
    })
}
function Saved-Scheme([string]$Name) {
    $share = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
    $file = [IO.File]::Open($profiles,[IO.FileMode]::Open,[IO.FileAccess]::Read,$share)
    $reader = [IO.StreamReader]::new($file)
    try { ($reader.ReadToEnd() | ConvertFrom-Json).profiles.$Name.color_scheme } finally { $reader.Dispose() }
}
function Edit-Scheme([string]$Name, [string]$EditLabel, [string]$Scheme, [string]$Save) {
    Click 'Tab menu'
    Invoke-Button (Wait-Until 'Settings menu item' { Find-Element 'Settings' ([System.Windows.Automation.ControlType]::MenuItem) })
    Click "$EditLabel $Name"
    $field = Wait-Until "scheme editor for saved profile '$Name'" {
        $profileName = Find-Element 'Profile name' ([System.Windows.Automation.ControlType]::Edit)
        $box = Find-Element 'Terminal color scheme' $combo
        if ($profileName -and $box -and $profileName.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq $Name) {
            $box
        }
    }
    # WinUI can expose the editor's controls outside the ScrollViewer viewport.
    # Focus scrolls the selector into view before the native popup is opened.
    $field.SetFocus()
    $field = Wait-Until "visible scheme selector for '$Name' with its default selected" {
        $box = Find-Element 'Terminal color scheme' $combo
        if ($box -and !$box.Current.IsOffscreen) {
            $old = $box.GetCurrentPattern($selection).Current.GetSelection()
            if ($old.Count -eq 1 -and $old[0].Current.Name -ceq 'w11term (default)') { $box }
        }
    }
    $field.GetCurrentPattern($expand).Expand()
    $choice = Wait-Until "expanded choice '$Scheme' for '$Name'" {
        $item = Find-Element $Scheme $listItem
        if ($item -and $item.Current.IsEnabled -and !$item.Current.IsOffscreen) { $item }
    }
    $choice.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    $field = Wait-Until "selected '$Scheme' in '$Name' editor" {
        $box = Find-Element 'Terminal color scheme' $combo
        if ($box) {
            $selected = $box.GetCurrentPattern($selection).Current.GetSelection()
            if ($selected.Count -eq 1 -and $selected[0].Current.Name -ceq $Scheme) { $box }
        }
    }
    $popup = $field.GetCurrentPattern($expand)
    if ($popup.Current.ExpandCollapseState -eq 'Expanded') { $popup.Collapse() }
    $scroll = Editor-Scroll $field
    Click $Save
    # The profile store replaces the file atomically. Wait for the UI thread to
    # finish that replacement before opening the destination, even with delete sharing.
    if ($Save -eq 'Save profile changes') {
        if ($scroll) { $scroll.SetScrollPercent(-1,0) }
        $null = Wait-Until "SSH profile '$Name' save completed" { Find-Element 'Changes saved.' }
    } else {
        $null = Wait-Until "local profile '$Name' returned to Settings" { Find-Element "$EditLabel $Name" $button }
    }
    $serialized = $Scheme.ToLowerInvariant().Replace(' ','-')
    $actual = Saved-Scheme $Name
    if ($actual -cne $serialized) { throw "Saved '$Name' with scheme '$actual', expected '$serialized'" }
}
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
