param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
. (Join-Path $PSScriptRoot 'support/mixed-tab-layout.ps1')

$root = Join-Path $env:W11TERM_DATA_DIR 'command-reference'
$private = Join-Path $root 'pages/linux'
$saved = Join-Path $root 'saved-pages.json'
$only = Join-Path $private 'team-only.md'
$null = [IO.Directory]::CreateDirectory($private)
[IO.File]::WriteAllText((Join-Path $private 'rg.md'), "# Team rg notes`n`n> Search the Team workspace.`n")
[IO.File]::WriteAllText($only, "# Team-only tools`n`n> Private team workflow.`n")
[IO.File]::WriteAllText($profiles, '{"profiles":{"Palette PowerShell":{"backend":"local","executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}}}')

$edit = [System.Windows.Automation.ControlType]::Edit
$combo = [System.Windows.Automation.ControlType]::ComboBox
$listItem = [System.Windows.Automation.ControlType]::ListItem
$value = [System.Windows.Automation.ValuePattern]::Pattern
$expand = [System.Windows.Automation.ExpandCollapsePattern]::Pattern
$select = [System.Windows.Automation.SelectionItemPattern]::Pattern
$savedSection = [System.Windows.Automation.PropertyCondition]::new(
    [System.Windows.Automation.AutomationElement]::AutomationIdProperty, 'saved-reference-pages')
$dot = ' ' + [char]0xb7 + ' '
$script:app = $null

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $control = Find-Element $Name $button
        if ($control -and $control.Current.IsEnabled) { $control }
    })
}

function Start-Reference([string]$Log) {
    $script:app = Start-Process -FilePath $Gui -ArgumentList @(
        '--profiles', ('"' + $profiles + '"'), '--profile', '"Palette PowerShell"'
    ) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State $Log)
    $script:window = Wait-InputWindow
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id
    Wait-Text $pane 'PS '
    Run-Palette 'Command reference'
    $null = Wait-Until 'bundled catalog available offline' {
        Find-Element "Bundled TLDR pages${dot}v2.3"
    }
}

function Stop-Reference {
    if (!$app) { return }
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
    $script:app = $null
}

function Search([string]$Text) {
    $field = Wait-Until 'command reference search field' { Find-Element 'Search command reference' $edit }
    $field.SetFocus()
    $field = Wait-Until 'focused reference search field' {
        $current = Find-Element 'Search command reference' $edit -Visible
        if ($current -and $current.Current.HasKeyboardFocus) { $current }
    }
    $field.GetCurrentPattern($value).SetValue($Text)
}

function Show-Saved {
    $section = Wait-Until 'Saved pages section' {
        $element = [System.Windows.Automation.AutomationElement]::FromHandle($window)
        $element.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $savedSection)
    }
    $pattern = $section.GetCurrentPattern($expand)
    if ($pattern.Current.ExpandCollapseState -eq 'Collapsed') { $pattern.Expand() }
}

function Save-Page {
    Click 'Save page'
    $null = Wait-Until 'manual was saved' {
        $remove = Find-Element 'Remove from saved pages' $button
        $remove -and $remove.Current.IsEnabled
    }
}

try {
    Start-Reference 'gui.log'
    Search 'rg'
    $null = Wait-Until 'the Common manual is active' {
        (Find-Element 'rg') -and (Find-Element "Common${dot}tldr-pages (CC BY 4.0)")
    }
    Save-Page

    $platform = Wait-Until 'platform dropdown' { Find-Element 'Command reference platform' $combo }
    $platform.GetCurrentPattern($expand).Expand()
    $option = Wait-Until 'Linux platform option' { Find-Element 'Linux' $listItem -Visible }
    $option.GetCurrentPattern($select).Select()
    $null = Wait-Until 'the private Linux manual with the same command name is active' {
        (Find-Element 'Team rg notes') -and (Find-Element "Linux${dot}Custom page")
    }
    Save-Page

    Search 'team-only'
    $null = Wait-Until 'the separate private manual is active' { Find-Element 'Team-only tools' }
    Save-Page
    Stop-Reference
    [IO.File]::Delete($only)

    Start-Reference 'restart.log'
    Show-Saved
    Click "Open saved page rg${dot}Common"
    $null = Wait-Until 'the saved Common manual opens after restart' {
        (Find-Element 'rg') -and (Find-Element "Common${dot}tldr-pages (CC BY 4.0)") -and
            (Find-Element 'Remove from saved pages' $button)
    }
    Show-Saved
    Click "Open saved page Team rg notes${dot}Linux"
    $null = Wait-Until 'the saved Linux variant opens without falling back to Common' {
        (Find-Element 'Team rg notes') -and (Find-Element "Linux${dot}Custom page") -and
            (Find-Element 'Remove from saved pages' $button)
    }

    Click 'Remove from saved pages'
    $null = Wait-Until 'the private manual remains open after removing its shortcut' {
        (Find-Element 'Team rg notes') -and (Find-Element 'Save page' $button)
    }
    Show-Saved
    Click "Remove unavailable saved page team-only${dot}Linux"
    $null = Wait-Until 'only the Common manual remains saved on disk' {
        $entries = ([IO.File]::ReadAllText($saved) | ConvertFrom-Json).pages
        $entries.Count -eq 1 -and $entries[0].platform -ceq 'common' -and $entries[0].name -ceq 'rg'
    }
    if (![IO.File]::Exists((Join-Path $private 'rg.md'))) { throw 'Removing a shortcut deleted its manual' }
} finally {
    Stop-Reference
}
