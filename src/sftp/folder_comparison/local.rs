use super::*;
use crate::windows_dirs;
use std::{fs::File, os::windows::fs::MetadataExt};
use windows::{
    Wdk::Storage::FileSystem::{
        FILE_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
    },
    Win32::{
        Foundation::RtlNtStatusToDosError,
        Storage::FileSystem::{
            FILE_ATTRIBUTE_DEVICE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_LIST_DIRECTORY, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
        },
    },
};

const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
const FILETIME_TICKS_PER_SECOND: i64 = 10_000_000;

fn unix_seconds(time: i64) -> Option<u32> {
    let elapsed = time.checked_sub(FILETIME_UNIX_EPOCH)?;
    if elapsed < 0 {
        return None;
    }
    u32::try_from(elapsed / FILETIME_TICKS_PER_SECOND).ok()
}

struct Scan<'a> {
    inventory: InventoryBuilder,
    directories: usize,
    entries: usize,
    cancel: &'a TransferCancel,
}

impl Scan<'_> {
    fn walk(&mut self, dir: &File, prefix: &str, depth: usize) -> Result<()> {
        self.directories += 1;
        ensure!(
            self.directories <= find::MAX_DIRECTORIES && depth <= find::MAX_DEPTH,
            "The Windows folder exceeded the scan limits"
        );
        windows_dirs::entries(dir, |entry| {
            ensure!(
                !self.cancel.is_cancelled(),
                "Windows folder capture stopped"
            );
            self.entries += 1;
            ensure!(
                self.entries <= find::MAX_ENTRIES,
                "The Windows folder exceeded the scan limits"
            );
            let name = String::from_utf16(entry.name)
                .context("Windows folder entry has an invalid Unicode name")?;
            ensure!(
                !name.contains(['/', '\\', ':']) && !name.chars().any(char::is_control),
                "The Windows folder contains an unsupported entry name"
            );
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            ensure!(
                path.len() <= find::MAX_PATH_BYTES,
                "A Windows folder entry path is too long"
            );
            let kind = if entry.attributes.contains(FILE_ATTRIBUTE_REPARSE_POINT) {
                WINDOWS_REPARSE
            } else if entry.attributes.contains(FILE_ATTRIBUTE_DEVICE) {
                WINDOWS_SPECIAL
            } else if entry.attributes.contains(FILE_ATTRIBUTE_DIRECTORY) {
                DIRECTORY
            } else {
                REGULAR
            };
            self.inventory.insert_entry(
                &path,
                FolderEntry {
                    kind,
                    size: entry.size.filter(|_| kind == REGULAR),
                    mtime: (kind == REGULAR)
                        .then(|| unix_seconds(entry.last_write_time))
                        .flatten(),
                },
            )?;
            if kind == DIRECTORY {
                let child = windows_dirs::open_relative(
                    dir,
                    &name,
                    FILE_LIST_DIRECTORY | FILE_TRAVERSE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    FILE_OPEN,
                    FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                )
                .map_err(|status| {
                    std::io::Error::from_raw_os_error(
                        unsafe { RtlNtStatusToDosError(status) } as i32
                    )
                })
                .with_context(|| format!("Opening Windows folder {path}"))?;
                ensure!(
                    child.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
                    "The Windows folder changed while capturing {path}"
                );
                self.walk(&child, &path, depth + 1)?;
            }
            Ok(())
        })
    }
}

/// Capture a Windows folder using directory metadata and handle-relative traversal.
pub fn capture_local_folder(local: &Path, cancel: &TransferCancel) -> Result<FolderInventory> {
    ensure!(!cancel.is_cancelled(), "Windows folder capture stopped");
    let root = local
        .to_str()
        .context("Windows folder path is not valid Unicode")?;
    let upload_directory::Source::Directory(dir) = upload_directory::open_source(
        local,
        FILE_LIST_DIRECTORY | FILE_TRAVERSE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )?
    else {
        bail!("Choose a Windows folder with a regular directory at its root");
    };
    let mut scan = Scan {
        inventory: InventoryBuilder::default(),
        directories: 0,
        entries: 0,
        cancel,
    };
    scan.walk(&dir, "", 0)?;
    ensure!(!cancel.is_cancelled(), "Windows folder capture stopped");
    Ok(scan
        .inventory
        .finish(root.to_owned(), FolderOrigin::Windows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::windows::fs::OpenOptionsExt};

    #[test]
    fn windows_times_use_the_sftp_second_and_mark_unrepresentable_values_unknown() {
        assert_eq!(unix_seconds(0), None);
        assert_eq!(unix_seconds(FILETIME_UNIX_EPOCH - 1), None);
        assert_eq!(unix_seconds(FILETIME_UNIX_EPOCH + 1), Some(0));
        let limit = FILETIME_UNIX_EPOCH + i64::from(u32::MAX) * FILETIME_TICKS_PER_SECOND;
        assert_eq!(
            unix_seconds(limit + FILETIME_TICKS_PER_SECOND - 1),
            Some(u32::MAX)
        );
        assert_eq!(unix_seconds(limit + FILETIME_TICKS_PER_SECOND), None);
    }

    #[test]
    fn windows_capture_reads_metadata_even_with_an_exclusively_open_file() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::create_dir(tmp.path().join("Nested"))?;
        let path = tmp.path().join("Nested/Config.txt");
        fs::write(&path, b"abc")?;
        let held = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&path)?;
        let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_321);
        held.set_times(fs::FileTimes::new().set_modified(modified))?;
        let snapshot = capture_local_folder(tmp.path(), &TransferCancel::default())?;
        assert_eq!(snapshot.origin, FolderOrigin::Windows);
        assert_eq!(snapshot.entries.len(), 2);
        assert_eq!(snapshot.entries["Nested"].kind, DIRECTORY);
        assert_eq!(snapshot.entries["Nested/Config.txt"].size, Some(3));
        assert_eq!(
            snapshot.entries["Nested/Config.txt"].mtime,
            Some(1_700_000_321)
        );
        Ok(())
    }

    #[test]
    fn windows_capture_rejects_partial_inventories() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let cancel = TransferCancel::default();
        cancel.cancel();
        assert!(
            capture_local_folder(tmp.path(), &cancel)
                .unwrap_err()
                .to_string()
                .contains("stopped")
        );
        let mut path = tmp.path().to_owned();
        for _ in 0..=find::MAX_DEPTH {
            path.push("next");
        }
        fs::create_dir_all(path)?;
        let error = capture_local_folder(tmp.path(), &TransferCancel::default()).unwrap_err();
        assert!(error.to_string().contains("scan limits"), "{error:#}");
        Ok(())
    }
}
