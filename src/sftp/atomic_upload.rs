use super::editor::POSIX_RENAME;
use super::*;

impl Engine {
    pub(super) async fn put_atomic(
        &self,
        (local, mut file): (PathBuf, tokio::fs::File),
        remote: String,
        overwrite: bool,
        progress: Option<&watch::Sender<TransferProgress>>,
        cancel: Option<&TransferCancel>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let cancelled = || cancel.is_some_and(TransferCancel::is_cancelled);
        if cancelled() {
            return Ok(cancelled_transfer(&local, &remote, 0, false));
        }
        let metadata = file.metadata().await?;
        ensure!(metadata.is_file(), "local source must be a regular file");
        let total = Some(metadata.len());
        report(progress, 0, total);

        let (parent, name) = remote.rsplit_once('/').unwrap_or((".", &remote));
        ensure!(
            !matches!(name, "" | "." | "..") && !remote.contains('\0'),
            "Choose a remote file name"
        );
        let parent = if parent.is_empty() { "/" } else { parent };
        let names = self.raw.realpath(parent).await?.files;
        let [parent] = names.as_slice() else {
            bail!("SFTP did not resolve the upload directory to a single path");
        };
        ensure!(
            parent.filename.starts_with('/') && !parent.filename.contains('\0'),
            "SFTP did not return an absolute upload directory"
        );
        let parent = parent.filename.trim_end_matches('/');
        let destination = format!("{parent}/{name}");
        let original = if overwrite {
            self.atomic_upload_destination(&destination).await?
        } else {
            self.require_new_name(&destination).await?;
            None
        };
        if original.is_some() {
            self.extension(POSIX_RENAME, "1")
                .context("The server cannot replace an existing file atomically")?;
        }
        if cancelled() {
            return Ok(cancelled_transfer(&local, &remote, 0, false));
        }

        let temporary = format!(
            "{parent}/.w11term-upload-{:032x}.tmp",
            rand::random::<u128>()
        );
        *stage = JobStage::AtomicUpload {
            temporary: temporary.clone(),
            publishing: false,
        };
        let handle = match self
            .raw
            .open(
                &temporary,
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                FileAttributes {
                    permissions: Some(0o600),
                    ..FileAttributes::empty()
                },
            )
            .await
        {
            Ok(reply) => reply.handle,
            Err(error) => {
                if !transport_error(&error) {
                    *stage = JobStage::Ordinary;
                }
                return Err(error.into());
            }
        };
        let prepared = async {
            let (bytes, complete) = self
                .write_upload(&handle, &mut file, total, progress, cancel, stage)
                .await?;
            if complete && !cancelled() {
                if let Some(attrs) = &original {
                    // Match a normal write's removal of set-id bits.
                    self.apply_staged_attributes(&handle, attrs, 0o1777).await?;
                }
                if self.extension(extensions::FSYNC, "1").is_ok() {
                    self.raw.fsync(&handle).await?;
                }
            }
            Ok::<_, anyhow::Error>((bytes, complete))
        }
        .await;
        let closed = self.raw.close(handle).await;
        let publication: Result<(u64, bool)> = async {
            closed?;
            let (bytes, complete) = prepared?;
            if !complete || cancelled() {
                return Ok((bytes, false));
            }
            if let Some(before) = &original {
                let after = self
                    .atomic_upload_destination(&destination)
                    .await?
                    .context("Remote destination changed during upload")?;
                ensure!(
                    (
                        before.size,
                        before.mtime,
                        before.uid,
                        before.gid,
                        before.permissions
                    ) == (
                        after.size,
                        after.mtime,
                        after.uid,
                        after.gid,
                        after.permissions
                    ),
                    "Remote destination changed during upload"
                );
            } else {
                self.require_new_name(&destination).await?;
            }
            if cancelled() {
                return Ok((bytes, false));
            }
            if let JobStage::AtomicUpload { publishing, .. } = stage {
                *publishing = true;
            }
            if original.is_some() {
                self.replace_staged_file(&temporary, &destination).await?;
            } else {
                self.raw.rename(&temporary, &destination).await?;
            }
            Ok((bytes, true))
        }
        .await;
        match publication {
            Ok((bytes, true)) => {
                *stage = JobStage::Ordinary;
                Ok(json!({"local":local,"remote":remote,"bytes_transferred":bytes,"atomic":true}))
            }
            Ok((bytes, false)) => {
                self.raw.remove(&temporary).await.with_context(|| {
                    format!("Could not remove temporary upload {temporary} after cancellation")
                })?;
                *stage = JobStage::Ordinary;
                Ok(cancelled_transfer(&local, &remote, bytes, false))
            }
            Err(error) if error.downcast_ref::<Error>().is_some_and(transport_error) => Err(error),
            Err(error) => {
                self.raw.remove(&temporary).await.with_context(|| {
                    format!(
                        "Upload failed ({error:#}); could not remove temporary file {temporary}"
                    )
                })?;
                *stage = JobStage::Ordinary;
                Err(error)
            }
        }
    }

    async fn atomic_upload_destination(&self, path: &str) -> Result<Option<FileAttributes>> {
        match self.raw.lstat(path).await {
            Ok(reply) => {
                ensure!(
                    EntryKind::from_permissions(reply.attrs.permissions) == Some(EntryKind::File),
                    "The remote destination is not a regular file"
                );
                Ok(Some(reply.attrs))
            }
            Err(Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}
