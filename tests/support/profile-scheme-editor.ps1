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
