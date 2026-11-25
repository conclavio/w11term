use super::*;
use std::{
    fs::{File, OpenOptions},
    mem::offset_of,
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
};
use windows::Win32::{
    Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE},
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_DISPOSITION_INFO, FILE_FLAG_OPEN_REPARSE_POINT, FILE_RENAME_INFO, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileDispositionInfo, FileRenameInfo, GetFileInformationByHandle,
        SetFileInformationByHandle,
    },
};

pub(super) struct ResumeDownload {
    pub(super) path: PathBuf,
    destination: PathBuf,
    _parent: File,
}

impl ResumeDownload {
    pub(super) fn open(local: &Path, digest: &str) -> Result<(Self, File)> {
        let name = local
            .file_name()
            .and_then(|name| name.to_str())
            .context("Resumable downloads require a Windows filename")?;
        validate_download_name(name)?;
        let parent = local
            .parent()
            .context("local destination has no parent")?
            .canonicalize()?;
        let pin = directory::pin_directory(&parent, FILE_SHARE_READ | FILE_SHARE_WRITE)?;
        let mut key = Sha256::new();
        key.update(digest);
        key.update(name);
        let path = parent.join(format!(".w11term-download-{}.part", digest_hex(key)));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .access_mode(GENERIC_READ.0 | GENERIC_WRITE.0 | DELETE.0)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(&path)
            .with_context(|| format!("opening partial download {}", path.display()))?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // Inspect the exact file that will receive data with write/delete sharing disabled.
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }?;
        ensure!(
            info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT).0
                == 0
                && info.nNumberOfLinks == 1,
            "Partial download is not an ordinary single-link file: {}",
            path.display()
        );
        apply_mark_of_the_web(&path)?;
        Ok((
            Self {
                path,
                destination: parent.join(name),
                _parent: pin,
            },
            file,
        ))
    }

    pub(super) fn publish(self, file: File, overwrite: bool) -> Result<()> {
        let wide: Vec<_> = self.destination.as_os_str().encode_wide().collect();
        let name_bytes = size_of_val(wide.as_slice());
        let bytes = offset_of!(FILE_RENAME_INFO, FileName) + name_bytes + size_of::<u16>();
        // The zero-filled allocation gives the native structure its alignment and
        // supplies a terminating zero after the variable-length filename.
        let mut buffer = vec![0u64; bytes.max(size_of::<FILE_RENAME_INFO>()).div_ceil(8)];
        let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // Rename the same exclusive handle that was read and verified.
        unsafe {
            (*info).Anonymous.ReplaceIfExists = overwrite;
            (*info).FileNameLength = name_bytes.try_into()?;
            std::ptr::copy_nonoverlapping(
                wide.as_ptr(),
                (&raw mut (*info).FileName).cast::<u16>(),
                wide.len(),
            );
            SetFileInformationByHandle(
                HANDLE(file.as_raw_handle()),
                FileRenameInfo,
                info.cast(),
                (buffer.len() * 8).try_into()?,
            )
        }
        .with_context(|| {
            format!(
                "publishing {} (partial download kept at {})",
                self.destination.display(),
                self.path.display()
            )
        })
    }

    pub(super) fn discard(self, file: File) -> Result<()> {
        let info = FILE_DISPOSITION_INFO { DeleteFile: true };
        unsafe {
            SetFileInformationByHandle(
                HANDLE(file.as_raw_handle()),
                FileDispositionInfo,
                (&raw const info).cast(),
                size_of_val(&info) as u32,
            )
        }
        .with_context(|| format!("removing invalid partial download {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};

    #[test]
    fn holds_checkpoint_exclusively_and_publishes_with_windows_download_mark() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("test 雪.bin");
        let digest = "a".repeat(64);
        let (checkpoint, mut file) = ResumeDownload::open(&destination, &digest).unwrap();
        file.write_all(b"prefix").unwrap();
        assert!(ResumeDownload::open(&destination, &digest).is_err());
        drop(file);
        drop(checkpoint);
        fs::write(&destination, "existing").unwrap();
        let (checkpoint, file) = ResumeDownload::open(&destination, &digest).unwrap();
        let partial = checkpoint.path.clone();
        assert!(checkpoint.publish(file, false).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"existing");
        let (checkpoint, file) = ResumeDownload::open(&destination, &digest).unwrap();
        checkpoint.publish(file, true).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"prefix");
        assert!(!partial.exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("test 雪.bin:Zone.Identifier")).unwrap(),
            "[ZoneTransfer]\r\nZoneId=3\r\n"
        );
    }

    #[test]
    fn refuses_a_checkpoint_hardlinked_to_another_file() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("download.bin");
        let digest = "a".repeat(64);
        let (checkpoint, file) = ResumeDownload::open(&destination, &digest).unwrap();
        let original = dir.path().join("other.bin");
        drop(file);
        fs::hard_link(&checkpoint.path, &original).unwrap();
        assert!(ResumeDownload::open(&destination, &digest).is_err());
    }
}
