use super::*;

impl Engine {
    pub(super) async fn read_preview_text(
        &self,
        path: &str,
        cancel: Option<&TransferCancel>,
    ) -> Result<String> {
        ensure!(
            path.starts_with('/')
                && !path.contains('\0')
                && !matches!(path.rsplit('/').next(), None | Some("" | "." | "..")),
            "Text preview requires an absolute remote file path"
        );
        let check_cancel = || {
            ensure!(
                !cancel.is_some_and(TransferCancel::is_cancelled),
                "Text preview cancelled"
            );
            Ok::<_, anyhow::Error>(())
        };
        check_cancel()?;
        self.check_preview_entry(path).await?;
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
            self.check_preview_entry(path).await?;
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
                    check_size(reply.attrs.size)?;
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
                Some((
                    TEXT_PREVIEW_MAX_BYTES as u64,
                    "the 256 KiB text preview limit",
                )),
                cancel,
                |_| Ok(()),
            )
            .await?;
            check_cancel()?;
            ensure!(
                !bytes.contains(&0),
                "This file contains NUL bytes and cannot be previewed as text"
            );
            String::from_utf8(bytes).context("This file cannot be previewed as UTF-8 text")
        }
        .await;
        let closed = self.raw.close(handle).await;
        // Propagate transport/close errors so the worker can invalidate a broken subsystem.
        closed?;
        check_cancel()?;
        read
    }

    async fn check_preview_entry(&self, path: &str) -> Result<()> {
        let attrs = self.raw.lstat(path).await?.attrs;
        ensure!(
            EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::File),
            "Text preview requires a confirmed regular file"
        );
        check_size(attrs.size)
    }
}

fn check_size(size: Option<u64>) -> Result<()> {
    ensure!(
        size.is_none_or(|size| size <= TEXT_PREVIEW_MAX_BYTES as u64),
        "This file exceeds the 256 KiB text preview limit"
    );
    Ok(())
}
