use super::*;
use std::cmp::Ordering as CmpOrdering;
use std::collections::BTreeSet;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use windows::{
    Win32::Globalization::{CSTR_EQUAL, CSTR_LESS_THAN, CompareStringOrdinal},
    Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ, MOVE_FILE_FLAGS, MoveFileExW,
    },
    core::HSTRING,
};

pub(super) const MAX_DEPTH: usize = 32;
pub(super) const MAX_ENTRIES: usize = 65_536;
pub(super) const MAX_NAME_BYTES: usize = 8 * 1024 * 1024;

pub fn validate_download_name(name: &str) -> Result<()> {
    ensure!(
        valid_component(name),
        "not a valid Windows filename: {name:?}"
    );
    Ok(())
}

// Use Windows' filename comparison even in an explicitly case-sensitive directory.
#[derive(Eq)]
struct WindowsName(Vec<u16>);

impl Ord for WindowsName {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // Validated, nonempty components contain at most 255 UTF-16 units.
        match unsafe { CompareStringOrdinal(&self.0, &other.0, true) } {
            CSTR_LESS_THAN => CmpOrdering::Less,
            CSTR_EQUAL => CmpOrdering::Equal,
            _ => CmpOrdering::Greater,
        }
    }
}
impl PartialOrd for WindowsName {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for WindowsName {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == CmpOrdering::Equal
    }
}

// Open the directory itself and deny delete sharing while we use its children.
pub(super) fn pin_directory(path: &Path, sharing: FILE_SHARE_MODE) -> Result<std::fs::File> {
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(sharing.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)?;
    let metadata = dir.metadata()?;
    ensure!(
        metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0,
        "staged directory changed during download"
    );
    Ok(dir)
}

pub(super) fn clean_up(
    stage: tempfile::TempDir,
    sender: oneshot::Sender<JobResult>,
    result: JobResult,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let outcome = match stage.close() {
            Ok(()) => result,
            Err(error) => Err(format!(
                "{}; could not remove the staged folder: {error}",
                result
                    .err()
                    .unwrap_or_else(|| "Folder download cancelled".into())
            )),
        };
        let _ = sender.send(outcome);
    })
}

fn new_staged_file(path: &Path) -> std::io::Result<std::fs::File> {
    // Tokio may still own a duplicate handle in its blocking pool when the
    // enclosing SFTP future is dropped during write/sync.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_DELETE.0)
        .open(path)
}

fn publish(stage: &tempfile::TempDir, destination: &Path) -> Result<()> {
    // No REPLACE_EXISTING or COPY_ALLOWED. Both names are siblings on the same volume.
    let result = unsafe {
        MoveFileExW(
            &HSTRING::from(stage.path().as_os_str()),
            &HSTRING::from(destination.as_os_str()),
            MOVE_FILE_FLAGS(0),
        )
    };
    if let Err(error) = result {
        ensure!(
            std::fs::symlink_metadata(destination).is_err(),
            "local destination already exists"
        );
        return Err(error).context("publishing downloaded directory");
    }
    Ok(())
}

#[derive(Default)]
struct TreeCount {
    entries: usize,
    name_bytes: usize,
    files: usize,
    directories: usize,
    skipped: usize,
    bytes: u64,
}

struct Download<'a> {
    engine: &'a Engine,
    progress: Option<&'a watch::Sender<TransferProgress>>,
    cancel: Option<&'a TransferCancel>,
    count: TreeCount,
}

impl Download<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.is_some_and(TransferCancel::is_cancelled)
    }

    async fn check_path(&self, path: &str) -> Result<()> {
        ensure!(
            self.engine.canonical_remote_path(path).await? == path,
            "remote path changed or resolves through a symlink: {path}"
        );
        Ok(())
    }

    async fn directory(&mut self, remote: &str, local: &Path, depth: usize) -> Result<()> {
        ensure!(
            depth <= MAX_DEPTH,
            "folder download exceeds {MAX_DEPTH} levels"
        );
        if self.cancelled() {
            return Ok(());
        }
        let _pin = pin_directory(local, FILE_SHARE_READ)?;
        let attrs = self.engine.raw.lstat(remote).await?.attrs;
        ensure!(
            EntryKind::from_permissions(attrs.permissions) == Some(EntryKind::Directory),
            "expected a real remote directory: {remote}"
        );
        if self.cancelled() {
            return Ok(());
        }
        self.check_path(remote).await?;
        if self.cancelled() {
            return Ok(());
        }
        let h = self.engine.raw.opendir(remote).await?.handle;
        let work = async {
            // SFTP v3 lacks openat/O_NOFOLLOW; verify the resolved name again
            // after OPEN(DIR) and before using that handle.
            self.check_path(remote).await?;
            let mut seen = BTreeSet::new();
            while !self.cancelled() {
                let entries = match self.engine.raw.readdir(&h).await {
                    Ok(reply) => reply.files,
                    Err(e) if eof(&e) => break,
                    Err(e) => return Err(e.into()),
                };
                ensure!(!entries.is_empty(), "empty SFTP directory page without EOF");
                self.count.entries = self
                    .count
                    .entries
                    .checked_add(entries.len())
                    .context("entry count overflow")?;
                ensure!(
                    self.count.entries <= MAX_ENTRIES,
                    "folder download exceeds {MAX_ENTRIES} directory entries"
                );
                for entry in entries {
                    if self.cancelled() {
                        return Ok(());
                    }
                    let name = entry.filename;
                    if matches!(name.as_str(), "." | "..") {
                        continue;
                    }
                    validate_download_name(&name)?;
                    self.count.name_bytes += name.len();
                    ensure!(
                        self.count.name_bytes <= MAX_NAME_BYTES,
                        "folder download filenames exceed 8 MiB"
                    );
                    ensure!(
                        seen.insert(WindowsName(name.encode_utf16().collect())),
                        "duplicate or colliding Windows filename: {name:?}"
                    );
                    let child = format!("{}/{name}", remote.trim_end_matches('/'));
                    let path = local.join(&name);
                    let attrs = self.engine.raw.lstat(&child).await?.attrs;
                    let mode = attrs
                        .permissions
                        .context("server omitted the file type from LSTAT")?
                        & 0o170000;
                    let listed = entry.attrs.permissions.unwrap_or(0) & 0o170000;
                    ensure!(
                        listed == 0 || listed == mode,
                        "remote entry type changed: {child}"
                    );
                    if self.cancelled() {
                        return Ok(());
                    }
                    match mode {
                        0o040000 => {
                            std::fs::create_dir(&path)
                                .with_context(|| format!("creating staged directory {name:?}"))?;
                            Box::pin(self.directory(&child, &path, depth + 1)).await?;
                            self.count.directories += 1;
                        }
                        0o100000 => {
                            ensure!(
                                attrs.size.zip(entry.attrs.size).is_none_or(|(a, b)| a == b),
                                "remote file size changed since directory listing: {child}"
                            );
                            self.file(&child, &path, attrs.size.or(entry.attrs.size))
                                .await?;
                        }
                        0 => bail!("server omitted the file type from LSTAT"),
                        _ => self.count.skipped += 1,
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        let closed = self.engine.raw.close(h).await;
        work?;
        closed?;
        Ok(())
    }

    async fn file(&mut self, remote: &str, local: &Path, listed_size: Option<u64>) -> Result<()> {
        self.check_path(remote).await?;
        if self.cancelled() {
            return Ok(());
        }
        let h = self
            .engine
            .raw
            .open(remote, OpenFlags::READ, FileAttributes::empty())
            .await?
            .handle;
        let work = async {
            if self.cancelled() {
                return Ok(());
            }
            self.check_path(remote).await?;
            let opened = match self.engine.raw.fstat(&h).await {
                Ok(reply) => Some(reply.attrs),
                Err(Error::Status(s))
                    if matches!(
                        s.status_code,
                        StatusCode::OpUnsupported | StatusCode::PermissionDenied
                    ) =>
                {
                    None
                }
                Err(e) => return Err(e.into()),
            };
            if let Some(mode) = opened.as_ref().and_then(|attrs| attrs.permissions) {
                ensure!(
                    matches!(mode & 0o170000, 0 | 0o100000),
                    "remote file type changed after open: {remote}"
                );
            }
            let size = opened.and_then(|attrs| attrs.size);
            ensure!(
                listed_size.zip(size).is_none_or(|(a, b)| a == b),
                "remote file size changed before download: {remote}"
            );
            let expected = listed_size.or(size);
            if self.cancelled() {
                return Ok(());
            }
            let mut file = tokio::fs::File::from_std(new_staged_file(local)?);
            let base = self.count.bytes;
            let read = self
                .engine
                .read_contents(
                    &h,
                    &mut file,
                    expected.map(|n| (n, "reported file size")),
                    self.cancel,
                    |n, _| {
                        self.count.bytes = base.checked_add(n).context("folder size overflow")?;
                        report(self.progress, self.count.bytes, None);
                        Ok(())
                    },
                )
                .await?;
            if !self.cancelled() {
                if let Some(expected) = expected {
                    ensure!(
                        read == expected,
                        "remote file ended after {read} of {expected} bytes: {remote}"
                    );
                }
                file.sync_all().await?;
                drop(file);
                let staged = local.to_path_buf();
                tokio::task::spawn_blocking(move || super::apply_mark_of_the_web(&staged))
                    .await??;
                self.count.files += 1;
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        let closed = self.engine.raw.close(h).await;
        work?;
        closed?;
        Ok(())
    }
}

impl Engine {
    pub(super) async fn canonical_remote_path(&self, path: &str) -> Result<String> {
        let entries = self.raw.realpath(path).await?.files;
        let [entry] = entries.as_slice() else {
            bail!("invalid SFTP REALPATH reply")
        };
        let name = &entry.filename;
        ensure!(
            name.starts_with('/')
                && !name.contains(['\0', '\\'])
                && (name == "/" || !name[1..].split('/').any(|p| matches!(p, "" | "." | ".."))),
            "SFTP did not return a canonical absolute path"
        );
        Ok(name.clone())
    }
    pub(super) async fn get_dir(
        &self,
        remote: String,
        local: PathBuf,
        progress: Option<&watch::Sender<TransferProgress>>,
        cancel: Option<&TransferCancel>,
        staging: &mut Option<tempfile::TempDir>,
        deadline: Instant,
    ) -> Result<Value> {
        let mut download = Download {
            engine: self,
            progress,
            cancel,
            count: TreeCount::default(),
        };
        ensure!(local.is_absolute(), "local path must be absolute");
        let leaf = local
            .file_name()
            .and_then(|s| s.to_str())
            .context("local destination requires a folder name")?;
        validate_download_name(leaf)?;
        let parent =
            tokio::fs::canonicalize(local.parent().context("local destination has no parent")?)
                .await?;
        let destination = parent.join(leaf);
        match tokio::fs::symlink_metadata(&destination).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => bail!("local destination already exists"),
            Err(e) => return Err(e.into()),
        }
        ensure!(
            !remote.is_empty() && !remote.contains(['\0', '\\']),
            "invalid remote folder path"
        );
        let mut source = remote.trim_end_matches('/');
        while let Some(base) = source.strip_suffix("/.") {
            source = base.trim_end_matches('/')
        }
        let source = if source.is_empty() { "/" } else { source };
        // Resolve the containing directory, not a possibly substituted final link.
        let (parent_remote, name) = source.rsplit_once('/').unwrap_or((".", source));
        let root = if matches!(name, "" | "." | "..") {
            self.canonical_remote_path(source).await?
        } else {
            let parent = self
                .canonical_remote_path(if parent_remote.is_empty() {
                    "/"
                } else {
                    parent_remote
                })
                .await?;
            format!("{}/{name}", parent.trim_end_matches('/'))
        };
        report(progress, 0, None);
        let stage = staging.insert(
            tempfile::Builder::new()
                .prefix(".w11term-download-")
                .tempdir_in(&parent)?,
        );
        download.count.directories = 1;
        download.directory(&root, stage.path(), 0).await?;
        let count = download.count;
        if cancel.is_some_and(TransferCancel::is_cancelled) {
            return Ok(cancelled_transfer(&local, &remote, count.bytes, false));
        }
        // Keep the no-replace commit in this future; an expired/disconnected job
        // must never leave a detached blocking task that publishes it later.
        ensure!(
            Instant::now() < deadline,
            "folder download expired before publication"
        );
        publish(stage, &destination)?;
        let _ = staging.take().unwrap().keep();
        Ok(
            json!({"local":local,"remote":remote,"files":count.files,"directories":count.directories,
            "skipped":count.skipped,"bytes_transferred":count.bytes}),
        )
    }
}

pub(crate) fn valid_component(name: &str) -> bool {
    if name.is_empty()
        || name.ends_with(['.', ' '])
        || name.encode_utf16().count() > 255
        || name
            .chars()
            .any(|c| c.is_control() || r#"\/:"<>|?*"#.contains(c))
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) && !stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn interrupted_download_cleans_up_even_while_a_staged_write_handle_survives() {
        let parent = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir_in(parent.path()).unwrap();
        let path = stage.path().join("sub");
        std::fs::create_dir(&path).unwrap();
        let mut disk_handle = new_staged_file(&path.join("data")).unwrap();
        disk_handle.write_all(b"partial download").unwrap();
        let (sender, rx) = oneshot::channel();
        let pending = PendingReply {
            sender: Some(sender),
            stage: JobStage::DirectoryDownload {
                staging: Some(stage),
                deadline: Instant::now(),
            },
        };
        drop(pending);
        let reply = rx.await.unwrap().unwrap_err();
        assert_eq!(reply, "Folder download interrupted before publication");
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
        drop(disk_handle);
    }
}
