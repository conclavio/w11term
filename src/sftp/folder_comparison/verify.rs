use super::*;
use upload_directory::{Source, open_child, source_root};

pub const FILE_VERIFICATION_LIMIT: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerifiedFolderFile {
    pub reference_sha256: String,
    pub current_sha256: String,
}

impl VerifiedFolderFile {
    pub fn contents_match(&self) -> bool {
        self.reference_sha256 == self.current_sha256
    }
}

pub fn valid_comparison_file_path(path: &str) -> bool {
    path.len() <= find::MAX_PATH_BYTES
        && path.split('/').all(|part| {
            !matches!(part, "" | "." | "..")
                && !part.contains('\\')
                && !part.chars().any(char::is_control)
        })
}

fn hash_windows_reference(root: &Path, relative: &str, cancel: &TransferCancel) -> Result<String> {
    ensure!(!cancel.is_cancelled(), "File comparison cancelled");
    let (parent, name) = relative.rsplit_once('/').unwrap_or(("", relative));
    let mut directory = source_root(root)?;
    for part in parent.split('/').filter(|part| !part.is_empty()) {
        ensure!(!cancel.is_cancelled(), "File comparison cancelled");
        let Source::Directory(child) = open_child(&directory, part)? else {
            bail!("The Windows reference contains a link or a non-folder ancestor");
        };
        directory = child;
    }
    let Source::File(file, size) = open_child(&directory, name)? else {
        bail!("The Windows reference is no longer a regular file");
    };
    checksum::hash_local_handle(file, size, Some(FILE_VERIFICATION_LIMIT), cancel)
}

impl Engine {
    async fn folder_file_digest(
        &self,
        root: &str,
        relative: &str,
        cancel: Option<&TransferCancel>,
    ) -> Result<String> {
        let path = format!("{}/{}", root.trim_end_matches('/'), relative);
        ensure!(
            root.starts_with('/') && path.len() <= find::MAX_PATH_BYTES,
            "The remote file path is invalid or too long"
        );
        let reply = self
            .remote_sha256(&path, Some(FILE_VERIFICATION_LIMIT), cancel, true)
            .await?;
        reply["sha256"]
            .as_str()
            .map(str::to_owned)
            .context("Missing SHA-256 digest")
    }

    pub(in crate::sftp) async fn verify_compared_file(
        &self,
        reference_root: &str,
        reference_origin: FolderOrigin,
        current_root: &str,
        relative: &str,
        cancel: Option<&TransferCancel>,
    ) -> Result<VerifiedFolderFile> {
        ensure!(
            valid_comparison_file_path(relative),
            "Enter a relative file path from the comparison"
        );
        let reference_sha256 = match reference_origin {
            FolderOrigin::Remote => {
                self.folder_file_digest(reference_root, relative, cancel)
                    .await
            }
            FolderOrigin::Windows => {
                let root = PathBuf::from(reference_root);
                let relative = relative.to_owned();
                let cancel = cancel.cloned().unwrap_or_default();
                tokio::task::spawn_blocking(move || {
                    hash_windows_reference(&root, &relative, &cancel)
                })
                .await?
            }
        }
        .context("Could not hash the reference file")?;
        let current_sha256 = self
            .folder_file_digest(current_root, relative, cancel)
            .await
            .context("Could not hash the current remote file")?;
        Ok(VerifiedFolderFile {
            reference_sha256,
            current_sha256,
        })
    }
}
