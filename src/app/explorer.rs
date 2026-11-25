use super::{ComponentContext, Message, TerminalApp};
use anyhow::{Context, Result, anyhow, ensure};
use std::{
    os::windows::ffi::OsStrExt,
    path::{Component, Path, PathBuf, Prefix},
};
use w11term::events::SavedDownload;
use windows::{
    Win32::{
        Storage::FileSystem::GetDriveTypeW,
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            SystemServices::{SFGAO_FILESYSTEM, SFGAO_FOLDER},
        },
        UI::{
            Shell::{
                Common::ITEMIDLIST, SEE_MASK_FLAG_NO_UI, SEE_MASK_IDLIST, SEE_MASK_NOASYNC,
                SHELLEXECUTEINFOW, SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteExW,
            },
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    },
    core::{PCWSTR, w},
};

pub(super) enum Location {
    Drive(PathBuf),
    Wsl(PathBuf),
}

impl Location {
    pub(super) fn wsl(distribution: &str, linux_path: &str) -> Option<Self> {
        let mut folder = wsl_root(distribution)?;
        let tail = linux_path.strip_prefix('/')?;
        if tail.starts_with('/') {
            return None;
        }
        let tail = tail.strip_suffix('/').unwrap_or(tail);
        if !tail.is_empty() {
            for component in tail.split('/') {
                w11term::sftp::validate_download_name(component).ok()?;
                folder.push(component);
            }
        }
        Some(Self::Wsl(folder))
    }

    pub(super) fn translated_from_wsl(distribution: &str, translated: &str) -> Option<Self> {
        let mut components = Path::new(translated).components();
        let Component::Prefix(prefix) = components.next()? else {
            return None;
        };
        match prefix.kind() {
            Prefix::Disk(_) | Prefix::VerbatimDisk(_) => {
                resolve(translated.strip_prefix(r"\\?\").unwrap_or(translated), None)
                    .map(Self::Drive)
            }
            Prefix::UNC(host, share) | Prefix::VerbatimUNC(host, share)
                if (host.eq_ignore_ascii_case("wsl.localhost")
                    || host.eq_ignore_ascii_case("wsl$"))
                    && share.eq_ignore_ascii_case(distribution) =>
            {
                let mut location = wsl_root(distribution)?;
                for component in components {
                    match component {
                        Component::RootDir => {}
                        Component::Normal(name) => {
                            w11term::sftp::validate_download_name(name.to_str()?).ok()?;
                            location.push(name);
                        }
                        _ => return None,
                    }
                }
                Some(Self::Wsl(location))
            }
            _ => None,
        }
    }

    pub(super) fn reveal(&self) -> Result<()> {
        let path = self.checked_path()?;
        check_item(path)?;
        shell_item(path, ShellAction::Select)
    }

    pub(super) fn open(&self) -> Result<()> {
        let path = self.checked_path()?;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("Cannot read the folder {}", path.display()))?;
        ensure!(
            metadata.is_dir(),
            "The path must identify a filesystem folder"
        );
        shell_item(path, ShellAction::Explore)
    }

    fn checked_path(&self) -> Result<&Path> {
        match self {
            Self::Drive(path) => {
                check_drive(path)?;
                Ok(path)
            }
            Self::Wsl(path) => Ok(path),
        }
    }
}

fn check_saved_path(path: &Path) -> Result<()> {
    // This path comes from the user's save picker. Unlike a path taken
    // from terminal output, it can name a network filesystem.
    ensure!(
        path.is_absolute()
            && matches!(
                path.components().next(),
                Some(Component::Prefix(prefix)) if matches!(
                    prefix.kind(),
                    Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(..) | Prefix::VerbatimUNC(..)
                )
            ),
        "Expected an absolute filesystem path"
    );
    Ok(())
}

pub(super) fn reveal_saved_file(path: &Path) -> Result<()> {
    check_saved_path(path)?;
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("Cannot find {}", path.display()))?;
    ensure!(
        metadata.is_file(),
        "The saved path no longer identifies a file"
    );
    shell_item(path, ShellAction::Select)
}

pub(super) fn download_label(saved: &SavedDownload) -> &'static str {
    match saved {
        SavedDownload::BatchFolder(_) => "Open download folder",
        SavedDownload::File(_) | SavedDownload::Folder(_) => "Show download in File Explorer",
    }
}

pub(super) fn show_download(saved: SavedDownload, context: &ComponentContext<TerminalApp>) {
    context.spawn_background_with_rejection(
        move |cancel| {
            let result = if cancel.is_cancelled() {
                Err("The Explorer request was cancelled.".into())
            } else {
                show_saved_download(&saved).map_err(|error| format!("{error:#}"))
            };
            Message::ExplorerOpened(result)
        },
        Message::ExplorerOpened(Err("The Explorer request could not start.".into())),
    );
}

fn show_saved_download(saved: &SavedDownload) -> Result<()> {
    let (path, action) = match saved {
        SavedDownload::File(path) => return reveal_saved_file(path),
        SavedDownload::Folder(path) => (path, ShellAction::Select),
        SavedDownload::BatchFolder(path) => (path, ShellAction::Explore),
    };
    check_saved_path(path)?;
    let info = match action {
        ShellAction::Select => std::fs::symlink_metadata(path),
        ShellAction::Explore => std::fs::metadata(path),
    }
    .with_context(|| format!("Cannot find {}", path.display()))?;
    ensure!(
        info.is_dir(),
        "The saved path no longer identifies a folder"
    );
    shell_item(path, action)
}

fn wsl_root(distribution: &str) -> Option<PathBuf> {
    w11term::sftp::validate_download_name(distribution).ok()?;
    Some(PathBuf::from(format!(r"\\wsl.localhost\{distribution}\")))
}

fn drive_path(text: &str) -> Option<(PathBuf, &str)> {
    let [letter, b':', b'\\' | b'/', ..] = text.as_bytes() else {
        return None;
    };
    letter.is_ascii_alphabetic().then(|| {
        (
            PathBuf::from(format!("{}:\\", char::from(*letter))),
            &text[3..],
        )
    })
}

fn append(path: &mut PathBuf, tail: &str) -> Option<()> {
    if !tail.is_empty() {
        let tail = tail.strip_suffix(['/', '\\']).unwrap_or(tail);
        for name in tail.split(['/', '\\']) {
            match name {
                "." => {}
                ".." => {
                    path.pop().then_some(())?;
                }
                _ => {
                    w11term::sftp::validate_download_name(name).ok()?;
                    path.push(name);
                }
            }
        }
    }
    Some(())
}

// The hint matcher has already removed quotes and diagnostic line numbers.
// Resolve against the captured pane directory without consulting process state or disk.
pub(super) fn resolve(hint: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let (mut path, tail) = if let Some(absolute) = drive_path(hint) {
        absolute
    } else {
        if hint.is_empty()
            || hint.starts_with(['/', '\\'])
            || hint.split(['/', '\\']).next() == Some("~")
        {
            return None;
        }
        let (mut base, tail) = drive_path(cwd?.to_str()?)?;
        append(&mut base, tail)?;
        (base, hint)
    };
    append(&mut path, tail)?;
    Some(path)
}

fn check_drive(path: &Path) -> Result<()> {
    let (root, _) = path
        .to_str()
        .and_then(drive_path)
        .context("Expected an absolute Windows drive path")?;
    let drive: Vec<_> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    // Removable, fixed, optical, or RAM disk. A mapped network drive also has a drive letter.
    ensure!(
        matches!(
            unsafe { GetDriveTypeW(PCWSTR(drive.as_ptr())) },
            2 | 3 | 5 | 6
        ),
        "The path must be on a local drive"
    );
    Ok(())
}

fn check_item(path: &Path) -> Result<()> {
    // Check the directory entry itself, so links retain their own names.
    std::fs::symlink_metadata(path).with_context(|| format!("Cannot find {}", path.display()))?;
    Ok(())
}

#[derive(Clone, Copy)]
enum ShellAction {
    Select,
    Explore,
}

fn explore(item: *mut ITEMIDLIST) -> windows::core::Result<()> {
    let mut request = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // This worker has no message loop and its STA ends when the call returns.
        fMask: SEE_MASK_IDLIST | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("explore"),
        lpIDList: item.cast(),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut request) }
}

fn shell_item(path: &Path, action: ShellAction) -> Result<()> {
    // The caller runs on the Windows thread pool; give the Shell its own STA.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("w11term Explorer".into())
            .spawn_scoped(scope, || {
                let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                unsafe {
                    CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
                    let mut item = std::ptr::null_mut();
                    let required = match action {
                        ShellAction::Select => 0,
                        ShellAction::Explore => (SFGAO_FILESYSTEM | SFGAO_FOLDER).0,
                    };
                    let mut attributes = 0;
                    let result = SHParseDisplayName(
                        PCWSTR(wide.as_ptr()),
                        None,
                        &mut item,
                        required,
                        Some(&mut attributes),
                    )
                    .context("Resolving the Explorer item")
                    .and_then(|()| {
                        ensure!(
                            !item.is_null() && attributes & required == required,
                            "The path must identify a filesystem folder"
                        );
                        let opened = match action {
                            // With zero children the PIDL identifies the item
                            // to select in its parent.
                            ShellAction::Select => SHOpenFolderAndSelectItems(item, None, 0),
                            ShellAction::Explore => explore(item),
                        };
                        #[cfg(feature = "ui-smoke")]
                        if let Err(error) = trace_action(item, action, &opened) {
                            log::warn!("Recording the Explorer action for the fixture: {error:#}");
                        }
                        opened.context(match action {
                            ShellAction::Select => "Selecting the item",
                            ShellAction::Explore => "Opening the folder",
                        })
                    });
                    CoTaskMemFree(Some(item.cast()));
                    CoUninitialize();
                    result.with_context(|| match action {
                        ShellAction::Select => {
                            format!("Selecting {} in File Explorer", path.display())
                        }
                        ShellAction::Explore => {
                            format!("Opening {} in File Explorer", path.display())
                        }
                    })
                }
            })
            .context("Starting File Explorer")?
            .join()
            .map_err(|_| anyhow!("File Explorer stopped"))?
    })
}

// A runner can have Shell32 without an observable Explorer desktop. Let the
// native fixture verify the item parsed by Shell32 and the selection call's result.
#[cfg(feature = "ui-smoke")]
fn trace_action(
    item: *const ITEMIDLIST,
    action: ShellAction,
    opened: &windows::core::Result<()>,
) -> Result<()> {
    use windows::Win32::UI::Shell::{SHGetNameFromIDList, SIGDN_FILESYSPATH};

    let Some(output) = std::env::var_os("W11TERM_TEST_EXPLORER_TRACE") else {
        return Ok(());
    };
    let path = unsafe {
        let wide = SHGetNameFromIDList(item, SIGDN_FILESYSPATH)?;
        let path = wide.to_string();
        CoTaskMemFree(Some(wide.0.cast()));
        path?
    };
    let hresult = opened.as_ref().err().map_or(0, |e| e.code().0);
    let action = match action {
        ShellAction::Select => "select",
        ShellAction::Explore => "explore",
    };
    let output = Path::new(&output);
    let file = tempfile::NamedTempFile::new_in(output.parent().context("Trace directory")?)?;
    serde_json::to_writer(
        file.as_file(),
        &serde_json::json!({ "path": path, "hresult": hresult, "action": action }),
    )?;
    file.persist(output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_translations_can_address_local_drives_or_the_selected_wsl_share() {
        assert!(matches!(
            Location::translated_from_wsl("QA Linux", r"\\wsl$\qa linux\home\雪.rs"),
            Some(Location::Wsl(item)) if item == Path::new(r"\\wsl.localhost\QA Linux\home\雪.rs"),
        ));
        assert!(matches!(
            Location::translated_from_wsl("QA Linux", r"C:\source tree\main.rs"),
            Some(Location::Drive(item)) if item == Path::new(r"C:\source tree\main.rs"),
        ));
        for invalid in [
            r"\\wsl.localhost\Other Linux\file",
            r"\\invalid.example\QA Linux\file",
            r"\\wsl$\QA Linux\..\Other Linux\file",
            r"\\.\PhysicalDrive0",
            r"\\wsl$\QA Linux\home\file:stream",
            r"rel\file",
        ] {
            assert!(
                Location::translated_from_wsl("QA Linux", invalid).is_none(),
                "{invalid}"
            );
        }
    }
    #[test]
    fn the_folder_action_refuses_a_regular_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let error = Location::Drive(file.path().into()).open().unwrap_err();
        assert!(format!("{error:#}").contains("The path must identify a filesystem folder"));
    }

    #[test]
    fn wsl_folders_are_restricted_to_unambiguous_names_inside_the_selected_distribution() {
        for (linux, expected) in [
            ("/", r"\\wsl.localhost\QA Linux\"),
            (
                "/home/雪 [pilot] 100% #/",
                r"\\wsl.localhost\QA Linux\home\雪 [pilot] 100% #",
            ),
        ] {
            let Some(Location::Wsl(folder)) = Location::wsl("QA Linux", linux) else {
                panic!("expected a WSL folder for {linux}");
            };
            assert_eq!(folder, Path::new(expected));
        }
        for (distro, linux) in [
            ("", "/"),
            ("..", "/home"),
            (r"dev\other", "/home"),
            ("dev", "/../other"),
            ("dev", "//"),
            ("dev", "//host/share"),
            ("dev", "/home//other"),
            ("dev", "/home/file:stream"),
            ("dev", "/home/rewrite. "),
        ] {
            assert!(Location::wsl(distro, linux).is_none(), "{distro} {linux}");
        }
    }

    #[test]
    fn resolves_absolute_and_relative_paths_with_literal_names() {
        let cwd = Some(Path::new(r"C:\Work\build"));
        for (hint, expected) in [
            (
                r"D:\Project 100% # [pilot]\雪.ps1",
                r"D:\Project 100% # [pilot]\雪.ps1",
            ),
            (r"D:/Project/a/../b", r"D:\Project\b"),
            (
                r".\draft %USERPROFILE%.txt",
                r"C:\Work\build\draft %USERPROFILE%.txt",
            ),
            (r"../src/main.rs", r"C:\Work\src\main.rs"),
            (r"src/lib.rs", r"C:\Work\build\src\lib.rs"),
            (r".gitignore", r"C:\Work\build\.gitignore"),
            (r"~release.txt", r"C:\Work\build\~release.txt"),
            (r".\folder\", r"C:\Work\build\folder"),
            (r"C:/", r"C:\"),
        ] {
            assert_eq!(
                resolve(hint, cwd).as_deref(),
                Some(Path::new(expected)),
                "{hint}"
            );
        }
        assert_eq!(
            resolve(r"D:\target.txt", None).as_deref(),
            Some(Path::new(r"D:\target.txt"))
        );
        assert_eq!(resolve(r".\target.txt", None), None);
    }

    #[test]
    fn rejects_ambiguous_paths_devices_streams_and_invalid_bases() {
        let cwd = Some(Path::new(r"C:\work"));
        for hint in [
            "",
            r"\share\file",
            r"\\host\share\file",
            "//host/share/file",
            r"\\?\C:\file",
            r"\\.\pipe\file",
            r"C:relative\file",
            r"/etc/file",
            r"~/src/file",
            r"~\file",
            r"file:stream",
            r"file:///C:/file",
            r"C:\src\NUL.txt",
            r"C:\src\COM¹",
            r"C:\src\bad. ",
            r"C:\foo\\bar",
            r"C:\\host\path",
            r"C:\..\etc",
            r"..\..\etc",
            "C:/\0file",
            "C:/src/\nissue",
        ] {
            assert_eq!(resolve(hint, cwd), None, "{hint:?}");
        }
        for base in [r"\\host\share", r"\\?\C:\work", r"C:work", r"C:\CON"] {
            assert_eq!(resolve("file.txt", Some(Path::new(base))), None, "{base}");
        }
    }

    #[test]
    fn checks_the_exact_existing_local_item_before_asking_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("hello # [世界].cmd");
        assert!(check_item(dir.path()).is_ok());
        assert!(check_item(&entry).is_err());
        std::fs::write(&entry, []).unwrap();
        assert!(check_item(&entry).is_ok());
        assert!(check_drive(Path::new(r"\\invalid.example\share\file")).is_err());
    }
}
