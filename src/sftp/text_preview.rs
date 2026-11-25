use super::*;

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

impl Engine {
    pub(super) async fn read_preview_text(
        &self,
        path: &str,
        cancel: Option<&TransferCancel>,
    ) -> Result<String> {
        let bytes = self.read_preview_bytes(path, cancel, &TEXT).await?;
        ensure!(
            !bytes.contains(&0),
            "This file contains NUL bytes and cannot be previewed as text"
        );
        String::from_utf8(bytes).context("This file cannot be previewed as UTF-8 text")
    }

    pub(super) async fn read_preview_bytes(
        &self,
        path: &str,
        cancel: Option<&TransferCancel>,
        limit: &PreviewLimit,
    ) -> Result<Vec<u8>> {
        ensure!(
            path.starts_with('/')
                && !path.contains('\0')
                && !matches!(path.rsplit('/').next(), None | Some("" | "." | "..")),
            "{} requires an absolute remote file path",
            limit.name
        );
        let check_cancel = || {
            ensure!(
                !cancel.is_some_and(TransferCancel::is_cancelled),
                "{} cancelled",
                limit.name
            );
            Ok::<_, anyhow::Error>(())
        };
        check_cancel()?;
        self.check_preview_entry(path, limit).await?;
        check_cancel()?;
        let handle = self
            .raw
            .open(path, OpenFlags::READ, FileAttributes::empty())
            .await?
            .handle;
        let read = async {
            check_cancel()?;
            // SFTP v3 has no no-follow OPEN. Re-check the directory entry and
            // reject a special file type on the opened handle if the peer supplies one.
            self.check_preview_entry(path, limit).await?;
            match self.raw.fstat(&handle).await {
                Ok(reply) => {
                    if let Some(mode) = reply.attrs.permissions
                        && mode & 0o170000 != 0
                    {
                        ensure!(
                            EntryKind::from_permissions(Some(mode)) == Some(EntryKind::File),
                            "The opened entry is not a regular file"
                        );
                    }
                    limit.check_size(reply.attrs.size)?;
                }
                Err(Error::Status(status))
                    if matches!(
                        status.status_code,
                        StatusCode::OpUnsupported | StatusCode::PermissionDenied
                    ) => {}
                Err(error) => return Err(error.into()),
            }
            let mut bytes = Vec::new();
            self.read_contents(
                &handle,
                &mut bytes,
                0,
                Some((limit.bytes, limit.limit)),
                cancel,
                |_, _| Ok(()),
            )
            .await?;
            check_cancel()?;
            Ok(bytes)
        }
        .await;
        let closed = self.raw.close(handle).await;
        // Propagate transport/close errors so the worker can invalidate a broken subsystem.
        closed?;
        check_cancel()?;
        read
    }

    async fn check_preview_entry(&self, path: &str, limit: &PreviewLimit) -> Result<()> {
        let attrs = self.raw.lstat(path).await?.attrs;
        ensure!(
            EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::File),
            "{} requires a confirmed regular file",
            limit.name
        );
        limit.check_size(attrs.size)
    }
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
