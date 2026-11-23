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
    UploadFile,
    UploadFiles,
    UploadFolder,
    SshConfig(PathBuf),
    SshIdentity(Option<PathBuf>),
    Folder,
    DownloadFolder,
    Save(String),
    Scrollback,
    CommandOutput(String),
    StartRecording,
    OpenRecording,
}

fn local_folder(dialog: &IFileDialog, folder: &Path) -> bool {
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
    unsafe {
        // A typed UNC path or disconnected mapped drive is not a starting folder.
        if !matches!(GetDriveTypeW(PCWSTR(drive.as_ptr())), 2 | 3) || !folder.is_dir() {
            return false;
        }
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
    let mut selected = pick_items(owner, purpose)?;
    anyhow::ensure!(selected.len() <= 1, "Expected a single selection");
    Ok(selected.pop())
}

pub(super) fn pick_paths(owner: usize, purpose: Purpose) -> Result<Vec<PathBuf>> {
    Ok(pick_items(owner, purpose)?
        .into_iter()
        .map(|item| item.path)
        .collect())
}

fn pick_items(owner: usize, purpose: Purpose) -> Result<Vec<Selection>> {
    // The modal dialog needs its own STA independently of the Reactor UI and worker threads.
    std::thread::Builder::new()
        .name("w11term file picker".into())
        .spawn(move || show(owner, purpose))
        .context("Starting file picker")?
        .join()
        .map_err(|_| anyhow!("File picker stopped"))?
}

fn show(owner: usize, purpose: Purpose) -> Result<Vec<Selection>> {
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
            Purpose::UploadFile
                | Purpose::UploadFiles
                | Purpose::UploadFolder
                | Purpose::OpenRecording
                | Purpose::SshConfig(_)
                | Purpose::SshIdentity(_)
                | Purpose::Folder
                | Purpose::DownloadFolder
        ) {
            FileOpenDialog
        } else {
            FileSaveDialog
        };
        let dialog: IFileDialog = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)?;
        let mut options = dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR;
        let confirmed = Rc::new(RefCell::new(None));
        let multiple = matches!(purpose, Purpose::UploadFiles);
        let cookie = match purpose {
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
            Purpose::UploadFile | Purpose::UploadFiles => {
                dialog.SetTitle(if multiple {
                    w!("Choose files to upload")
                } else {
                    w!("Choose a file to upload")
                })?;
                options |= FOS_FILEMUSTEXIST | FOS_PATHMUSTEXIST;
                if multiple {
                    options |= FOS_ALLOWMULTISELECT;
                }
                #[cfg(feature = "ui-smoke")]
                if fixture_picker_folder(&dialog, "W11TERM_SFTP_PICKER_SMOKE_DIR")? {
                    options |= FOS_DONTADDTORECENT;
                }
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
            Purpose::SshIdentity(current) => {
                dialog.SetTitle(w!("Choose SSH private key"))?;
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
            Purpose::Folder | Purpose::DownloadFolder | Purpose::UploadFolder => {
                dialog.SetTitle(match purpose {
                    Purpose::DownloadFolder => w!("Choose a download destination folder"),
                    Purpose::UploadFolder => w!("Choose a folder to upload"),
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
            Purpose::Scrollback | Purpose::CommandOutput(_) => {
                #[cfg(feature = "ui-smoke")]
                if fixture_picker_folder(&dialog, "W11TERM_EXPORT_SMOKE_DIR")? {
                    options |= FOS_DONTADDTORECENT;
                }
                let (title, suggestion) = match &purpose {
                    Purpose::CommandOutput(suggestion) => {
                        (w!("Save command output as text"), suggestion.as_str())
                    }
                    _ => (w!("Save scrollback as text"), "terminal.txt"),
                };
                dialog.SetTitle(title)?;
                let wide: Vec<u16> = suggestion.encode_utf16().chain(Some(0)).collect();
                dialog.SetFileName(PCWSTR(wide.as_ptr()))?;
                dialog.SetDefaultExtension(w!("txt"))?;
                // The atomic writer creates its own sibling file and reports any write errors.
                options |= FOS_OVERWRITEPROMPT | FOS_NOTESTFILECREATE;
                let sink: IFileDialogEvents = Overwrite {
                    path: confirmed.clone(),
                }
                .into();
                Some(dialog.Advise(&sink)?)
            }
        };
        dialog.SetOptions(options)?;
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
        let overwrite = confirmed.borrow().as_ref() == Some(&path);
        Ok(vec![Selection { path, overwrite }])
    }
}
