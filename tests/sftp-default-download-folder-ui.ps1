[CmdletBinding()]
param([Parameter(Mandatory)][string]$Gui, [Parameter(Mandatory)][string]$Cli,
      [Parameter(Mandatory)][string]$State)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'support/native-ui.ps1')
Add-Type -Path (Join-Path $PSScriptRoot 'support/save-dialog.cs')
$env:W11TERM_DATA_DIR = Join-Path $State 'data'
$env:W11TERM_SETTINGS_PATH = Join-Path $State 'appearance.json'
$env:W11TERM_SFTP_PICKER_SMOKE_DIR = Join-Path $State 'picker'
$profiles = Join-Path $State 'profiles.json'
$preferred = Join-Path $env:W11TERM_SFTP_PICKER_SMOKE_DIR 'preferred downloads'
$button = [System.Windows.Automation.ControlType]::Button
$ellipsis = [char]0x2026

function Click([string]$Name) {
    Invoke-Button (Wait-Until "enabled '$Name'" {
        $item = Find-Element $Name $button -Visible
        if ($item -and $item.Current.IsEnabled) { $item }
    })
}

function Invoke-DownloadFolderSetting([string]$Name) {
    Invoke-Button (Get-NativeMenuAction 'More folder actions' $Name -Submenu 'Default download folder')
}

function Open-DownloadDialog([string]$Entry, [string]$Action, [string]$Title, [string]$Marker) {
    Click $Entry
    Click $Action
    $dialog = Wait-Until "owned '$Title' dialog" { Find-OwnedDialog $Title }
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($dialog)
    $condition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::NameProperty, $Marker)
    $null = Wait-Until "'$Marker' in the starting folder" {
        $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
    }
    return $dialog
}

function Cancel-DownloadDialog([IntPtr]$Dialog, [string]$Title) {
    $null = Wait-Until 'cancel the destination picker' { [FixtureDialog]::ClickButton($Dialog, 2) }
    $null = Wait-Until 'the destination picker closes' { !(Find-OwnedDialog $Title) }
}

$app = Start-Process -FilePath $Gui -ArgumentList @('--profiles',('"' + $profiles + '"'),'--profile','palette-ssh') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $State 'gui.log')
try {
    $window = Wait-InputWindow
    $null = Wait-Until 'control pipe' { Test-Path -LiteralPath (Get-ControlManifestPath $app.Id) }
    $pane = ((Control @('list')).sessions | Where-Object label -eq 'palette-ssh').id
    $null = Control @('wait-for-text','--pane',"$pane",'--timeout','15','src/trace.rs')
    Run-Palette 'Open files here'
    $null = Wait-Until 'remote directory contents' { Find-Element 'payload.bin' $button -Visible }

    Invoke-DownloadFolderSetting "Choose default download folder$ellipsis"
    $dialog = Wait-Until 'owned default download folder picker' {
        Find-OwnedDialog 'Choose the default SFTP download folder'
    }
    Select-DialogItem $dialog 'preferred downloads'
    $null = Wait-Until 'confirm the preferred folder' { [FixtureDialog]::ClickButton($dialog, 1) }
    $null = Wait-Until 'saved profile default' {
        $saved = (Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles.'palette-ssh'.sftp_download_folder
        $saved -and [FixtureDialog]::DirectoryPath($saved) -ieq [FixtureDialog]::DirectoryPath($preferred)
    }

    $dialog = Open-DownloadDialog 'payload.bin' "Download$ellipsis" 'Save As' 'preferred marker'
    Cancel-DownloadDialog $dialog 'Save As'
    if (Test-Path -LiteralPath (Join-Path $preferred 'payload.bin')) { throw 'Cancelled download created a file' }

    $dialog = Open-DownloadDialog 'snapshot/' "Download folder$ellipsis" 'Choose a download destination folder' 'preferred marker'
    Cancel-DownloadDialog $dialog 'Choose a download destination folder'

    $dialog = Open-DownloadDialog 'payload.bin' "Download$ellipsis" 'Save As' 'preferred marker'
    $null = Wait-Until 'save in the preferred folder' { [FixtureDialog]::ClickButton($dialog, 1) }
    $savedFile = Join-Path $preferred 'payload.bin'
    $null = Wait-Until 'downloaded file in the preferred folder' { Test-Path -LiteralPath $savedFile }

    Invoke-DownloadFolderSetting 'Use Windows default folder'
    $null = Wait-Until 'cleared profile default' {
        !(Get-Content -LiteralPath $profiles -Raw | ConvertFrom-Json).profiles.'palette-ssh'.sftp_download_folder
    }
    $dialog = Open-DownloadDialog 'payload.bin' "Download$ellipsis" 'Save As' 'usual marker'
    Cancel-DownloadDialog $dialog 'Save As'
} finally {
    Close-FixtureApp $app
}
