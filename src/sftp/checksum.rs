use super::*;
use std::{
    fs::File,
    io::{Read, Seek},
};
use upload_directory::{Source, open_source};
use windows::Win32::Storage::FileSystem::{FILE_READ_DATA, FILE_SHARE_READ};

/// Hash a selected file through one read-only handle that denies writes and deletes.
pub fn local_sha256(path: &Path, cancel: &TransferCancel) -> Result<String> {
    local_sha256_with_size(path, None, cancel)
}

pub(crate) fn local_sha256_with_size(
    path: &Path,
    expected_bytes: Option<u64>,
    cancel: &TransferCancel,
) -> Result<String> {
    ensure!(!cancel.is_cancelled(), "SHA-256 comparison cancelled");
    let (file, size) = open_local_for_hash(path, expected_bytes)?;
    hash_local_handle(file, size, None, cancel)
}

pub(crate) fn open_local_verified(path: &Path, bytes: u64, sha256: &str) -> Result<File> {
    let (mut file, size) = open_local_for_hash(path, Some(bytes))?;
    let found = hash_local_handle(&mut file, size, None, &TransferCancel::default())?;
    ensure!(
        found == sha256,
        "File SHA-256 differs from the expected digest"
    );
    file.rewind()?;
    Ok(file)
}

fn open_local_for_hash(path: &Path, expected_bytes: Option<u64>) -> Result<(File, u64)> {
    let Source::File(file, size) = open_source(path, FILE_READ_DATA, FILE_SHARE_READ)? else {
        bail!("Choose a regular file for SHA-256 comparison");
    };
    if let Some(expected) = expected_bytes {
        ensure!(
            size == expected,
            "File byte count differs from the expected {expected} bytes (found {size})"
        );
    }
    Ok((file, size))
}

pub(super) fn hash_local_handle(
    mut file: impl Read,
    size: u64,
    max_bytes: Option<u64>,
    cancel: &TransferCancel,
) -> Result<String> {
    let check_cancel = || {
        ensure!(!cancel.is_cancelled(), "SHA-256 comparison cancelled");
        Ok::<_, anyhow::Error>(())
    };
    check_cancel()?;
    if let Some(limit) = max_bytes {
        ensure!(
            size <= limit,
            "The Windows file exceeds the SHA-256 byte limit ({limit} bytes)"
        );
    }
    let mut digest = Sha256::new();
    let mut buffer = [0; CHUNK];
    let mut bytes = 0u64;
    loop {
        check_cancel()?;
        let length = file.read(&mut buffer).context("reading the local file")?;
        if length == 0 {
            break;
        }
        bytes += length as u64;
        ensure!(
            bytes <= size,
            "Local file size changed during SHA-256 calculation"
        );
        digest.update(&buffer[..length]);
    }
    check_cancel()?;
    ensure!(
        bytes == size,
        "Local file size changed during SHA-256 calculation"
    );
    Ok(digest_hex(digest))
}

impl Engine {
    pub(super) async fn remote_sha256(
        &self,
        path: &str,
        max_bytes: Option<u64>,
        cancel: Option<&TransferCancel>,
        require_canonical_parent: bool,
    ) -> Result<Value> {
        ensure!(
            !path.contains('\0')
                && !matches!(path.rsplit('/').next(), None | Some("" | "." | "..")),
            "SHA-256 requires a remote file path"
        );
        let check_cancel = || {
            ensure!(
                !cancel.is_some_and(TransferCancel::is_cancelled),
                "SHA-256 calculation cancelled"
            );
            Ok::<_, anyhow::Error>(())
        };
        check_cancel()?;
        let initial = self.checksum_entry(path, require_canonical_parent).await?;
        let limit = max_bytes.map(|bytes| (bytes, "the SHA-256 byte limit"));
        if let Some((max, reason)) = limit {
            ensure!(
                initial.size.is_none_or(|size| size <= max),
                "file exceeds {reason} ({max} bytes)"
            );
        }
        check_cancel()?;
        let handle = self
            .raw
            .open(path, OpenFlags::READ, FileAttributes::empty())
            .await?
            .handle;
        let result = async {
            check_cancel()?;
            // SFTP v3 does not expose a no-follow OPEN. Confirm the path again
            // before reading and use handle metadata when the server supplies it.
            let opened_path = self.checksum_entry(path, require_canonical_parent).await?;
            check_unchanged(&initial, &opened_path)?;
            let opened_handle = match self.raw.fstat(&handle).await {
                Ok(reply) => {
                    let attrs = reply.attrs;
                    ensure!(
                        attrs.permissions.is_none_or(|mode| mode & 0o170000 == 0
                            || EntryKind::from_permissions(Some(mode)) == Some(EntryKind::File)),
                        "The opened entry is not a regular file"
                    );
                    check_unchanged(&opened_path, &attrs)?;
                    attrs
                }
                Err(Error::Status(status))
                    if matches!(
                        status.status_code,
                        StatusCode::OpUnsupported | StatusCode::PermissionDenied
                    ) =>
                {
                    FileAttributes::empty()
                }
                Err(error) => return Err(error.into()),
            };
            let total = opened_handle.size.or(opened_path.size).or(initial.size);
            if let Some((max, reason)) = limit {
                ensure!(
                    total.is_none_or(|size| size <= max),
                    "file exceeds {reason} ({max} bytes)"
                );
            }
            let mut digest = Sha256::new();
            let bytes = self
                .read_contents(
                    &handle,
                    &mut tokio::io::sink(),
                    0,
                    limit,
                    cancel,
                    |_, data| {
                        digest.update(data);
                        Ok(())
                    },
                )
                .await?;
            check_cancel()?;
            let final_attrs = self.checksum_entry(path, require_canonical_parent).await?;
            check_unchanged(&initial, &final_attrs)?;
            check_unchanged(&opened_path, &final_attrs)?;
            check_unchanged(&opened_handle, &final_attrs)?;
            ensure!(
                total.is_none_or(|size| size == bytes)
                    && final_attrs.size.is_none_or(|size| size == bytes),
                "Remote file size changed during SHA-256 calculation"
            );
            Ok(json!({"sha256": digest_hex(digest), "bytes": bytes}))
        }
        .await;
        self.raw.close(handle).await?;
        check_cancel()?;
        result
    }

    async fn checksum_entry(
        &self,
        path: &str,
        require_canonical_parent: bool,
    ) -> Result<FileAttributes> {
        if require_canonical_parent {
            self.checked_source_folder(path).await?;
        }
        let attrs = self.raw.lstat(path).await?.attrs;
        ensure!(
            EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::File),
            "SHA-256 requires a confirmed regular file"
        );
        Ok(attrs)
    }
}

fn check_unchanged(before: &FileAttributes, after: &FileAttributes) -> Result<()> {
    ensure!(
        before.size.zip(after.size).is_none_or(|(a, b)| a == b)
            && before.mtime.zip(after.mtime).is_none_or(|(a, b)| a == b),
        "Remote file changed during SHA-256 calculation"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_local_file_bytes_across_read_boundaries() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("résumé [local].bin");
        let cancel = TransferCancel::default();
        std::fs::write(&path, [])?;
        assert_eq!(
            local_sha256(&path, &cancel)?,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let data = b"w11term\0".repeat(CHUNK);
        std::fs::write(&path, &data)?;
        assert_eq!(
            local_sha256(&path, &cancel)?,
            "8caddaac9b2a1eae17432050d03e02c33b3a9fe7826debb7b6e3b1da1ad322a8"
        );
        assert!(local_sha256(directory.path(), &cancel).is_err());
        cancel.cancel();
        assert!(
            local_sha256(&path, &cancel)
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        Ok(())
    }
}
