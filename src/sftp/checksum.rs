use super::*;

impl Engine {
    pub(super) async fn remote_sha256(
        &self,
        path: &str,
        max_bytes: Option<u64>,
        cancel: Option<&TransferCancel>,
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
        let initial = self.checksum_entry(path).await?;
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
            let opened_path = self.checksum_entry(path).await?;
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
            let final_attrs = self.checksum_entry(path).await?;
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

    async fn checksum_entry(&self, path: &str) -> Result<FileAttributes> {
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
