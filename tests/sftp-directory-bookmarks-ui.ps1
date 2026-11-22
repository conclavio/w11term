[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$profiles = Join-Path $State 'profiles.json'
$catalog = [IO.File]::ReadAllText($profiles) | ConvertFrom-Json
$other = $catalog.profiles.'palette-ssh' | ConvertTo-Json -Depth 16 | ConvertFrom-Json
$other.user = 'other-account'
$catalog.profiles | Add-Member NoteProperty 'other-bookmark-profile' $other
[IO.File]::WriteAllText($profiles, ($catalog | ConvertTo-Json -Depth 16), [Text.UTF8Encoding]::new($false))
$bookmark = "/Pilot's Work"
$bookmarks = 'Bookmarks' + [char]0x2026
$openBookmark = "Open bookmark $bookmark"
$removeBookmark = "Remove bookmark $bookmark"
$button = [System.Windows.Automation.ControlType]::Button
$text = [System.Windows.Automation.ControlType]::Text

function Click([string]$Label) {
    Invoke-Button (Wait-Until "enabled '$Label'" {
        $element = Find-Element $Label $button
        if ($element -and $element.Current.IsEnabled) { $element }
    })
}
function Listing([string]$Path, [string]$Entry) {
    $null = Wait-Until "listing $Path containing '$Entry'" {
        (Find-Element $Path $text) -and (Find-Element $Entry $button)
    }
}
function Enter-Folder([string]$Label) {
    Click ($Label + '/')
    Click 'Open folder'
}
function Bookmark-Panel {
    Click $bookmarks
    $null = Wait-Until 'bookmark panel' { Find-Element 'Back to files' $button }
}
function Assert-NoBookmark {
    if ((Find-Element $openBookmark $button) -or (Find-Element $removeBookmark $button)) {
        throw "Unexpected bookmark for '$bookmark'"
    }
}
function Wait-OpenError {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($window)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty, $text)
    $null = Wait-Until 'remote error for the missing bookmark' {
        foreach ($entry in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)) {
            $message = $entry.Current.Name
            if ($message.StartsWith('Could not open bookmark') -and $message.Contains($bookmark)) { return $true }
        }
    }
}

for ($launch = 1; $launch -le 2; $launch++) {
    $app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State "gui-$launch.log")
    try {
        $window = Wait-InputWindow
        $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Join-Path $env:LOCALAPPDATA "w11term\instances\$($app.Id).json") }
        $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
        $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','ready')
        $generation = ((Control @('list')).sessions | Where-Object id -eq $pane).connection_generation
        Run-Palette 'SFTP files'
        Listing '/' 'stable/'
        if ($launch -eq 1) {
            Enter-Folder "Pilot's Work"
            Listing $bookmark 'bookmark-witness.txt'
            Bookmark-Panel
            Assert-NoBookmark
            Click 'Bookmark this folder'
            $null = Wait-Until 'new bookmark displays both operations' {
                (Find-Element $openBookmark $button) -and (Find-Element $removeBookmark $button)
            }
            Click 'Back to files'
            Enter-Folder 'deeper'
            Listing "$bookmark/deeper" 'child-witness.txt'
            Bookmark-Panel
            Click $openBookmark
            Listing $bookmark 'bookmark-witness.txt'

            $otherPane = (Control @('open','--profile','other-bookmark-profile','--activate')).pane
            Wait-Focus $otherPane
            $null = Control @('wait-for-text','--pane',"$otherPane",'--timeout','15','ready')
            Run-Palette 'SFTP files'
            Listing '/' 'stable/'
            Bookmark-Panel
            Assert-NoBookmark
        } else {
            Enter-Folder 'stable'
            Listing '/stable' 'stable-witness.txt'
            Click 'stable-witness.txt'
            $null = Control @('sftp','--pane',"$pane",'rename',$bookmark,'/temporarily-away')
            Bookmark-Panel
            Click $openBookmark
            Wait-OpenError
            Listing '/stable' 'stable-witness.txt'
            $download = 'Download' + [char]0x2026
            if (!(Find-Element $download $button).Current.IsEnabled) { throw 'Failed bookmark discarded the previous file selection' }

            $null = Control @('sftp','--pane',"$pane",'rename','/temporarily-away',$bookmark)
            Bookmark-Panel
            Click $openBookmark
            Listing $bookmark 'bookmark-witness.txt'
            $current = (Control @('list')).sessions | Where-Object id -eq $pane
            if (!$current.active -or $current.connection_generation -ne $generation) { throw 'Opening a saved bookmark replaced the SSH session' }
            Bookmark-Panel
            Click $removeBookmark
            $null = Wait-Until 'removed bookmark absent' {
                (Find-Element 'Back to files' $button) -and !(Find-Element $openBookmark $button) -and !(Find-Element $removeBookmark $button)
            }
            Click 'Back to files'
            Listing $bookmark 'bookmark-witness.txt'
        }
    } catch {
        $details = [IO.File]::ReadAllText((Join-Path $State "gui-$launch.log"))
        throw "$_ (launch $launch) at $($_.ScriptStackTrace). GUI: $details"
    } finally {
        if (!$app.HasExited) {
            $null = $app.CloseMainWindow()
            if (!$app.WaitForExit(5000)) { $app.Kill(); $app.WaitForExit() }
        }
        $app.Dispose()
    }
}
