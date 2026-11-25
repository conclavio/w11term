[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_EXPORT_SMOKE_DIR = $State
$profiles = Join-Path $State 'profiles.json'
$label = 'Save scrollback as text' + [char]0x2026

function Owned-Dialog([IntPtr]$Owner, [string]$Title) {
    foreach ($handle in [FixtureDialog]::Windows($app.Id)) {
        if ([FixtureDialog]::GetWindow($handle, 4) -ne $Owner) { continue }
        $candidate = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        if ($candidate.Current.ClassName -eq '#32770' -and (!$Title -or $candidate.Current.Name -eq $Title)) {
            return $candidate
        }
    }
}
function Dialog-Control($Dialog, $Type, [string]$AutomationId) {
    $byType = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $Type)
    $byId = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::AutomationIdProperty, $AutomationId)
    return $Dialog.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.AndCondition]::new($byType, $byId))
}
function Open-Export([switch]$Menu) {
    if ($Menu) {
        Invoke-Button (Wait-Until 'tab menu' { Find-Element 'Tab menu' })
        $sub = Wait-Until 'Panes submenu' { Find-Element 'Panes' ([System.Windows.Automation.ControlType]::MenuItem) }
        $sub.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
        Invoke-Button (Wait-Until 'export action in Panes' { Find-Element $label })
    } else {
        $field = Open-Palette $label
        $field.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($label)
        $entry = Wait-Until 'enabled export action in palette' {
            $button = Find-Element $label ([System.Windows.Automation.ControlType]::Button)
            if ($button -and $button.Current.IsEnabled) { return $button }
        }
        Invoke-Button $entry
    }
    try {
        return Wait-Until 'native save dialog on the fixture window' { Owned-Dialog $window 'Save scrollback as text' }
    } catch {
        $visible = foreach ($h in [FixtureDialog]::Windows($app.Id)) {
            $el = [System.Windows.Automation.AutomationElement]::FromHandle($h)
            "$h '$($el.Current.Name)' $($el.Current.ClassName) owned by $([FixtureDialog]::GetWindow($h,4))"
        }
        throw "$_ Fixture HWND $window. Visible fixture windows: $($visible -join ', ')"
    }
}
function Set-Filename($Dialog, [string]$Path) {
    $handle = [IntPtr]::new($Dialog.Current.NativeWindowHandle)
    $null = Wait-Until 'editable suggested filename terminal.txt' {
        [FixtureDialog]::SetFilename($handle, $Path)
    }
}
function Click-DialogButton($Dialog, [string]$Id) {
    $handle = [IntPtr]::new($Dialog.Current.NativeWindowHandle)
    $null = Wait-Until "dialog button $Id" {
        if ([FixtureDialog]::ClickButton($handle, [int]$Id)) { return $true }
        $button = Dialog-Control $Dialog ([System.Windows.Automation.ControlType]::Button) $Id
        if (!$button -and $Id -eq '6') {
            $yes = [System.Windows.Automation.PropertyCondition]::new(
                [System.Windows.Automation.AutomationElement]::NameProperty, 'Yes')
            $button = $Dialog.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $yes)
        }
        if ($button -and $button.Current.IsEnabled) {
            $invoke = $null
            if ($button.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$invoke)) {
                $invoke.Invoke()
                return $true
            }
            if ($Id -eq '6') { return [FixtureDialog]::ConfirmReplace($handle) }
        }
    }
}
function Wait-Saved([string]$Path) {
    $prefix = "Saved scrollback for pane $remote to "
    $buttons = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $exportEvidence = @{ Status = '(no export status)' }
    try {
        $null = Wait-Until "export to $Path" {
            foreach ($button in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $buttons)) {
                $name = $button.Current.Name
                if ($name.StartsWith($prefix)) {
                    $exportEvidence.Status = $name
                    # Match the file actually reported. Shell display names can differ
                    # in casing, DOS aliases, or the spelling of the user's profile path.
                    return [FixtureDialog]::SameFile($Path, $name.Substring($prefix.Length))
                }
                if ($name.StartsWith('Could not save scrollback')) { $exportEvidence.Status = $name }
            }
        }
    } catch {
        $open = foreach ($handle in [FixtureDialog]::Windows($app.Id)) {
            $candidate = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
            if ($candidate.Current.ClassName -eq '#32770') {
                "$($candidate.Current.Name) (HWND $handle, owner $([FixtureDialog]::GetWindow($handle,4)))"
            }
        }
        throw "$_ Destination exists: $([IO.File]::Exists($Path)). Status: $($exportEvidence.Status). Open fixture dialogs: [$($open -join ', ')]."
    }
    Wait-Focus $remote
}
function Shell-Pid([string]$Tag) {
    $command = "'$Tag=' + " + '$PID'
    foreach ($c in $command.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    Press-Key 13
    $pattern = "$Tag=([0-9]+)"
    $text = (Control @('wait-for-text','--pane',"$local",'--regex','--timeout','15',$pattern)).text
    return [int]([regex]::Match($text, $pattern).Groups[1].Value)
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles', ('"' + $profiles + '"'), '--profile', 'palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $manifest = Get-ControlManifestPath $app.Id
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath $manifest }
    $remote = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','15','ready')
    Run-Palette 'Split right with Palette PowerShell'
    $local = Wait-Until 'PowerShell split' { ((Control @('list')).sessions | Where-Object label -eq 'Palette PowerShell').id }
    Wait-Focus $local
    $null = Control @('wait-for-text','--pane',"$local",'--timeout','15','PS ')
    $shellPid = Shell-Pid 'neighbor-before'
    Picker-Query 'palette-ssh'
    Press-Key 13
    Wait-Focus $remote
    $inputBytes = [IO.File]::ReadAllBytes((Join-Path $State 'history.vt'))
    $null = Control @('send-text','--pane',"$remote",'--no-paste','--base64',[Convert]::ToBase64String($inputBytes))
    $frame = Control @('wait-for-text','--pane',"$remote",'--timeout','15','snapshot-end')
    if ($frame.text.Contains('archived') -or $frame.cols -ge 510) { throw 'Fixture needs offscreen and soft-wrapped content' }

    $new = Join-Path $State '.\new.txt'
    $dialog = Open-Export -Menu
    # Output that arrives while the user chooses the file belongs to the next capture.
    $null = Control @('send-text','--pane',"$remote",'--no-paste','arrived-after-picker')
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','arrived-after-picker')
    Set-Filename $dialog $new
    Click-DialogButton $dialog '1'
    Wait-Saved $new

    $dialog = Open-Export
    Set-Filename $dialog (Join-Path $State 'cancel.txt')
    Click-DialogButton $dialog '2'
    $null = Wait-Until 'save dialog canceled' { !(Owned-Dialog $window 'Save scrollback as text') }
    Wait-Focus $remote

    $replacement = Join-Path $State 'replace.txt'
    $dialog = Open-Export
    Set-Filename $dialog $replacement
    $pickerHandle = [IntPtr]::new($dialog.Current.NativeWindowHandle)
    Click-DialogButton $dialog '1'
    $confirm = Wait-Until 'overwrite confirmation owned by the save dialog' { Owned-Dialog $pickerHandle 'Confirm Save As' }
    $null = Wait-Until 'the overwrite prompt to name the fixture replace.txt' {
        foreach ($node in $confirm.FindAll([System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.Condition]::TrueCondition)) {
            if ($node.Current.Name -match '(?i)(?<![\w.-])replace\.txt(?![\w.-])') { return $true }
        }
    }
    Click-DialogButton $confirm '6' # IDYES
    Wait-Saved $replacement
    foreach ($c in 'remote-resumed'.ToCharArray()) { Post-Key 0x102 ([uint32]$c) }
    $null = Control @('wait-for-text','--pane',"$remote",'--timeout','10','remote-resumed')
    Picker-Query 'Palette PowerShell'
    Press-Key 13
    Wait-Focus $local
    if ((Shell-Pid 'neighbor-after') -ne $shellPid) { throw 'PowerShell PID changed during export' }
} catch {
    throw "$_ at $($_.ScriptStackTrace)"
} finally {
    if (!$app.HasExited) {
        $null = $app.CloseMainWindow()
        if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
    }
    $app.Dispose()
}
