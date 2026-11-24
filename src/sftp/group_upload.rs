use super::upload_directory::{Source, open_source};
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::File,
    os::windows::io::{AsRawHandle, FromRawHandle},
    path::{Path, PathBuf},
    sync::Arc,
};
use windows::Win32::{Foundation::HANDLE, Storage::FileSystem::*};

/// The confirmed file stays open without write or delete sharing while a batch
/// runs. Each host reads the same file object with its own independent offset.
#[derive(Clone, Debug)]
pub struct GroupUploadFile {
    path: PathBuf,
    file: Arc<File>,
    size: u64,
}

impl GroupUploadFile {
    pub fn open(path: PathBuf) -> Result<Self> {
        let Source::File(file, size) = open_source(&path, FILE_READ_DATA, FILE_SHARE_READ)? else {
            bail!("Choose an ordinary file for the group upload");
        };
        Ok(Self {
            path,
            file: Arc::new(file),
            size,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub(super) fn reader(&self) -> Result<File> {
        let reopened = unsafe {
            ReOpenFile(
                HANDLE(self.file.as_raw_handle()),
                (FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE).0,
                FILE_SHARE_READ,
                FILE_FLAG_SEQUENTIAL_SCAN,
            )
        }
        .context("opening an independent reader for the selected file")?;
        // ReOpenFile transfers ownership of a new handle to this reader.
        Ok(unsafe { File::from_raw_handle(reopened.0) })
    }
}

pub fn validate_group_upload_path(path: &str) -> Result<()> {
    // OpenSSH for Windows also recognizes drive roots, with or without the
    // initial slash. The same explicit filename must be used on every host.
    let ordinary = path.strip_prefix('/').unwrap_or(path);
    let drive = ordinary.as_bytes();
    let has_drive = matches!(drive, [letter, b':', b'/', ..] if letter.is_ascii_alphabetic());
    let components = if has_drive { &ordinary[3..] } else { ordinary };
    ensure!(
        (path.starts_with('/') || has_drive)
            && path.len() <= 4096
            && !path.contains('\\')
            && !path.chars().any(char::is_control)
            && !components.contains(':')
            && components.split('/').all(|name| {
                !matches!(name, "" | "." | "..") && (!has_drive || !name.ends_with([' ', '.']))
            }),
        "Enter an absolute remote file path with a filename, such as /srv/incoming/report.txt"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn reopened_readers_use_independent_offsets_and_the_source_stays_locked() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("source.txt");
        std::fs::write(&path, b"group bytes")?;
        let source = GroupUploadFile::open(path.clone())?;
        let mut east = source.reader()?;
        let mut west = source.reader()?;
        let mut head = [0; 5];
        east.read_exact(&mut head)?;
        let mut complete = Vec::new();
        west.read_to_end(&mut complete)?;
        let mut rest = Vec::new();
        east.read_to_end(&mut rest)?;
        assert_eq!(&head, b"group");
        assert_eq!(rest, b" bytes");
        assert_eq!(complete, b"group bytes");
        assert!(std::fs::OpenOptions::new().write(true).open(&path).is_err());
        assert!(GroupUploadFile::open(temp.path().into()).is_err());
        drop((east, west, source));
        std::fs::OpenOptions::new().write(true).open(path)?;
        Ok(())
    }

    #[test]
    fn group_destination_requires_an_explicit_absolute_filename() {
        for path in [
            "/srv/incoming/report.txt",
            "/name with spaces",
            "C:/data/report.txt",
            "/Z:/report.txt",
        ] {
            assert!(validate_group_upload_path(path).is_ok(), "{path}");
        }
        for path in [
            "",
            "/",
            "local.txt",
            "/data/",
            "/data//report",
            "/data/../report",
            "/./report",
            "/srv/report\ntext",
            "/C:/",
            "/C:/data/alias:stream",
            "C:/name.",
        ] {
            assert!(validate_group_upload_path(path).is_err(), "{path}");
        }
    }
}
