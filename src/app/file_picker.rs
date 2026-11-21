use anyhow::{Context, Result, anyhow};
use std::{cell::RefCell, ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf, rc::Rc};
use windows::{
    Win32::{
        Foundation::{ERROR_CANCELLED, HWND},
        System::Com::*,
        UI::Shell::*,
    },
    core::{self as wincore, HRESULT, PCWSTR, Ref, implement, w},
};

pub(super) enum Purpose {
    Open,
    Folder,
    Save(String),
    Scrollback,
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
    // The modal dialog needs its own STA independently of the Reactor UI and worker threads.
    std::thread::Builder::new()
        .name("w11term file picker".into())
        .spawn(move || show(owner, purpose))
        .context("Starting file picker")?
        .join()
        .map_err(|_| anyhow!("File picker stopped"))?
}

fn show(owner: usize, purpose: Purpose) -> Result<Option<Selection>> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        let _apartment = Apartment;
        let clsid = if matches!(purpose, Purpose::Open | Purpose::Folder) {
            FileOpenDialog
        } else {
            FileSaveDialog
        };
        let dialog: IFileDialog = CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)?;
        let mut options = dialog.GetOptions()? | FOS_FORCEFILESYSTEM | FOS_NOCHANGEDIR;
        let confirmed = Rc::new(RefCell::new(None));
        let cookie = match purpose {
            Purpose::Open => None,
            Purpose::Folder => {
                dialog.SetTitle(w!("Choose a folder to share"))?;
                options |= FOS_PICKFOLDERS | FOS_PATHMUSTEXIST | FOS_DONTADDTORECENT;
                #[cfg(feature = "ui-smoke")]
                fixture_picker_folder(&dialog, "W11TERM_FOLDER_SHARE_SMOKE_DIR")?;
                None
            }
            Purpose::Save(suggestion) => {
                let suggested: Vec<u16> = suggestion.encode_utf16().chain(Some(0)).collect();
                dialog.SetFileName(PCWSTR(suggested.as_ptr()))?;
                None
            }
            Purpose::Scrollback => {
                #[cfg(feature = "ui-smoke")]
                if fixture_picker_folder(&dialog, "W11TERM_EXPORT_SMOKE_DIR")? {
                    options |= FOS_DONTADDTORECENT;
                }
                dialog.SetTitle(w!("Save scrollback as text"))?;
                dialog.SetFileName(w!("terminal.txt"))?;
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
                return Ok(None);
            }
            result => result?,
        }
        let path = shell_path(&dialog.GetResult()?)?;
        let overwrite = confirmed.borrow().as_ref() == Some(&path);
        Ok(Some(Selection { path, overwrite }))
    }
}
