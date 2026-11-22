use anyhow::{Context, Result, anyhow, ensure};
use std::{
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Storage::FileSystem::GetDriveTypeW,
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
        UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName},
    },
    core::PCWSTR,
};

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

fn check_item(path: &Path) -> Result<()> {
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
    // Check the directory entry itself, so links retain their own names and missing
    // leaves never turn a reveal into merely opening their nearest existing parent.
    std::fs::symlink_metadata(path).with_context(|| format!("Cannot find {}", path.display()))?;
    Ok(())
}

pub(super) fn reveal(path: &Path) -> Result<()> {
    check_item(path)?;
    // The caller runs on the Windows thread pool; give the Shell its own STA.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("w11term Explorer".into())
            .spawn_scoped(scope, || {
                let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                unsafe {
                    CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
                    let mut item = std::ptr::null_mut();
                    let result =
                        SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut item, 0, None)
                            .and_then(|()| {
                                // With zero children the absolute PIDL identifies the item
                                // to select in its parent, for both files and directories.
                                SHOpenFolderAndSelectItems(item, None, 0)
                            });
                    CoTaskMemFree(Some(item.cast()));
                    CoUninitialize();
                    result.with_context(|| format!("Selecting {} in File Explorer", path.display()))
                }
            })
            .context("Starting File Explorer selection")?
            .join()
            .map_err(|_| anyhow!("File Explorer selection stopped"))?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(check_item(Path::new(r"\\invalid.example\share\file")).is_err());
    }
}
