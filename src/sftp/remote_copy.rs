use super::*;
use russh_sftp::protocol::Packet;

const COPY_DATA: &str = "copy-data";

impl Engine {
    pub(super) async fn copy_entry(
        &self,
        from: &str,
        directory: &str,
        stage: &mut JobStage,
    ) -> Result<Value> {
        let (folder, destination) = self.cross_folder_paths(from, directory, false).await?;
        self.require_destination_directory(&folder).await?;
        self.require_new_name(&destination).await?;
        let before = self.copy_source_attributes(from).await?;
        let source = self
            .raw
            .open(from, OpenFlags::READ, FileAttributes::empty())
            .await?
            .handle;
        let temporary = format!(
            "{}/.w11term-copy-{:032x}.tmp",
            folder.trim_end_matches('/'),
            rand::random::<u128>(),
        );

        let work = async {
            verify_source(&before, &self.copy_source_attributes(from).await?)?;
            let opened = self.copy_handle_attributes(&source).await?;
            if let Some(attrs) = &opened {
                verify_source(&before, attrs)?;
            }
            let size = before
                .size
                .or_else(|| opened.as_ref().and_then(|attrs| attrs.size));
            ensure!(
                self.canonical_remote_path(&folder).await? == folder,
                "The destination folder changed before copy"
            );
            let bytes = self
                .stage_remote_copy(&source, &temporary, size, stage)
                .await?;
            if let Some(after) = self.copy_handle_attributes(&source).await? {
                verify_source(opened.as_ref().unwrap_or(&before), &after)?;
                ensure!(
                    after.size.is_none_or(|size| size == bytes),
                    "The source changed during copy"
                );
            }
            Ok::<_, anyhow::Error>(bytes)
        }
        .await;
        let source_closed = self.raw.close(source).await;
        let copied = async {
            source_closed?;
            let copied = work?;
            let after = self.copy_source_attributes(from).await?;
            verify_source(&before, &after)?;
            ensure!(
                after.size.is_none_or(|size| size == copied),
                "The source changed during copy"
            );
            ensure!(
                self.canonical_remote_path(&folder).await? == folder,
                "The destination folder changed during copy"
            );
            self.require_new_name(&destination).await?;
            if let JobStage::StagedCopy { publishing, .. } = stage {
                *publishing = true;
            }
            self.raw.rename(&temporary, &destination).await?;
            Ok::<_, anyhow::Error>(copied)
        }
        .await;

        match copied {
            Ok(bytes) => {
                *stage = JobStage::Ordinary;
                Ok(json!({"directory":folder,"bytes_copied":bytes}))
            }
            Err(error)
                if matches!(stage, JobStage::StagedCopy { .. })
                    && !error.downcast_ref::<Error>().is_some_and(transport_error) =>
            {
                let removed = self.raw.remove(&temporary).await;
                if let Err(cleanup) = removed {
                    return Err(anyhow::Error::from(cleanup).context(format!(
                        "Copy failed ({error:#}); could not remove temporary file {temporary}"
                    )));
                }
                *stage = JobStage::Ordinary;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    async fn copy_source_attributes(&self, path: &str) -> Result<FileAttributes> {
        let attrs = self.raw.lstat(path).await?.attrs;
        ensure!(
            EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::File),
            "Copy requires a regular source file"
        );
        Ok(attrs)
    }

    async fn copy_handle_attributes(&self, handle: &str) -> Result<Option<FileAttributes>> {
        match self.raw.fstat(handle).await {
            Ok(reply) => Ok(Some(reply.attrs)),
            Err(Error::Status(status))
                if matches!(
                    status.status_code,
                    StatusCode::OpUnsupported | StatusCode::PermissionDenied
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn stage_remote_copy(
        &self,
        source: &str,
        temporary: &str,
        size: Option<u64>,
        stage: &mut JobStage,
    ) -> Result<u64> {
        *stage = JobStage::StagedCopy {
            temporary: temporary.into(),
            publishing: false,
        };
        let output = match self
            .raw
            .open(
                temporary,
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
                // A failed exclusive OPEN must not give us ownership of an existing file.
                if !transport_error(&error) {
                    *stage = JobStage::Checking("Copy was not started");
                }
                return Err(error.into());
            }
        };
        let copied = async {
            let fast_size = match size {
                Some(length) if length > 0 && self.extension(COPY_DATA, "1").is_ok() => self
                    .copy_data(source, &output, length)
                    .await?
                    .then_some(length),
                _ => None,
            };
            let bytes = match fast_size {
                Some(length) => length,
                None => self.relay_file_chunks(source, &output, size).await?,
            };
            if self.extension(extensions::FSYNC, "1").is_ok() {
                self.raw.fsync(&output).await?;
            }
            Ok::<_, anyhow::Error>(bytes)
        }
        .await;
        let closed = self.raw.close(output).await;
        closed?;
        let bytes = copied?;
        let attributes = self.raw.lstat(temporary).await?.attrs;
        ensure!(
            EntryKind::from_permissions(attributes.permissions) == Some(EntryKind::File)
                && attributes.size.is_none_or(|size| size == bytes),
            "The server did not stage a complete copy"
        );
        Ok(bytes)
    }

    async fn copy_data(&self, source: &str, output: &str, length: u64) -> Result<bool> {
        let payload = russh_sftp::ser::to_bytes(&(source, 0u64, length, output, 0u64))?;
        let reply = match self.raw.extended(COPY_DATA, payload.to_vec()).await {
            Ok(Packet::Status(status)) => status,
            Ok(_) => return Err(Error::UnexpectedPacket.into()),
            Err(Error::Status(status)) => status,
            Err(error) => return Err(error.into()),
        };
        match reply.status_code {
            StatusCode::Ok => Ok(true),
            StatusCode::OpUnsupported => Ok(false),
            _ => Err(Error::Status(reply).into()),
        }
    }

    async fn relay_file_chunks(
        &self,
        source: &str,
        output: &str,
        expected: Option<u64>,
    ) -> Result<u64> {
        let mut offset = 0u64;
        loop {
            let requested = expected.map_or(self.read_size as u64, |size| {
                (size - offset).saturating_add(1).min(self.read_size as u64)
            }) as u32;
            let data = match self.raw.read(source, offset, requested).await {
                Ok(reply) => reply.data,
                Err(error) if eof(&error) => break,
                Err(error) => return Err(error.into()),
            };
            ensure!(
                !data.is_empty() && data.len() <= requested as usize,
                "The server returned an invalid SFTP data length"
            );
            let end = offset
                .checked_add(data.len() as u64)
                .context("Remote file size overflow")?;
            ensure!(
                expected.is_none_or(|size| end <= size),
                "The source grew during copy"
            );
            for chunk in data.chunks(self.write_size) {
                self.raw.write(output, offset, chunk.to_vec()).await?;
                offset += chunk.len() as u64;
            }
        }
        ensure!(
            expected.is_none_or(|size| size == offset),
            "The source shrank during copy"
        );
        Ok(offset)
    }
}

fn verify_source(before: &FileAttributes, after: &FileAttributes) -> Result<()> {
    ensure!(
        after.permissions.is_none_or(|mode| {
            mode & 0o170000 == 0 || EntryKind::from_permissions(Some(mode)) == Some(EntryKind::File)
        }) && before.size.zip(after.size).is_none_or(|(a, b)| a == b)
            && before.mtime.zip(after.mtime).is_none_or(|(a, b)| a == b),
        "The source changed during copy"
    );
    Ok(())
}
