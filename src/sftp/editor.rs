use super::*;
use russh_sftp::protocol::Packet;

const POSIX_RENAME: &str = "posix-rename@openssh.com";

impl Engine {
    pub(super) async fn save_text(
        &self,
        path: &str,
        original: Option<&str>,
        text: &str,
        stage: &mut JobStage,
    ) -> Result<()> {
        ensure!(
            text.len() <= TEXT_PREVIEW_MAX_BYTES && !text.contains('\0'),
            "Save supports UTF-8 text up to 256 KiB without NUL bytes"
        );
        let (parent, leaf) = path
            .rsplit_once('/')
            .context("Expected an absolute remote file path")?;
        ensure!(
            path.starts_with('/') && !path.contains('\0') && !matches!(leaf, "" | "." | ".."),
            "Expected an absolute remote file path"
        );
        let attributes = if let Some(original) = original {
            self.extension(POSIX_RENAME, "1")
                .context("This server cannot replace a file atomically; use Save as")?;
            self.check_original(path, original).await?;
            Some(self.editor_attributes(path).await?)
        } else {
            self.require_new_name(path).await?;
            None
        };

        let temporary = format!("{parent}/.w11term-edit-{:032x}.tmp", rand::random::<u128>());
        *stage = JobStage::EditorSave {
            temporary: temporary.clone(),
            publishing: false,
        };
        let handle = self
            .raw
            .open(
                &temporary,
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                FileAttributes {
                    permissions: Some(0o600),
                    ..FileAttributes::empty()
                },
            )
            .await?
            .handle;
        let prepared = self
            .write_editor_file(&handle, text, attributes.as_ref())
            .await;
        let closed = self.raw.close(handle).await;
        let published: Result<()> = async {
            closed?;
            prepared?;
            if let (Some(original), Some(attributes)) = (original, attributes) {
                // A same-length change can happen inside one SFTP timestamp tick.
                // Compare the actual file immediately before dispatching the rename.
                self.check_original(path, original).await?;
                let current = self.editor_attributes(path).await?;
                ensure!(current.uid == attributes.uid && current.gid == attributes.gid
                    && current.permissions == attributes.permissions,
                    "Remote file permissions or ownership changed; save your draft under another name");
            } else {
                self.require_new_name(path).await?;
            }
            if let JobStage::EditorSave { publishing, .. } = stage { *publishing = true; }
            if original.is_some() {
                let payload = russh_sftp::ser::to_bytes(&(&temporary, path))?;
                match self.raw.extended(POSIX_RENAME, payload.to_vec()).await? {
                    Packet::Status(status) if status.status_code == StatusCode::Ok => {}
                    Packet::Status(status) => return Err(Error::Status(status).into()),
                    _ => return Err(Error::UnexpectedPacket.into()),
                }
            } else {
                // Standard SFTP v3 RENAME requires that the new name is absent.
                self.raw.rename(&temporary, path).await?;
            }
            Ok(())
        }.await;
        if let Err(error) = &published {
            if error.downcast_ref::<Error>().is_some_and(transport_error) {
                // A lost acknowledgement cannot establish who now owns either path.
                return published;
            }
            self.raw.remove(&temporary).await.with_context(|| {
                format!("Save failed ({error:#}); could not remove temporary file {temporary}")
            })?;
        }
        *stage = JobStage::Ordinary;
        published
    }

    async fn require_new_name(&self, path: &str) -> Result<()> {
        match self.raw.lstat(path).await {
            Err(Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => Ok(()),
            Err(error) => Err(error.into()),
            Ok(_) => bail!("An entry already exists at {path}; choose another name"),
        }
    }

    async fn check_original(&self, path: &str, original: &str) -> Result<()> {
        ensure!(
            self.read_preview_text(path, None).await? == original,
            "The remote file changed since it was opened; save your draft under another name"
        );
        Ok(())
    }

    async fn editor_attributes(&self, path: &str) -> Result<FileAttributes> {
        let attrs = self.raw.lstat(path).await?.attrs;
        ensure!(
            EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::File),
            "The remote entry is no longer a regular file"
        );
        Ok(attrs)
    }

    async fn write_editor_file(
        &self,
        handle: &str,
        text: &str,
        attrs: Option<&FileAttributes>,
    ) -> Result<()> {
        let mut offset = 0;
        for chunk in text.as_bytes().chunks(self.write_size) {
            self.raw.write(handle, offset, chunk.to_vec()).await?;
            offset += chunk.len() as u64;
        }
        if let Some(original) = attrs {
            // Apply ownership before mode: chown can clear set-id bits.
            let temp = self.raw.fstat(handle).await?.attrs;
            if let (Some(uid), Some(gid)) = (original.uid, original.gid)
                && (temp.uid != Some(uid) || temp.gid != Some(gid))
            {
                self.raw
                    .fsetstat(
                        handle,
                        FileAttributes {
                            uid: Some(uid),
                            gid: Some(gid),
                            ..FileAttributes::empty()
                        },
                    )
                    .await
                    .context("Preserving remote file ownership")?;
            }
            let permissions = original.permissions.map(|mode| mode & 0o7777);
            self.raw
                .fsetstat(
                    handle,
                    FileAttributes {
                        permissions,
                        ..FileAttributes::empty()
                    },
                )
                .await
                .context("Preserving remote file permissions")?;
        }
        if self.extension(extensions::FSYNC, "1").is_ok() {
            self.raw.fsync(handle).await?;
        }
        Ok(())
    }
}
