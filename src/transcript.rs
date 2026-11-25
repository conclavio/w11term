use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{self, Write},
    path::Path,
};

/// Publish complete UTF-8 text with a single rename in the destination directory.
pub fn save(path: &Path, text: &str, overwrite: bool) -> Result<()> {
    save_with(path, overwrite, |file| file.write_all(text.as_bytes()))
}

/// Write the export before publishing it at the selected destination.
pub fn save_with(
    path: &Path,
    overwrite: bool,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> Result<()> {
    ensure!(path.file_name().is_some(), "Choose a file name");
    let directory = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(directory)
        .with_context(|| format!("Creating a temporary file in {}", directory.display()))?;
    write(file.as_file_mut()).context("Writing export")?;
    file.as_file().sync_all()?;
    let result = if overwrite {
        file.persist(path)
    } else {
        file.persist_noclobber(path)
    };
    result
        .map(|_| ())
        .map_err(|error| error.error)
        .with_context(|| format!("Saving {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn preserves_exact_bytes_and_requires_permission_to_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.txt");
        save(&path, "first\r\n", false).unwrap();
        assert!(save(&path, "second", false).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"first\r\n");
        let unicode = "日本語 🐈\r\n\r\nlast";
        save(&path, unicode, true).unwrap();
        assert_eq!(fs::read(&path).unwrap(), unicode.as_bytes());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_publish_cleans_the_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("directory");
        fs::create_dir(&destination).unwrap();
        assert!(save(&destination, "transcript", true).is_err());
        assert!(destination.is_dir());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn interrupted_write_keeps_the_previous_export() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("previous.txt");
        fs::write(&path, "previous").unwrap();
        let error = save_with(&path, true, |file| {
            file.write_all(b"partial replacement")?;
            Err(io::Error::other("interrupted"))
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("interrupted"));
        assert_eq!(fs::read(&path).unwrap(), b"previous");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn failed_replacement_preserves_the_existing_file() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in-use.txt");
        fs::write(&path, "original").unwrap();
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(save(&path, "replacement", true).is_err());
        drop(locked);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
