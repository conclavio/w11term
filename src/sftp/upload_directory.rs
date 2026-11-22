use super::*;
use directory::{MAX_DEPTH, MAX_ENTRIES, MAX_NAME_BYTES};
use std::{
    fs::File,
    io,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Component, Prefix},
};
use windows::{
    Wdk::Storage::FileSystem::{FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT},
    Win32::{
        Foundation::{
            HANDLE, RtlNtStatusToDosError, STATUS_IO_REPARSE_TAG_NOT_HANDLED,
            STATUS_REPARSE_POINT_ENCOUNTERED, STATUS_STOPPED_ON_SYMLINK,
        },
        Storage::FileSystem::*,
    },
};

pub(super) struct UploadState {
    remote: String,
    resolved_remote: Option<String>,
    pub(super) root_dispatched: bool,
    deadline: Instant,
    bytes: u64,
    files: u64,
    directories: usize,
    skipped: usize,
}

impl UploadState {
    pub(super) fn new(remote: String, deadline: Instant) -> Self {
        Self {
            remote,
            resolved_remote: None,
            root_dispatched: false,
            deadline,
            bytes: 0,
            files: 0,
            directories: 0,
            skipped: 0,
        }
    }

    pub(super) fn interrupted(&self) -> String {
        let remote = self.resolved_remote.as_deref().unwrap_or(&self.remote);
        if self.root_dispatched {
            format!(
                "Folder upload interrupted after {} confirmed bytes and {} complete files; remote destination {remote} may contain a partial folder",
                self.bytes, self.files
            )
        } else {
            format!("Folder upload interrupted before creating the remote destination {remote}")
        }
    }

    fn result(&self, local: &Path) -> Value {
        let mut reply = json!({"local":local,"remote":self.remote,"files":self.files,"directories":self.directories,
            "skipped":self.skipped,"bytes_transferred":self.bytes});
        if let Some(root) = &self.resolved_remote {
            reply["resolved_remote"] = json!(root);
        }
        reply
    }

    pub(super) fn cancelled(&self, local: &Path) -> Value {
        let mut value = self.result(local);
        value["cancelled"] = true.into();
        value["remote_may_be_partial"] = self.root_dispatched.into();
        value
    }
}

fn validate_component(name: &str) -> Result<()> {
    ensure!(
        !matches!(name, "" | "." | "..")
            && !name.contains(['/', '\\', ':', '\0'])
            && name.encode_utf16().count() <= 255,
        "invalid local folder entry: {name:?}"
    );
    Ok(())
}

enum Source {
    Directory(Arc<File>),
    File(File, u64),
    Skipped,
}

fn classify(file: File) -> Result<Source> {
    if unsafe { GetFileType(HANDLE(file.as_raw_handle())) } != FILE_TYPE_DISK {
        return Ok(Source::Skipped);
    }
    let meta = file.metadata()?;
    if meta.file_attributes() & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DEVICE).0 != 0 {
        Ok(Source::Skipped)
    } else if meta.is_dir() {
        Ok(Source::Directory(Arc::new(file)))
    } else if meta.is_file() {
        Ok(Source::File(file, meta.len()))
    } else {
        Ok(Source::Skipped)
    }
}

fn open_child(parent: &File, name: &str) -> Result<Source> {
    validate_component(name)?;
    // Each lookup uses the directory object we enumerated. The final file handle
    // denies writes and renames until all its bytes have been read from that handle.
    let file = match crate::windows_dirs::open_relative(
        parent,
        name,
        FILE_READ_DATA,
        FILE_SHARE_READ,
        FILE_OPEN,
        FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
    ) {
        Ok(file) => file,
        Err(
            STATUS_REPARSE_POINT_ENCOUNTERED
            | STATUS_STOPPED_ON_SYMLINK
            | STATUS_IO_REPARSE_TAG_NOT_HANDLED,
        ) => return Ok(Source::Skipped),
        Err(status) => {
            return Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(status) } as i32
            )
            .into());
        }
    };
    classify(file)
}

fn source_root(local: &Path) -> Result<Arc<File>> {
    ensure!(local.is_absolute(), "local path must be absolute");
    let mut components = local.components();
    ensure!(
        matches!(components.next(), Some(Component::Prefix(p)) if matches!(p.kind(),
        Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _))),
        "expected a drive or UNC source folder"
    );
    ensure!(
        components.next() == Some(Component::RootDir),
        "expected a rooted source folder"
    );
    for entry in components {
        let Component::Normal(name) = entry else {
            bail!("invalid local source path component");
        };
        validate_component(
            name.to_str()
                .context("local source path is not valid Unicode")?,
        )?;
    }
    // Ancestors follow normal picker/Windows path semantics (including mounted
    // volumes). Open the selected root without following its final component,
    // then never resolve this absolute pathname again.
    // A trailing separator must not make a selected junction an intermediate
    // component of the Win32 open (and therefore follow its target).
    let selected = local
        .parent()
        .zip(local.file_name())
        .map(|(parent, name)| parent.join(name));
    let root = std::fs::OpenOptions::new()
        .access_mode((FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE).0)
        .share_mode(FILE_SHARE_READ.0)
        .custom_flags((FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT).0)
        .open(selected.as_deref().unwrap_or(local))
        .context("opening local source folder")?;
    match classify(root)? {
        Source::Directory(dir) => Ok(dir),
        _ => bail!("local source must be a real directory, not a reparse point"),
    }
}

// OpenSSH servers on Windows use either C:/ or /C:/ as the absolute root.
fn drive_name(name: &str) -> bool {
    matches!(name.as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic())
}

fn validate_canonical(name: &str) -> Result<()> {
    let without_slash = name.strip_prefix('/').unwrap_or(name);
    let components = if let Some((drive, tail)) = without_slash.split_once('/')
        && drive_name(drive)
    {
        tail
    } else {
        name.strip_prefix('/')
            .context("SFTP did not return a canonical absolute path")?
    };
    ensure!(
        !name.contains(['\0', '\\'])
            && (components.is_empty()
                || components.split('/').all(|n| !matches!(n, "" | "." | ".."))),
        "invalid SFTP canonical path"
    );
    Ok(())
}

fn destination_parts(remote: &str) -> Result<(&str, &str)> {
    let target = remote.trim_end_matches('/');
    ensure!(
        !target.is_empty() && !remote.contains("//") && !remote.contains(['\0', '\\']),
        "invalid remote destination folder"
    );
    ensure!(
        !drive_name(target.strip_prefix('/').unwrap_or(target)),
        "remote destination requires a new folder name"
    );
    let (parent, leaf) = target.rsplit_once('/').unwrap_or((".", target));
    ensure!(
        !matches!(leaf, "." | ".."),
        "remote destination requires a new folder name"
    );
    let parent = if parent.is_empty() {
        "/"
    } else if drive_name(parent.strip_prefix('/').unwrap_or(parent)) {
        &target[..parent.len() + 1]
    } else {
        parent
    };
    Ok((parent, leaf))
}

struct Upload<'a> {
    engine: &'a Engine,
    state: &'a mut UploadState,
    cancel: Option<&'a TransferCancel>,
    progress: Option<&'a watch::Sender<TransferProgress>>,
    entries: usize,
    name_bytes: usize,
}

impl Upload<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.is_some_and(TransferCancel::is_cancelled)
    }

    fn progress(&self) {
        if let Some(sender) = self.progress {
            sender.send_replace(TransferProgress {
                bytes: self.state.bytes,
                total: None,
                files: Some(self.state.files),
            });
        }
    }

    async fn canonical(&self, path: &str) -> Result<String> {
        let entries = self.engine.raw.realpath(path).await?.files;
        let [entry] = entries.as_slice() else {
            bail!("invalid SFTP REALPATH reply");
        };
        validate_canonical(&entry.filename)?;
        Ok(entry.filename.clone())
    }

    async fn check_path(&self, path: &str) -> Result<()> {
        ensure!(
            self.canonical(path).await? == path,
            "remote path changed or resolves through a symlink: {path}"
        );
        Ok(())
    }

    async fn list(&mut self, dir: Arc<File>) -> Result<Vec<String>> {
        let entry_limit = MAX_ENTRIES - self.entries;
        let names_limit = MAX_NAME_BYTES - self.name_bytes;
        let deadline = self.state.deadline;
        let cancel = self.cancel.cloned();
        let (names, bytes) = tokio::task::spawn_blocking(move || {
            let mut names = Vec::new();
            let mut bytes = 0;
            crate::windows_dirs::names(&dir, |units| {
                ensure!(Instant::now() < deadline, "folder upload expired");
                ensure!(
                    !cancel.as_ref().is_some_and(TransferCancel::is_cancelled),
                    "folder upload cancelled"
                );
                ensure!(
                    names.len() < entry_limit,
                    "folder upload exceeds {MAX_ENTRIES} directory entries"
                );
                let name =
                    String::from_utf16(units).context("local filename is not valid Unicode")?;
                validate_component(&name)?;
                bytes += name.len();
                ensure!(bytes <= names_limit, "folder upload filenames exceed 8 MiB");
                names.push(name);
                Ok(())
            })?;
            Ok::<_, anyhow::Error>((names, bytes))
        })
        .await??;
        self.entries += names.len();
        self.name_bytes += bytes;
        Ok(names)
    }

    async fn directory(&mut self, dir: Arc<File>, remote: &str, depth: usize) -> Result<()> {
        let names = match self.list(dir.clone()).await {
            Err(_) if self.cancelled() => return Ok(()),
            result => result?,
        };
        for name in names {
            if self.cancelled() {
                break;
            }
            let parent = dir.clone();
            let path = format!("{remote}/{name}");
            let source = tokio::task::spawn_blocking(move || open_child(&parent, &name))
                .await?
                .with_context(|| format!("opening local source for {path}"))?;
            if self.cancelled() {
                break;
            }
            if matches!(source, Source::Skipped) {
                self.state.skipped += 1;
                continue;
            }
            self.check_path(remote).await?;
            if self.cancelled() {
                break;
            }
            match source {
                Source::Directory(child) => {
                    ensure!(
                        depth < MAX_DEPTH,
                        "folder upload exceeds {MAX_DEPTH} levels"
                    );
                    self.engine
                        .raw
                        .mkdir(&path, FileAttributes::empty())
                        .await
                        .with_context(|| format!("creating remote directory {path}"))?;
                    self.state.directories += 1;
                    self.check_path(&path).await?;
                    Box::pin(self.directory(child, &path, depth + 1)).await?;
                }
                Source::File(file, len) => self.file(file, len, &path).await?,
                Source::Skipped => unreachable!(),
            }
        }
        Ok(())
    }

    async fn file(&mut self, source: File, len: u64, path: &str) -> Result<()> {
        self.state
            .bytes
            .checked_add(len)
            .context("folder upload size overflow")?;
        let mut file = tokio::fs::File::from_std(source);
        let h = self
            .engine
            .raw
            .open(
                path,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
                FileAttributes::empty(),
            )
            .await
            .with_context(|| format!("creating remote file {path}"))?
            .handle;
        let work = async {
            self.check_path(path).await?;
            let mut buf = vec![0; self.engine.write_size];
            let mut offset = 0;
            while offset < len && !self.cancelled() {
                let limit = (len - offset).min(buf.len() as u64) as usize;
                let n = file.read(&mut buf[..limit]).await?;
                ensure!(n != 0, "local file changed while uploading {path}");
                if self.cancelled() {
                    break;
                }
                self.engine
                    .raw
                    .write(&h, offset, buf[..n].to_vec())
                    .await
                    .with_context(|| format!("writing remote file {path}"))?;
                offset += n as u64;
                self.state.bytes += n as u64;
                self.progress();
            }
            if offset == len {
                ensure!(
                    file.metadata().await?.len() == len,
                    "local file changed while uploading {path}"
                );
            }
            Ok::<_, anyhow::Error>(offset == len)
        }
        .await;
        let close = self.engine.raw.close(h).await;
        let complete = work?;
        close.with_context(|| format!("closing remote file {path}"))?;
        if complete {
            self.state.files += 1;
            self.progress();
        }
        Ok(())
    }
}

impl Engine {
    pub(super) async fn put_directory(
        &self,
        local: PathBuf,
        progress: Option<&watch::Sender<TransferProgress>>,
        cancel: Option<&TransferCancel>,
        state: &mut UploadState,
    ) -> Result<Value> {
        let source = local.clone();
        let dir = tokio::task::spawn_blocking(move || source_root(&source)).await??;
        let remote = state.remote.clone();
        let (parent, leaf) = destination_parts(&remote)?;
        let mut upload = Upload {
            engine: self,
            state,
            cancel,
            progress,
            entries: 0,
            name_bytes: 0,
        };
        let parent = upload.canonical(parent).await?;
        let root = format!("{}/{leaf}", parent.trim_end_matches('/'));
        upload.state.resolved_remote = Some(root.clone());
        match self.raw.lstat(&root).await {
            Ok(_) => bail!("remote destination already exists: {remote}"),
            Err(Error::Status(s)) if s.status_code == StatusCode::NoSuchFile => {}
            Err(error) => return Err(error.into()),
        }
        upload.progress();
        if upload.cancelled() {
            return Ok(upload.state.cancelled(&local));
        }
        upload.check_path(&parent).await?;
        if upload.cancelled() {
            return Ok(upload.state.cancelled(&local));
        }
        upload.state.root_dispatched = true;
        if let Err(error) = self.raw.mkdir(&root, FileAttributes::empty()).await {
            if !transport_error(&error) {
                upload.state.root_dispatched = false;
                if self.raw.lstat(&root).await.is_ok() {
                    bail!("remote destination already exists: {remote}");
                }
            }
            return Err(error.into());
        }
        upload.state.directories = 1;
        upload.check_path(&root).await?;
        upload.directory(dir, &root, 0).await?;
        if upload.cancelled() {
            return Ok(upload.state.cancelled(&local));
        }
        Ok(upload.state.result(&local))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Read};

    #[test]
    fn source_handles_follow_chosen_mount_ancestors_and_skip_child_reparse_points() -> Result<()> {
        let temp = tempfile::tempdir()?;
        fs::create_dir_all(temp.path().join("actual").join("pack"))?;
        let pack = temp.path().join("actual").join("pack");
        fs::write(pack.join("data"), b"selected bytes")?;
        for (name, target) in [("mounted", "actual"), (r"actual\pack\child", "actual")] {
            let outcome = std::process::Command::new("cmd.exe")
                .current_dir(temp.path())
                .args(["/d", "/c", "mklink", "/J", name, target])
                .output()?;
            assert!(
                outcome.status.success(),
                "{}",
                String::from_utf8_lossy(&outcome.stderr)
            );
        }
        assert!(source_root(&temp.path().join("mounted")).is_err());
        assert!(source_root(&temp.path().join(r"mounted\")).is_err());
        let chosen = source_root(&temp.path().join("mounted").join("pack"))?;
        let mut names = Vec::new();
        crate::windows_dirs::names(&chosen, |units| {
            names.push(String::from_utf16(units)?);
            Ok(())
        })?;
        names.sort();
        assert_eq!(names, ["child", "data"]);
        assert!(matches!(open_child(&chosen, "child")?, Source::Skipped));
        let Source::File(mut file, len) = open_child(&chosen, "data")? else {
            panic!("expected file")
        };
        assert_eq!(len, 14);
        assert!(
            fs::OpenOptions::new()
                .write(true)
                .open(pack.join("data"))
                .is_err()
        );
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        assert_eq!(bytes, b"selected bytes");
        Ok(())
    }

    #[test]
    fn interrupted_tree_identifies_the_admitted_destination_after_a_parent_alias_changes() {
        let source = Path::new(r"C:\pack");
        let mut state = UploadState::new("current/new".into(), Instant::now());
        assert!(state.cancelled(source).get("resolved_remote").is_none());
        state.resolved_remote = Some("/projects/v1/new".into());
        state.root_dispatched = true;
        for reply in [state.cancelled(source), state.result(source)] {
            assert_eq!(reply["remote"], "current/new");
            assert_eq!(reply["resolved_remote"], "/projects/v1/new");
        }
        assert!(state.interrupted().contains("/projects/v1/new"));
        assert!(!state.interrupted().contains("current/new"));
    }

    #[test]
    fn exact_remote_destinations_accept_posix_and_both_windows_drive_roots() {
        for (path, parent, leaf) in [
            ("new", ".", "new"),
            ("sub/new", "sub", "new"),
            ("/new", "/", "new"),
            ("C:/new", "C:/", "new"),
            ("/C:/new/", "/C:/", "new"),
            ("/C:/sub/new", "/C:/sub", "new"),
            ("/release/a:", "/release", "a:"),
        ] {
            assert_eq!(destination_parts(path).unwrap(), (parent, leaf));
        }
        for path in ["", "/", ".", "..", "C:", "C:/", "/C:/", "/broken//path"] {
            assert!(destination_parts(path).is_err(), "{path:?}");
        }
        for path in [
            "/",
            "/release",
            "/a/b",
            "C:/",
            "/C:/",
            "C:/sub/file",
            "/C:/sub/file",
        ] {
            assert!(validate_canonical(path).is_ok(), "{path}");
        }
        for path in ["", "abc/path", "/a/../b", "/x/.", "/a//b", "C://", "/C://"] {
            assert!(validate_canonical(path).is_err(), "{path:?}");
        }
    }

    #[test]
    fn native_child_names_are_one_unicode_component() {
        for name in ["a", ".config", "報告 #2", "trailing."] {
            assert!(validate_component(name).is_ok());
        }
        for name in [
            "",
            ".",
            "..",
            "a/b",
            r"a\b",
            "a:stream",
            "x\0",
            &"界".repeat(256),
        ] {
            assert!(validate_component(name).is_err());
        }
    }
}
