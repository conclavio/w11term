use super::*;
use std::io::Read;
use upload_directory::{Source, open_source};
use windows::Win32::Storage::FileSystem::{FILE_READ_DATA, FILE_SHARE_READ};

pub(super) struct PreviewLimit {
    pub bytes: u64,
    pub name: &'static str,
    pub limit: &'static str,
}

const TEXT: PreviewLimit = PreviewLimit {
    bytes: TEXT_PREVIEW_MAX_BYTES as u64,
    name: "Text preview",
    limit: "the 256 KiB text preview limit",
};

/// Read a selected Windows file using the same text and size limits as a remote preview.
pub fn local_preview_text(path: &Path, cancel: &TransferCancel) -> Result<String> {
    let check_cancel = || check_preview_cancel(Some(cancel), "Text comparison");
    check_cancel()?;
    let Source::File(file, size) = open_source(path, FILE_READ_DATA, FILE_SHARE_READ)? else {
        bail!("Choose a regular local text file");
    };
    TEXT.check_size(Some(size))?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(TEXT.bytes + 1)
        .read_to_end(&mut bytes)
        .context("reading the selected local text file")?;
    check_cancel()?;
    ensure!(
        bytes.len() as u64 == size,
        "The local file changed while reading"
    );
    decode_preview_text(bytes)
}

fn decode_preview_text(bytes: Vec<u8>) -> Result<String> {
    ensure!(
        !bytes.contains(&0),
        "This file contains NUL bytes and cannot be previewed as text"
    );
    String::from_utf8(bytes).context("This file cannot be previewed as UTF-8 text")
}

impl Engine {
    pub(super) async fn read_preview_text(
        &self,
        path: &str,
        cancel: Option<&TransferCancel>,
    ) -> Result<String> {
        let bytes = self.read_preview_bytes(path, cancel, &TEXT).await?;
        decode_preview_text(bytes)
    }

    pub(super) async fn read_preview_bytes(
        &self,
        path: &str,
        cancel: Option<&TransferCancel>,
        limit: &PreviewLimit,
    ) -> Result<Vec<u8>> {
        let (handle, _) = self
            .open_preview_file(path, limit.name, Some(limit), cancel)
            .await?;
        let mut bytes = Vec::new();
        let read = self
            .read_contents(
                &handle,
                &mut bytes,
                0,
                Some((limit.bytes, limit.limit)),
                cancel,
                |_, _| Ok(()),
            )
            .await;
        // Propagate transport/close errors so the worker can invalidate a broken subsystem.
        self.raw.close(handle).await?;
        check_preview_cancel(cancel, limit.name)?;
        read?;
        Ok(bytes)
    }

    pub(super) async fn open_preview_file(
        &self,
        path: &str,
        name: &str,
        limit: Option<&PreviewLimit>,
        cancel: Option<&TransferCancel>,
    ) -> Result<(String, Option<u64>)> {
        ensure!(
            path.starts_with('/')
                && !path.contains('\0')
                && !matches!(path.rsplit('/').next(), None | Some("" | "." | "..")),
            "{name} requires an absolute remote file path"
        );
        check_preview_cancel(cancel, name)?;
        self.check_preview_entry(path, name, limit).await?;
        check_preview_cancel(cancel, name)?;
        let handle = self
            .raw
            .open(path, OpenFlags::READ, FileAttributes::empty())
            .await?
            .handle;
        let checked = async {
            check_preview_cancel(cancel, name)?;
            // SFTP v3 has no no-follow OPEN. Recheck the directory entry and
            // verify the opened handle's type when its server supplies one.
            let path_size = self.check_preview_entry(path, name, limit).await?;
            let size = match self.raw.fstat(&handle).await {
                Ok(reply) => {
                    if let Some(mode) = reply.attrs.permissions
                        && mode & 0o170000 != 0
                    {
                        ensure!(
                            EntryKind::from_permissions(Some(mode)) == Some(EntryKind::File),
                            "The opened entry is not a regular file"
                        );
                    }
                    if let Some(limit) = limit {
                        limit.check_size(reply.attrs.size)?;
                    }
                    reply.attrs.size.or(path_size)
                }
                Err(Error::Status(status))
                    if matches!(
                        status.status_code,
                        StatusCode::OpUnsupported | StatusCode::PermissionDenied
                    ) =>
                {
                    path_size
                }
                Err(error) => return Err(error.into()),
            };
            check_preview_cancel(cancel, name)?;
            Ok(size)
        }
        .await;
        match checked {
            Ok(size) => Ok((handle, size)),
            Err(error) => {
                self.raw.close(handle).await?;
                Err(error)
            }
        }
    }

    async fn check_preview_entry(
        &self,
        path: &str,
        name: &str,
        limit: Option<&PreviewLimit>,
    ) -> Result<Option<u64>> {
        let attrs = self.raw.lstat(path).await?.attrs;
        ensure!(
            EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::File),
            "{name} requires a confirmed regular file"
        );
        if let Some(limit) = limit {
            limit.check_size(attrs.size)?;
        }
        Ok(attrs.size)
    }
}

pub(super) fn check_preview_cancel(cancel: Option<&TransferCancel>, name: &str) -> Result<()> {
    ensure!(
        !cancel.is_some_and(TransferCancel::is_cancelled),
        "{name} cancelled"
    );
    Ok(())
}

impl PreviewLimit {
    fn check_size(&self, size: Option<u64>) -> Result<()> {
        ensure!(
            size.is_none_or(|size| size <= self.bytes),
            "This file exceeds {}",
            self.limit
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_comparison_reads_exact_utf8_and_rejects_binary_and_oversize() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("layout 雪.txt");
        let cancel = TransferCancel::default();
        let text = "\u{feff}line 1\r\nline 雪";
        std::fs::write(&path, text)?;
        assert_eq!(local_preview_text(&path, &cancel)?, text);
        std::fs::write(&path, b"one\0two")?;
        assert!(
            local_preview_text(&path, &cancel)
                .unwrap_err()
                .to_string()
                .contains("NUL bytes")
        );
        std::fs::write(&path, [0xff])?;
        assert!(
            local_preview_text(&path, &cancel)
                .unwrap_err()
                .to_string()
                .contains("UTF-8")
        );
        std::fs::write(&path, vec![b'x'; TEXT_PREVIEW_MAX_BYTES + 1])?;
        assert!(
            local_preview_text(&path, &cancel)
                .unwrap_err()
                .to_string()
                .contains("256 KiB")
        );
        assert!(local_preview_text(temp.path(), &cancel).is_err());
        cancel.cancel();
        assert!(
            local_preview_text(&path, &cancel)
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        Ok(())
    }
}
