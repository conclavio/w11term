use anyhow::{Context, Result, anyhow};
use std::{
    cell::RefCell,
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    rc::Rc,
};
use windows::{
    Win32::{
        Foundation::{ERROR_CANCELLED, HWND},
        Storage::FileSystem::GetDriveTypeW,
        System::Com::*,
        UI::Shell::*,
    },
    core::{self as wincore, HRESULT, Interface, PCWSTR, Ref, implement, w},
};

pub(super) enum Purpose {
    UploadFiles,
    UploadFolder,
    CompareLocalFile,
    CompareLocalFolder,
    SshConfig(PathBuf),
    DotfilesBundle(PathBuf),
    ToolBundle(PathBuf),
    ToolExecutable(PathBuf),
    SaveToolBundle {
        current: PathBuf,
        suggestion: String,
    },
    OpenSshHostList,
    OpenSshGroupDefinition,
    SshIdentity {
        current: Option<PathBuf>,
        certificate: bool,
    },
    Folder,
    DefaultDownloadFolder,
    DownloadFolder,
    Save(String),
    SaveSearchResults,
    Scrollback,
    PaneImage,
    CommandOutput(String),
    CommandOutputDiff(String),
    WindowCommandHistory,
    GroupReport(String),
    SshLoginReport(String),
    GroupJsonReport(String),
    GroupRunComparison(String),
    FolderComparison,
    SftpTransferHistory,
    GroupOutputExcerpt(String),
    GroupOutputDiff(String),
    GroupLiveRecording(String),
    SshHostList,
    SshGroupDefinition,
    StartRecording,
    OpenRecording,
    OpenTriageReport(PathBuf),
    CompareTriageReport(PathBuf),
    SaveTriageReport(PathBuf),
    SaveCpuProfile(PathBuf),
    OpenPacketCapture(PathBuf),
    SavePacketCapture(PathBuf),
    WiresharkExecutable(PathBuf),
    OpenSyscallTrace(PathBuf),
    SaveSyscallTrace(PathBuf),
    SyscallComparison,
    CpuComparison,
    TriageComparison,
    FlameGraph {
        path: PathBuf,
        export: bool,
    },
}

fn is_local_folder(folder: &Path) -> bool {
    use std::path::{Component, Prefix};
    if !folder.is_absolute() {
        return false;
    }
    let Some(Component::Prefix(prefix)) = folder.components().next() else {
        return false;
    };
    let (Prefix::Disk(letter) | Prefix::VerbatimDisk(letter)) = prefix.kind() else {
        return false;
    };
    let drive = [u16::from(letter), b':' as u16, b'\\' as u16, 0];
    // A typed UNC path or disconnected mapped drive is not a starting folder.
    (unsafe { matches!(GetDriveTypeW(PCWSTR(drive.as_ptr())), 2 | 3) }) && folder.is_dir()
}

fn local_folder(dialog: &IFileDialog, folder: &Path) -> bool {
    if !is_local_folder(folder) {
        return false;
    }
    unsafe {
        let wide: Vec<_> = folder.as_os_str().encode_wide().chain(Some(0)).collect();
        // The proposed parent (such as ~/.ssh) may not exist yet. In that case
        // leave the file dialog's normal starting directory available.
        SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wide.as_ptr()), None)
            .and_then(|item| dialog.SetFolder(&item))
            .is_ok()
    }
}

pub(super) struct Selection {
    pub path: PathBuf,
    pub overwrite: bool,
}

fn shell_path(item: &IShellItem) -> wincore::Result<PathBuf> {
    unsafe {
        let wide = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = OsString::from_wide(wide.as_wide());
        CoTaskMemFree(Some(wide.0.cast()));
        Ok(path.into())
    }
}

#[cfg(feature = "ui-smoke")]
fn fixture_picker_folder(dialog: &IFileDialog, variable: &str) -> wincore::Result<bool> {
    use std::os::windows::ffi::OsStrExt;
    let Some(folder) = std::env::var_os(variable) else {
        return Ok(false);
    };
    let path: Vec<_> = folder.encode_wide().chain(Some(0)).collect();
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None)?;
        dialog.SetFolder(&item)?;
    }
    Ok(true)
}

// FOS_OVERWRITEPROMPT lets the shell ask for confirmation. Remember the destination it
// asked about so a new file appearing after Save can't turn into an implicit overwrite.
#[implement(IFileDialogEvents)]
struct Overwrite {
    path: Rc<RefCell<Option<PathBuf>>>,
}

impl IFileDialogEvents_Impl for Overwrite_Impl {
    fn OnOverwrite(
        &self,
        _: Ref<IFileDialog>,
        item: Ref<IShellItem>,
    ) -> wincore::Result<FDE_OVERWRITE_RESPONSE> {
        *self.path.borrow_mut() = Some(shell_path(item.ok()?)?);
        Ok(FDEOR_DEFAULT)
    }
    fn OnFileOk(&self, _: Ref<IFileDialog>) -> wincore::Result<()> {
        Ok(())
    }
    fn OnFolderChanging(&self, _: Ref<IFileDialog>, _: Ref<IShellItem>) -> wincore::Result<()> {
        Ok(())
    }
    fn OnFolderChange(&self, _: Ref<IFileDialog>) -> wincore::Result<()> {
        Ok(())
    }
    fn OnSelectionChange(&self, _: Ref<IFileDialog>) -> wincore::Result<()> {
        Ok(())
    }
    fn OnTypeChange(&self, _: Ref<IFileDialog>) -> wincore::Result<()> {
        Ok(())
    }
    fn OnShareViolation(
        &self,
        _: Ref<IFileDialog>,
        _: Ref<IShellItem>,
    ) -> wincore::Result<FDE_SHAREVIOLATION_RESPONSE> {
        Ok(FDESVR_DEFAULT)
    }
}

pub(super) fn pick(owner: usize, purpose: Purpose) -> Result<Option<Selection>> {
    let mut selected = pick_items(owner, purpose, None)?;
    anyhow::ensure!(selected.len() <= 1, "Expected a single selection");
    Ok(selected.pop())
}

pub(super) fn pick_paths(owner: usize, purpose: Purpose) -> Result<Vec<PathBuf>> {
    pick_paths_from_folder(owner, purpose, None)
}

pub(super) fn pick_paths_from_folder(
    owner: usize,
    purpose: Purpose,
    initial_folder: Option<PathBuf>,
) -> Result<Vec<PathBuf>> {
    Ok(pick_items(owner, purpose, initial_folder)?
        .into_iter()
        .map(|item| item.path)
        .collect())
}

fn pick_items(
    owner: usize,
    purpose: Purpose,
    initial_folder: Option<PathBuf>,
) -> Result<Vec<Selection>> {
    // The modal dialog needs its own STA independently of the Reactor UI and worker threads.
    std::thread::Builder::new()
        .name("w11term file picker".into())
        .spawn(move || show(owner, purpose, initial_folder))
        .context("Starting file picker")?
        .join()
        .map_err(|_| anyhow!("File picker stopped"))?
}

fn show(owner: usize, purpose: Purpose, initial_folder: Option<PathBuf>) -> Result<Vec<Selection>> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        let _apartment = Apartment;
        let clsid = if matches!(
            purpose,
            Purpose::UploadFiles
                | Purpose::UploadFolder
                | Purpose::CompareLocalFile
                | Purpose::CompareLocalFolder
                | Purpose::OpenRecording
                | Purpose::OpenTriageReport(_)
                | Purpose::CompareTriageReport(_)
                | Purpose::OpenPacketCapture(_)
                | Purpose::WiresharkExecutable(_)
                | Purpose::OpenSyscallTrace(_)
                | Purpose::FlameGraph { export: false, .. }
                | Purpose::SshConfig(_)
                | Purpose::DotfilesBundle(_)
                | Purpose::ToolBundle(_)
                | Purpose::ToolExecutable(_)
                | Purpose::OpenSshHostList
                | Purpose::OpenSshGroupDefinition
                | Purpose::SshIdentity { .. }
                | Purpose::Folder
                | Purpose::DefaultDownloadFolder
                | Purpose::DownloadFolder
        ) {
            FileOpenDialog
        } else {
            FileSaveDialog
        };
        let dialog: IFileDialog = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)?;
        let mut options = dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR;
        let confirmed = Rc::new(RefCell::new(None));
        let multiple = matches!(purpose, Purpose::UploadFiles | Purpose::OpenSyscallTrace(_));
        let setting_download_folder = matches!(purpose, Purpose::DefaultDownloadFolder);
        let cookie = match purpose {
            Purpose::SaveSyscallTrace(path) => {
                use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
                dialog.SetTitle(w!("Save syscall trace"))?;
                dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
                    pszName: w!("Syscall trace"),
                    pszSpec: w!("*.strace"),
                }])?;
                dialog.SetDefaultExtension(w!("strace"))?;
                dialog.SetFileName(w!("syscall-trace.strace"))?;
                options |= FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT | FOS_NOTESTFILECREATE;
                options &= !FOS_OVERWRITEPROMPT;
                if let Some(folder) = path.parent() {
                    local_folder(&dialog, folder);
                }
                None
            }
            Purpose::OpenSyscallTrace(path) => {
                dialog.SetTitle(w!("Open syscall trace"))?;
                options |= FOS_FILEMUSTEXIST
                    | FOS_PATHMUSTEXIST
                    | FOS_DONTADDTORECENT
                    | FOS_ALLOWMULTISELECT;
                if let Some(folder) = path.parent() {
                    local_folder(&dialog, folder);
                }
                None
            }
            Purpose::OpenPacketCapture(path) => {
                use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
                dialog.SetTitle(w!("Open packet capture"))?;
                dialog.SetFileTypes(&[
                    COMDLG_FILTERSPEC {
                        pszName: w!("Packet captures"),
                        pszSpec: w!("*.pcap;*.pcapng;*.cap"),
                    },
                    COMDLG_FILTERSPEC {
                        pszName: w!("All files"),
                        pszSpec: w!("*.*"),
                    },
                ])?;
                options |= FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                if let Some(folder) = path.parent() {
                    local_folder(&dialog, folder);
                }
                None
            }
            Purpose::SavePacketCapture(path) | Purpose::WiresharkExecutable(path) => {
                use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
                options |= FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                if clsid == FileSaveDialog {
                    dialog.SetTitle(w!("Save packet capture"))?;
                    dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
                        pszName: w!("Packet capture"),
                        pszSpec: w!("*.pcap"),
                    }])?;
                    dialog.SetDefaultExtension(w!("pcap"))?;
                    dialog.SetFileName(w!("packet-capture.pcap"))?;
                    options |= FOS_NOTESTFILECREATE;
                    options &= !FOS_OVERWRITEPROMPT;
                } else {
                    dialog.SetTitle(w!("Choose Wireshark executable"))?;
                    dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
                        pszName: w!("Windows applications"),
                        pszSpec: w!("*.exe"),
                    }])?;
                    options |= FOS_FILEMUSTEXIST;
                }
                if let Some(folder) = path.parent() {
                    local_folder(&dialog, folder);
                }
                None
            }
            Purpose::SaveCpuProfile(path) => {
                use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
                dialog.SetTitle(w!("Save CPU profile"))?;
                dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
                    pszName: w!("Folded stacks"),
                    pszSpec: w!("*.folded"),
                }])?;
                dialog.SetDefaultExtension(w!("folded"))?;
                dialog.SetFileName(w!("cpu-profile.folded"))?;
                options |= FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT | FOS_NOTESTFILECREATE;
                options &= !FOS_OVERWRITEPROMPT;
                if let Some(folder) = path.parent() {
                    local_folder(&dialog, folder);
                }
                None
            }
            Purpose::FlameGraph { path, export } => {
                use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
                options |= FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                if export {
                    dialog.SetTitle(w!("Export CPU flame graph"))?;
                    dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
                        pszName: w!("Scalable Vector Graphics"),
                        pszSpec: w!("*.svg"),
                    }])?;
                    dialog.SetDefaultExtension(w!("svg"))?;
                    dialog.SetFileName(w!("cpu-flame-graph.svg"))?;
                    options |= FOS_NOTESTFILECREATE;
                    options &= !FOS_OVERWRITEPROMPT;
                } else {
                    dialog.SetTitle(w!("Open folded stacks"))?;
                    dialog.SetFileTypes(&[
                        COMDLG_FILTERSPEC {
                            pszName: w!("Folded stacks"),
                            pszSpec: w!("*.folded;*.collapsed"),
                        },
                        COMDLG_FILTERSPEC {
                            pszName: w!("All files"),
                            pszSpec: w!("*.*"),
                        },
                    ])?;
                    options |= FOS_FILEMUSTEXIST;
                }
                if let Some(folder) = path.parent() {
                    local_folder(&dialog, folder);
                }
                None
            }
            Purpose::OpenTriageReport(ref current)
            | Purpose::CompareTriageReport(ref current)
            | Purpose::SaveTriageReport(ref current) => {
                use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
                dialog.SetFileTypes(&[COMDLG_FILTERSPEC {
                    pszName: w!("Linux triage reports"),
                    pszSpec: w!("*.json"),
                }])?;
                dialog.SetDefaultExtension(w!("json"))?;
                options |= FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                if matches!(
                    &purpose,
                    Purpose::OpenTriageReport(_) | Purpose::CompareTriageReport(_)
                ) {
                    dialog.SetTitle(if matches!(&purpose, Purpose::CompareTriageReport(_)) {
                        w!("Choose after triage report")
                    } else {
                        w!("Open Linux triage report")
                    })?;
                    options |= FOS_FILEMUSTEXIST;
                } else {
                    dialog.SetTitle(w!("Save Linux triage report"))?;
                    dialog.SetFileName(w!("linux-triage.json"))?;
                    options &= !FOS_OVERWRITEPROMPT;
                    options |= FOS_NOTESTFILECREATE;
                }
                if let Some(folder) = current.parent() {
                    local_folder(&dialog, folder);
                }
                None
            }
            Purpose::StartRecording | Purpose::OpenRecording => {
                use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
                dialog.SetFileTypes(&[
                    COMDLG_FILTERSPEC {
                        pszName: w!("Terminal recordings"),
                        pszSpec: w!("*.vt"),
                    },
                    COMDLG_FILTERSPEC {
                        pszName: w!("All files"),
                        pszSpec: w!("*.*"),
                    },
                ])?;
                dialog.SetDefaultExtension(w!("vt"))?;
                options |= FOS_PATHMUSTEXIST;
                if matches!(purpose, Purpose::StartRecording) {
                    dialog.SetTitle(w!("Start timed recording"))?;
                    dialog.SetFileName(w!("session.vt"))?;
                    // The recorder creates the capture and its sidecars exclusively.
                    options &= !FOS_OVERWRITEPROMPT;
                    options |= FOS_NOTESTFILECREATE;
                } else {
                    dialog.SetTitle(w!("Open recording"))?;
                    options |= FOS_FILEMUSTEXIST;
                }
                #[cfg(feature = "ui-smoke")]
                if fixture_picker_folder(&dialog, "W11TERM_RECORDING_SMOKE_DIR")? {
                    options |= FOS_DONTADDTORECENT;
                }
                None
            }
            Purpose::UploadFiles => {
                dialog.SetTitle(w!("Choose files to upload"))?;
                options |= FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_ALLOWMULTISELECT;
                #[cfg(feature = "ui-smoke")]
                if fixture_picker_folder(&dialog, "W11TERM_SFTP_PICKER_SMOKE_DIR")? {
                    options |= FOS_DONTADDTORECENT;
                }
                None
            }
            Purpose::CompareLocalFile => {
                dialog.SetTitle(w!("Choose a local file to compare"))?;
                options |= FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                #[cfg(feature = "ui-smoke")]
                fixture_picker_folder(&dialog, "W11TERM_SFTP_PICKER_SMOKE_DIR")?;
                None
            }
            Purpose::SshConfig(current) => {
                dialog.SetTitle(w!("Open OpenSSH config"))?;
                options |= FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                // Leave file types unrestricted: OpenSSH usually names this file "config".
                if let Some(folder) = current.parent() {
                    local_folder(&dialog, folder);
                }
                None
            }
            Purpose::DotfilesBundle(ref current) | Purpose::ToolBundle(ref current) => {
                dialog.SetTitle(if matches!(&purpose, Purpose::ToolBundle(_)) {
                    w!("Choose SSH tool bundle")
                } else {
                    w!("Choose dotfiles bundle")
                })?;
                options |= FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                if let Some(folder) = current.parent() {
                    local_folder(&dialog, folder);
                }
                None
            }
            Purpose::ToolExecutable(current) => {
                dialog.SetTitle(w!("Choose a Linux executable"))?;
                options |= FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                if let Some(folder) = current.parent() {
                    local_folder(&dialog, folder);
                }
                None
            }
            Purpose::SaveToolBundle {
                current,
                suggestion,
            } => {
                dialog.SetTitle(w!("Save new SSH tool bundle"))?;
                let wide: Vec<_> = suggestion.encode_utf16().chain(Some(0)).collect();
                dialog.SetFileName(PCWSTR(wide.as_ptr()))?;
                dialog.SetDefaultExtension(w!("json"))?;
                options &= !FOS_OVERWRITEPROMPT;
                options |= FOS_NOTESTFILECREATE | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                if let Some(folder) = current.parent() {
                    local_folder(&dialog, folder);
                }
                None
            }
            Purpose::OpenSshHostList => {
                dialog.SetTitle(w!("Open SSH host list"))?;
                options |= FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                #[cfg(feature = "ui-smoke")]
                fixture_picker_folder(&dialog, "W11TERM_HOST_LIST_SMOKE_DIR")?;
                None
            }
            Purpose::OpenSshGroupDefinition => {
                dialog.SetTitle(w!("Open SSH group definition"))?;
                options |= FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                #[cfg(feature = "ui-smoke")]
                fixture_picker_folder(&dialog, "W11TERM_EXPORT_SMOKE_DIR")?;
                None
            }
            Purpose::SshIdentity {
                current,
                certificate,
            } => {
                dialog.SetTitle(if certificate {
                    w!("Choose SSH user certificate")
                } else {
                    w!("Choose SSH private key")
                })?;
                options |= FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                // OpenSSH private keys often have no extension.
                let seeded = current
                    .as_deref()
                    .and_then(Path::parent)
                    .is_some_and(|folder| local_folder(&dialog, folder));
                if !seeded && let Some(home) = std::env::home_dir() {
                    local_folder(&dialog, &home.join(".ssh"));
                }
                None
            }
            Purpose::Folder
            | Purpose::DefaultDownloadFolder
            | Purpose::DownloadFolder
            | Purpose::UploadFolder
            | Purpose::CompareLocalFolder => {
                dialog.SetTitle(match purpose {
                    Purpose::DefaultDownloadFolder => w!("Choose the default SFTP download folder"),
                    Purpose::DownloadFolder => w!("Choose a download destination folder"),
                    Purpose::UploadFolder => w!("Choose a folder to upload"),
                    Purpose::CompareLocalFolder => w!("Choose a Windows folder for comparison"),
                    _ => w!("Choose a folder to share"),
                })?;
                options |= FOS_PICKFOLDERS | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                #[cfg(feature = "ui-smoke")]
                fixture_picker_folder(
                    &dialog,
                    if matches!(purpose, Purpose::Folder) {
                        "W11TERM_FOLDER_SHARE_SMOKE_DIR"
                    } else {
                        "W11TERM_SFTP_PICKER_SMOKE_DIR"
                    },
                )?;
                None
            }
            Purpose::Save(suggestion) => {
                #[cfg(feature = "ui-smoke")]
                if fixture_picker_folder(&dialog, "W11TERM_SFTP_PICKER_SMOKE_DIR")? {
                    options |= FOS_DONTADDTORECENT;
                }
                let suggested: Vec<u16> = suggestion.encode_utf16().chain(Some(0)).collect();
                dialog.SetFileName(PCWSTR(suggested.as_ptr()))?;
                None
            }
            Purpose::SaveSearchResults => {
                #[cfg(feature = "ui-smoke")]
                fixture_picker_folder(&dialog, "W11TERM_SFTP_PICKER_SMOKE_DIR")?;
                dialog.SetTitle(w!("Download marked search results"))?;
                dialog.SetFileNameLabel(w!("New folder name:"))?;
                dialog.SetOkButtonLabel(w!("Download"))?;
                dialog.SetFileName(w!("Search results"))?;
                options &= !FOS_OVERWRITEPROMPT;
                options |= FOS_PATHMUSTEXIST | FOS_NOTESTFILECREATE | FOS_DONTADDTORECENT;
                None
            }
            Purpose::GroupLiveRecording(suggestion) => {
                #[cfg(feature = "ui-smoke")]
                if fixture_picker_folder(&dialog, "W11TERM_EXPORT_SMOKE_DIR")? {
                    options |= FOS_DONTADDTORECENT;
                }
                dialog.SetTitle(w!("Record SSH group output"))?;
                let wide: Vec<u16> = suggestion.encode_utf16().chain(Some(0)).collect();
                dialog.SetFileName(PCWSTR(wide.as_ptr()))?;
                dialog.SetDefaultExtension(w!("txt"))?;
                options &= !FOS_OVERWRITEPROMPT;
                options |= FOS_PATHMUSTEXIST | FOS_NOTESTFILECREATE;
                None
            }
            Purpose::Scrollback
            | Purpose::PaneImage
            | Purpose::CommandOutput(_)
            | Purpose::CommandOutputDiff(_)
            | Purpose::WindowCommandHistory
            | Purpose::GroupReport(_)
            | Purpose::SshLoginReport(_)
            | Purpose::GroupJsonReport(_)
            | Purpose::GroupRunComparison(_)
            | Purpose::FolderComparison
            | Purpose::SyscallComparison
            | Purpose::CpuComparison
            | Purpose::TriageComparison
            | Purpose::SftpTransferHistory
            | Purpose::GroupOutputExcerpt(_)
            | Purpose::GroupOutputDiff(_)
            | Purpose::SshGroupDefinition
            | Purpose::SshHostList => {
                #[cfg(feature = "ui-smoke")]
                if fixture_picker_folder(&dialog, "W11TERM_EXPORT_SMOKE_DIR")? {
                    options |= FOS_DONTADDTORECENT;
                }
                let (title, suggestion, extension) = match &purpose {
                    Purpose::PaneImage => (w!("Save pane image"), "terminal.png", w!("png")),
                    Purpose::CommandOutput(suggestion) => (
                        w!("Save command output as text"),
                        suggestion.as_str(),
                        w!("txt"),
                    ),
                    Purpose::CommandOutputDiff(suggestion) => (
                        w!("Save command output differences"),
                        suggestion.as_str(),
                        w!("txt"),
                    ),
                    Purpose::WindowCommandHistory => (
                        w!("Save command history results as JSON"),
                        "w11term-command-history.json",
                        w!("json"),
                    ),
                    Purpose::GroupReport(suggestion) => (
                        w!("Save SSH group results as text"),
                        suggestion.as_str(),
                        w!("txt"),
                    ),
                    Purpose::SshLoginReport(suggestion) => {
                        (w!("Save SSH login report"), suggestion.as_str(), w!("txt"))
                    }
                    Purpose::GroupJsonReport(suggestion) => (
                        w!("Save SSH group results as JSON"),
                        suggestion.as_str(),
                        w!("json"),
                    ),
                    Purpose::GroupRunComparison(suggestion) => (
                        w!("Save SSH group run comparison as JSON"),
                        suggestion.as_str(),
                        w!("json"),
                    ),
                    Purpose::FolderComparison => (
                        w!("Save folder comparison as JSON"),
                        "sftp-folder-comparison.json",
                        w!("json"),
                    ),
                    Purpose::SyscallComparison => (
                        w!("Save syscall comparison"),
                        "syscall-comparison.txt",
                        w!("txt"),
                    ),
                    Purpose::CpuComparison => (
                        w!("Save CPU profile comparison"),
                        "cpu-profile-comparison.json",
                        w!("json"),
                    ),
                    Purpose::TriageComparison => (
                        w!("Save Linux triage comparison"),
                        "linux-triage-comparison.txt",
                        w!("txt"),
                    ),
                    Purpose::SftpTransferHistory => (
                        w!("Save recent SFTP transfers as JSON"),
                        "sftp-transfers.json",
                        w!("json"),
                    ),
                    Purpose::GroupOutputExcerpt(suggestion) => (
                        w!("Save shown SSH group output"),
                        suggestion.as_str(),
                        w!("txt"),
                    ),
                    Purpose::GroupOutputDiff(suggestion) => (
                        w!("Save SSH group differences"),
                        suggestion.as_str(),
                        w!("txt"),
                    ),
                    Purpose::SshHostList => (w!("Save SSH host list"), "ssh-hosts.txt", w!("txt")),
                    Purpose::SshGroupDefinition => (
                        w!("Save SSH group definition"),
                        "ssh-group.json",
                        w!("json"),
                    ),
                    _ => (w!("Save scrollback as text"), "terminal.txt", w!("txt")),
                };
                dialog.SetTitle(title)?;
                let wide: Vec<u16> = suggestion.encode_utf16().chain(Some(0)).collect();
                dialog.SetFileName(PCWSTR(wide.as_ptr()))?;
                dialog.SetDefaultExtension(extension)?;
                // The atomic writer creates its own sibling file and reports any write errors.
                options |= FOS_NOTESTFILECREATE;
                if matches!(
                    purpose,
                    Purpose::SyscallComparison | Purpose::CpuComparison | Purpose::TriageComparison
                ) {
                    options |= FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                    options &= !FOS_OVERWRITEPROMPT;
                    None
                } else {
                    options |= FOS_OVERWRITEPROMPT;
                    let sink: IFileDialogEvents = Overwrite {
                        path: confirmed.clone(),
                    }
                    .into();
                    Some(dialog.Advise(&sink)?)
                }
            }
        };
        dialog.SetOptions(options)?;
        if let Some(folder) = initial_folder {
            local_folder(&dialog, &folder);
        }
        let shown = dialog.Show(Some(HWND(owner as *mut _)));
        if let Some(cookie) = cookie {
            dialog.Unadvise(cookie)?;
        }
        match shown {
            Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => {
                return Ok(Vec::new());
            }
            result => result?,
        }
        if multiple {
            let items = dialog.cast::<IFileOpenDialog>()?.GetResults()?;
            return (0..items.GetCount()?)
                .map(|index| {
                    Ok(Selection {
                        path: shell_path(&items.GetItemAt(index)?)?,
                        overwrite: false,
                    })
                })
                .collect();
        }
        let path = shell_path(&dialog.GetResult()?)?;
        if setting_download_folder {
            anyhow::ensure!(
                is_local_folder(&path),
                "Choose a folder on a local Windows drive"
            );
        }
        let overwrite = confirmed.borrow().as_ref() == Some(&path);
        Ok(vec![Selection { path, overwrite }])
    }
}
