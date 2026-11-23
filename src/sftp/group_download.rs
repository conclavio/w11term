use super::directory::{pin_directory, valid_component};
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};
use windows::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

/// One newly created, pinned Windows directory for a reviewed collection.
pub struct GroupDownloadFolder {
    root: PathBuf,
    display: PathBuf,
    _folder: File,
    _parent: File,
}

impl GroupDownloadFolder {
    pub fn create(parent: &Path) -> Result<Self> {
        ensure!(parent.is_absolute(), "Choose an absolute local folder");
        let canonical = fs::canonicalize(parent).context("Resolving the chosen folder")?;
        // Windows needs write sharing on the directory to publish a staged
        // child with an atomic rename. Deny delete sharing on both directories.
        let sharing = FILE_SHARE_READ | FILE_SHARE_WRITE;
        let parent_pin = pin_directory(&canonical, sharing).context("Opening the chosen folder")?;
        let stage = tempfile::Builder::new()
            .prefix("ssh-group-download-")
            .tempdir_in(&canonical)
            .context("Creating the SSH group download folder")?;
        let folder =
            pin_directory(stage.path(), sharing).context("Opening the new download folder")?;
        let display = parent.join(
            stage
                .path()
                .file_name()
                .context("Naming the download folder")?,
        );
        Ok(Self {
            root: stage.keep(),
            display,
            _folder: folder,
            _parent: parent_pin,
        })
    }

    pub fn path(&self) -> &Path {
        &self.display
    }

    pub fn destination(&self, pane: u64, remote: &str) -> PathBuf {
        self.root.join(group_download_name(pane, remote))
    }
}

pub fn group_download_name(pane: u64, remote: &str) -> String {
    let leaf = remote.rsplit('/').next().unwrap_or_default();
    let name = format!("pane-{pane}-{leaf}");
    if !leaf.is_empty() && valid_component(&name) {
        name
    } else {
        format!("pane-{pane}.download")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_disjoint_and_restrict_the_local_file_to_one_component() {
        assert_eq!(
            group_download_name(5, "/host/report.txt"),
            "pane-5-report.txt"
        );
        assert_eq!(
            group_download_name(6, "/host/report.txt"),
            "pane-6-report.txt"
        );
        assert_eq!(group_download_name(5, "/host/AUX"), "pane-5-AUX");
        for path in [
            "/host/a:b",
            "/host/a\\b",
            "/host/a?",
            "/host/end. ",
            "/host/",
        ] {
            assert_eq!(group_download_name(5, path), "pane-5.download");
        }
        assert_eq!(
            group_download_name(5, &format!("/{}", "雪".repeat(255))),
            "pane-5.download"
        );
    }

    #[test]
    fn each_batch_gets_its_own_folder_and_pins_it_during_the_fetch() -> Result<()> {
        use std::io::Write;

        let root = tempfile::tempdir()?;
        let first = GroupDownloadFolder::create(root.path())?;
        let second = GroupDownloadFolder::create(root.path())?;
        assert_ne!(first.path(), second.path());
        let received = first.destination(3, "/report.txt");
        let mut staged = tempfile::NamedTempFile::new_in(first.path())?;
        staged.write_all(b"contents")?;
        staged
            .persist_noclobber(&received)
            .context("installing a complete file inside the pinned folder")?;
        assert!(!second.destination(3, "/report.txt").exists());
        let moved = root.path().join("moved");
        assert!(fs::rename(first.path(), &moved).is_err());
        let path = first.path().to_owned();
        drop(first);
        drop(second);
        fs::rename(path, &moved).context("renaming the released folder")?;
        assert_eq!(fs::read(moved.join("pane-3-report.txt"))?, b"contents");
        Ok(())
    }
}
