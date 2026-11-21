//! SFTP v3 on a sibling channel of the pane's authenticated SSH connection.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use russh_sftp::{
    client::{RawSftpSession, error::Error, rawsession::Limits},
    extensions,
    protocol::{FileAttributes, OpenFlags, StatusCode, Version},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
    time::Instant,
};

pub const CHUNK: usize = 32 * 1024;
pub const DEFAULT_TIMEOUT: u64 = 600;
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Directory entry types the drawer can delete with the SFTP v3 primitives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
}

impl EntryKind {
    pub fn from_permissions(permissions: Option<u32>) -> Option<Self> {
        match permissions? & 0o170000 {
            0o100000 => Some(Self::File),
            0o040000 => Some(Self::Directory),
            0o120000 => Some(Self::Symlink),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Operation {
    Info,
    Get {
        remote: String,
        local: PathBuf,
        #[serde(default)]
        max_bytes: Option<u64>,
        #[serde(default)]
        overwrite: bool,
    },
    Put {
        local: PathBuf,
        remote: String,
        #[serde(default)]
        overwrite: bool,
    },
    Ls {
        path: String,
    },
    /// The absolute path verified by SSHFS can only be opened on the source pane.
    #[serde(skip)]
    ListMountedFolder {
        path: String,
    },
    Opendir {
        path: String,
    },
    Readdir {
        handle: u64,
    },
    Open {
        path: String,
        flags: Vec<String>,
        #[serde(default)]
        attrs: Attributes,
    },
    Close {
        handle: u64,
    },
    Read {
        handle: u64,
        offset: u64,
        len: u32,
    },
    Write {
        handle: u64,
        offset: u64,
        data_base64: String,
    },
    Stat {
        path: String,
    },
    Lstat {
        path: String,
    },
    Fstat {
        handle: u64,
    },
    Setstat {
        path: String,
        attrs: Attributes,
    },
    Fsetstat {
        handle: u64,
        attrs: Attributes,
    },
    Mkdir {
        path: String,
        #[serde(default)]
        attrs: Attributes,
    },
    Rename {
        from: String,
        to: String,
    },
    /// Interactive drawer approval, bound to the inspected connection.
    #[serde(skip)]
    RenameEntry {
        from: String,
        to: String,
    },
    /// Delete the selected entry with the type and SSH connection the user approved.
    #[serde(skip)]
    RemoveEntry {
        path: String,
        kind: EntryKind,
    },
    RemoveFile {
        path: String,
    },
    RemoveDir {
        path: String,
    },
    Realpath {
        path: String,
    },
    Readlink {
        path: String,
    },
    Symlink {
        path: String,
        target: String,
        #[serde(default)]
        spec_order: bool,
    },
    Hardlink {
        from: String,
        to: String,
    },
    Fsync {
        handle: u64,
    },
    Statvfs {
        path: String,
    },
    Limits,
    ExpandPath {
        path: String,
    },
}

/// Only fields supported by SFTP v3. Paired fields must be supplied together.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attributes {
    pub size: Option<u64>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub permissions: Option<u32>,
    pub atime: Option<u32>,
    pub mtime: Option<u32>,
}
impl Attributes {
    fn protocol(&self) -> Result<FileAttributes> {
        ensure!(
            self.uid.is_some() == self.gid.is_some(),
            "uid and gid must be supplied together"
        );
        ensure!(
            self.atime.is_some() == self.mtime.is_some(),
            "atime and mtime must be supplied together"
        );
        Ok(FileAttributes {
            size: self.size,
            uid: self.uid,
            gid: self.gid,
            permissions: self.permissions,
            atime: self.atime,
            mtime: self.mtime,
            ..FileAttributes::empty()
        })
    }
}

pub fn open_flags(flags: &[String]) -> Result<OpenFlags> {
    let mut result = OpenFlags::empty();
    for flag in flags {
        result |= match flag.as_str() {
            "read" => OpenFlags::READ,
            "write" => OpenFlags::WRITE,
            "append" => OpenFlags::APPEND,
            "create" => OpenFlags::CREATE,
            "truncate" => OpenFlags::TRUNCATE,
            "exclusive" => OpenFlags::EXCLUDE,
            _ => bail!("unknown open flag {flag}"),
        };
    }
    ensure!(
        result.intersects(OpenFlags::READ | OpenFlags::WRITE),
        "open requires read and/or write"
    );
    ensure!(
        !result.intersects(OpenFlags::APPEND | OpenFlags::TRUNCATE | OpenFlags::CREATE)
            || result.contains(OpenFlags::WRITE),
        "append/create/truncate require write"
    );
    ensure!(
        !result.contains(OpenFlags::EXCLUDE) || result.contains(OpenFlags::CREATE),
        "exclusive requires create"
    );
    Ok(result)
}

/// Bytes written to the local file or acknowledged by the remote server.
/// The total is a metadata estimate. Completion is reported by the job reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransferProgress {
    pub bytes: u64,
    pub total: Option<u64>,
}

fn report(progress: Option<&watch::Sender<TransferProgress>>, bytes: u64, total: Option<u64>) {
    if let Some(progress) = progress {
        progress.send_replace(TransferProgress {
            bytes,
            total: total.filter(|size| *size >= bytes),
        });
    }
}

type JobResult = Result<Value, String>;

#[derive(Debug)]
pub struct Job {
    /// A UI action approved against a particular live SSH connection.
    /// The worker owns that connection and rejects stale approvals before I/O.
    pub expected_generation: Option<u64>,
    pub operation: Operation,
    pub deadline: Instant,
    pub reply: oneshot::Sender<JobResult>,
    pub progress: Option<watch::Sender<TransferProgress>>,
}

// The SSH transport aborts its SFTP worker when the connection goes away.
// Preserve the uncertain outcome of an admitted request across that teardown.
struct PendingReply {
    sender: Option<oneshot::Sender<JobResult>>,
    stage: JobStage,
}

enum JobStage {
    Ordinary,
    Upload,
    Checking(&'static str),
    AwaitingStatus(&'static str),
}

impl JobStage {
    fn interrupted(&self) -> &'static str {
        match self {
            Self::Ordinary => "SFTP operation interrupted; outcome may be unknown",
            Self::Upload => {
                "SFTP upload interrupted; remote destination may contain a partial file"
            }
            Self::Checking(message) | Self::AwaitingStatus(message) => message,
        }
    }
}

impl PendingReply {
    fn finish(mut self, result: JobResult) -> Result<(), JobResult> {
        self.sender.take().unwrap().send(result)
    }
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Err(self.stage.interrupted().into()));
        }
    }
}

pub(crate) async fn run(
    session: Arc<russh::client::Handle<crate::ssh::Handler>>,
    generation: u64,
    mut jobs: mpsc::Receiver<Job>,
    mut cleanup: mpsc::UnboundedReceiver<Job>,
) {
    let mut engine: Option<Engine> = None;
    loop {
        let job = tokio::select! {
            biased;
            Some(job) = cleanup.recv() => job,
            job = jobs.recv() => match job {Some(job)=>job,None=>break},
            _ = tokio::time::sleep(Duration::from_secs(300)) => { engine = None; continue; }
        };
        let Job {
            expected_generation,
            operation,
            deadline,
            reply,
            progress,
        } = job;
        if expected_generation.is_none() {
            let missing = match operation {
                Operation::RenameEntry { .. } => {
                    Some("Missing SSH connection approval; rename was not started")
                }
                Operation::RemoveEntry { .. } => {
                    Some("Missing SSH connection approval; delete was not started")
                }
                Operation::ListMountedFolder { .. } => {
                    Some("Missing SSH connection for this mounted folder")
                }
                _ => None,
            };
            if let Some(message) = missing {
                let _ = reply.send(Err(message.into()));
                continue;
            }
        }
        if expected_generation.is_some_and(|expected| expected != generation) {
            let _ = reply.send(Err(
                "SSH connection changed; operation was not started".into()
            ));
            continue;
        }
        if reply.is_closed() && !matches!(operation, Operation::Close { .. }) {
            continue;
        }
        let detached_close = reply.is_closed() && matches!(operation, Operation::Close { .. });
        let deadline = if detached_close {
            Instant::now() + Duration::from_secs(15)
        } else {
            deadline
        };
        if deadline <= Instant::now() {
            let _ = reply.send(Err(
                "SFTP request expired in the queue; operation was not started".into(),
            ));
            continue;
        }
        let creates_handle = matches!(
            operation,
            Operation::Open { .. } | Operation::Opendir { .. }
        );
        let mut reply = PendingReply {
            sender: Some(reply),
            stage: match operation {
                Operation::Put { .. } => JobStage::Upload,
                Operation::RenameEntry { .. } => JobStage::Checking("Rename was not started"),
                Operation::RemoveEntry { .. } => JobStage::Checking("Delete was not started"),
                _ => JobStage::Ordinary,
            },
        };
        // Finish an admitted request even if its caller disappears. Cancelling a
        // read must not reset the shared SFTP channel and invalidate CLI handles.
        let result = match tokio::time::timeout_at(deadline, async {
            if engine.is_none() {
                engine = Some(Engine::connect(&session).await?);
            }
            engine
                .as_mut()
                .unwrap()
                .execute(operation, progress.as_ref(), &mut reply.stage)
                .await
        })
        .await
        {
            Ok(result) => {
                let lost = result
                    .as_ref()
                    .err()
                    .and_then(|e| e.downcast_ref::<Error>())
                    .is_some_and(transport_error);
                if lost {
                    engine = None;
                }
                if matches!(reply.stage, JobStage::Checking(_))
                    || (lost && matches!(reply.stage, JobStage::AwaitingStatus(_)))
                {
                    result.context(reply.stage.interrupted())
                } else if lost {
                    result.context("SFTP channel failed; outcome may be unknown; handles invalidated, do not blindly retry")
                } else {
                    result
                }
            }
            Err(_) => {
                engine = None;
                if matches!(
                    reply.stage,
                    JobStage::Checking(_) | JobStage::AwaitingStatus(_)
                ) {
                    Err(anyhow::anyhow!(
                        "SFTP timed out; {}; handles invalidated",
                        reply.stage.interrupted()
                    ))
                } else {
                    Err(anyhow::anyhow!(
                        "SFTP timed out; partial uploads may remain and the operation may have completed; handles invalidated, do not blindly retry"
                    ))
                }
            }
        };
        if let Err(Ok(value)) = reply.finish(result.map_err(|e| format!("SFTP: {e:#}")))
            && creates_handle
            && let Some(handle) = value["handle"].as_u64()
            && let Some(active) = engine.as_mut()
        {
            let closed =
                tokio::time::timeout(Duration::from_secs(5), active.close_handle(handle)).await;
            if !matches!(closed, Ok(Ok(_))) {
                engine = None;
            }
        }
    }
}

struct Handle {
    remote: String,
    directory: bool,
}
struct Engine {
    raw: RawSftpSession,
    version: Version,
    handles: HashMap<u64, Handle>,
    read_size: usize,
    write_size: usize,
}
// Bare russh Channels do not close on drop. Guard negotiation too, so rejection
// or cancellation cannot leave unused session channels on a long-lived SSH link.
struct PendingChannel(Option<russh::Channel<russh::client::Msg>>);
impl Drop for PendingChannel {
    fn drop(&mut self) {
        if let Some(channel) = self.0.take() {
            drop(channel.into_stream());
        }
    }
}
impl Engine {
    async fn connect(session: &russh::client::Handle<crate::ssh::Handler>) -> Result<Self> {
        let channel = session
            .channel_open_session()
            .await
            .context("opening SFTP channel")?;
        let mut pending = PendingChannel(Some(channel));
        let channel = pending.0.as_mut().unwrap();
        channel.request_subsystem(true, "sftp").await?;
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match channel.wait().await {
                    Some(russh::ChannelMsg::Success) => break Ok(()),
                    Some(russh::ChannelMsg::Failure | russh::ChannelMsg::Close) | None => {
                        bail!("server rejected or closed the SFTP subsystem")
                    }
                    _ => {}
                }
            }
        })
        .await
        .context("SFTP subsystem request timed out")??;
        let mut raw = RawSftpSession::new(pending.0.take().unwrap().into_stream());
        let version = raw.init().await.context("initializing SFTP")?;
        ensure!(
            version.version == 3,
            "unsupported SFTP version {} (requires v3)",
            version.version
        );
        let mut read_size = CHUNK;
        let mut write_size = CHUNK;
        if version
            .extensions
            .get(extensions::LIMITS)
            .is_some_and(|v| v == "1")
        {
            let limits = Limits::from(raw.limits().await?);
            read_size = read_size.min(limits.read_len.unwrap_or(CHUNK as u64) as usize);
            // Leave room for packet headers and server handles.
            write_size = write_size
                .min(limits.write_len.unwrap_or(CHUNK as u64) as usize)
                .min(limits.packet_len.unwrap_or(u64::MAX).saturating_sub(1024) as usize);
            raw.set_limits(limits);
        }
        ensure!(
            read_size > 0 && write_size > 0,
            "server advertised unusable packet limits"
        );
        Ok(Self {
            raw,
            version,
            handles: HashMap::new(),
            read_size,
            write_size,
        })
    }
    fn extension(&self, name: &str, version: &str) -> Result<()> {
        ensure!(
            self.version
                .extensions
                .get(name)
                .is_some_and(|v| v == version),
            "server does not advertise {name} version {version}"
        );
        Ok(())
    }
    fn handle(&self, id: u64, directory: Option<bool>) -> Result<String> {
        let h = self
            .handles
            .get(&id)
            .context("unknown, closed, expired, or wrong-pane SFTP handle")?;
        if let Some(directory) = directory {
            ensure!(h.directory == directory, "incorrect SFTP handle type");
        }
        Ok(h.remote.clone())
    }
    async fn remember(&mut self, remote: String, directory: bool) -> Result<Value> {
        if self.handles.len() >= 128 {
            let _ = self.raw.close(remote).await;
            bail!("128 open handles; close unused handles");
        }
        let id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
        self.handles.insert(id, Handle { remote, directory });
        Ok(json!({"handle":id,"directory":directory}))
    }
    async fn close_handle(&mut self, handle: u64) -> Result<()> {
        self.raw.close(self.handle(handle, None)?).await?;
        self.handles.remove(&handle);
        Ok(())
    }

    async fn execute(
        &mut self,
        op: Operation,
        progress: Option<&watch::Sender<TransferProgress>>,
        stage: &mut JobStage,
    ) -> Result<Value> {
        use Operation::*;
        let ok = json!({});
        let mounted = matches!(op, ListMountedFolder { .. });
        Ok(match op {
            Info => {
                json!({"version":self.version.version,"extensions":self.version.extensions,"open_handles":self.handles.len(),"read_chunk":self.read_size,"write_chunk":self.write_size})
            }
            Get {
                remote,
                local,
                overwrite,
                max_bytes,
            } => {
                self.get(remote, local, overwrite, max_bytes, progress)
                    .await?
            }
            Put {
                local,
                remote,
                overwrite,
            } => self.put(local, remote, overwrite, progress).await?,
            Opendir { path } => {
                let h = self.raw.opendir(path).await?.handle;
                self.remember(h, true).await?
            }
            Open { path, flags, attrs } => {
                let h = self
                    .raw
                    .open(path, open_flags(&flags)?, attrs.protocol()?)
                    .await?
                    .handle;
                self.remember(h, false).await?
            }
            Close { handle } => {
                self.close_handle(handle).await?;
                ok
            }
            Readdir { handle } => match self.raw.readdir(self.handle(handle, Some(true))?).await {
                Ok(names) => json!({"entries":names.files,"eof":false}),
                Err(e) if eof(&e) => json!({"entries":[],"eof":true}),
                Err(e) => return Err(e.into()),
            },
            Ls { path } | ListMountedFolder { path } => {
                if mounted {
                    let names = self
                        .raw
                        .realpath(&path)
                        .await
                        .context("SFTP could not resolve this mount path")?
                        .files;
                    ensure!(
                        matches!(names.as_slice(), [entry] if entry.filename == path),
                        "SFTP resolves this mount to a different path"
                    );
                }
                let h = self.raw.opendir(path).await?.handle;
                let result = async {
                    let mut entries = Vec::new();
                    let mut bytes = 0;
                    loop {
                        match self.raw.readdir(&h).await {
                            Ok(names) => {
                                bytes += serde_json::to_vec(&names.files)?.len();
                                ensure!(
                                    bytes <= 8 * 1024 * 1024,
                                    "directory listing exceeds 8 MiB; use opendir/readdir/close"
                                );
                                entries.extend(names.files);
                            }
                            Err(e) if eof(&e) => break,
                            Err(e) => return Err(e.into()),
                        }
                    }
                    Ok(json!({"entries":entries}))
                }
                .await;
                let closed = self.raw.close(h).await;
                let value: Value = result?;
                closed?;
                value
            }
            Read {
                handle,
                offset,
                len,
            } => {
                ensure!(
                    len > 0 && len as usize <= self.read_size,
                    "read length must be 1..={}",
                    self.read_size
                );
                match self
                    .raw
                    .read(self.handle(handle, Some(false))?, offset, len)
                    .await
                {
                    Ok(data) => {
                        ensure!(
                            data.data.len() <= len as usize,
                            "server returned more SFTP data than requested"
                        );
                        json!({"data_base64":STANDARD.encode(&data.data),"bytes_read":data.data.len(),"eof":false})
                    }
                    Err(e) if eof(&e) => json!({"data_base64":"","bytes_read":0,"eof":true}),
                    Err(e) => return Err(e.into()),
                }
            }
            Write {
                handle,
                offset,
                data_base64,
            } => {
                ensure!(
                    data_base64.len() <= self.write_size.div_ceil(3) * 4,
                    "write exceeds {} bytes",
                    self.write_size
                );
                let data = STANDARD
                    .decode(data_base64)
                    .context("invalid data_base64")?;
                ensure!(
                    data.len() <= self.write_size,
                    "write exceeds {} bytes",
                    self.write_size
                );
                let count = data.len();
                self.raw
                    .write(self.handle(handle, Some(false))?, offset, data)
                    .await?;
                json!({"bytes_written":count})
            }
            Stat { path } => json!({"attrs":self.raw.stat(path).await?.attrs}),
            Lstat { path } => json!({"attrs":self.raw.lstat(path).await?.attrs}),
            Fstat { handle } => {
                json!({"attrs":self.raw.fstat(self.handle(handle,None)?).await?.attrs})
            }
            Setstat { path, attrs } => {
                self.raw.setstat(path, attrs.protocol()?).await?;
                ok
            }
            Fsetstat { handle, attrs } => {
                self.raw
                    .fsetstat(self.handle(handle, None)?, attrs.protocol()?)
                    .await?;
                ok
            }
            Mkdir { path, attrs } => {
                self.raw.mkdir(path, attrs.protocol()?).await.map_err(|error| {
                    if matches!(error, Error::Status(ref status) if !matches!(status.status_code, StatusCode::NoConnection | StatusCode::ConnectionLost)) {
                        anyhow::Error::from(error)
                    } else {
                        anyhow::Error::from(error).context("Folder creation outcome unknown; refresh to check")
                    }
                })?;
                ok
            }
            Rename { from, to } => {
                self.raw.rename(from, to).await?;
                ok
            }
            RenameEntry { from, to } => {
                // LSTAT sees the directory entry even if it is a dangling link.
                // A concurrent creator after this check is the v3 server's responsibility.
                match self.raw.lstat(&to).await {
                    Ok(_) => bail!("An entry with that name already exists"),
                    Err(Error::Status(status)) if status.status_code == StatusCode::NoSuchFile => {}
                    Err(error) => {
                        return Err(anyhow::Error::from(error)
                            .context("Could not check the destination name"));
                    }
                }
                *stage = JobStage::AwaitingStatus("Rename outcome unknown; refresh to check");
                self.raw.rename(from, to).await?;
                ok
            }
            RemoveEntry { path, kind } => {
                // A trailing slash can make even LSTAT dereference a directory link;
                // Windows SFTP servers can also interpret backslashes in a leaf.
                let name = path.rsplit('/').next().unwrap_or("");
                ensure!(
                    !matches!(name, "" | "." | "..")
                        && !name.contains('\\')
                        && !path.contains('\0'),
                    "Expected a remote directory entry name"
                );
                let attrs = self
                    .raw
                    .lstat(&path)
                    .await
                    .context("Could not check the selected entry")?
                    .attrs;
                let actual = EntryKind::from_permissions(attrs.permissions)
                    .context("The server reported an unsupported type for this entry")?;
                ensure!(
                    actual == kind,
                    "The selected entry type changed; refresh to check"
                );
                // v3 has no conditional unlink. Use exactly one request against
                // this name; the server enforces directory emptiness for RMDIR.
                *stage = JobStage::AwaitingStatus("Delete outcome unknown; refresh to check");
                match kind {
                    EntryKind::File | EntryKind::Symlink => self.raw.remove(path).await?,
                    EntryKind::Directory => self.raw.rmdir(path).await?,
                };
                ok
            }
            RemoveFile { path } => {
                self.raw.remove(path).await?;
                ok
            }
            RemoveDir { path } => {
                self.raw.rmdir(path).await?;
                ok
            }
            Realpath { path } => json!({"entries":self.raw.realpath(path).await?.files}),
            Readlink { path } => json!({"entries":self.raw.readlink(path).await?.files}),
            Symlink {
                path,
                target,
                spec_order,
            } => {
                // OpenSSH deliberately reverses the draft-v3 wire fields. Most deployed
                // servers follow it; strict draft servers can request the original order.
                if spec_order {
                    self.raw.symlink(path, target).await?;
                } else {
                    self.raw.symlink(target, path).await?;
                }
                ok
            }
            Hardlink { from, to } => {
                self.extension(extensions::HARDLINK, "1")?;
                self.raw.hardlink(from, to).await?;
                ok
            }
            Fsync { handle } => {
                self.extension(extensions::FSYNC, "1")?;
                self.raw.fsync(self.handle(handle, Some(false))?).await?;
                ok
            }
            Statvfs { path } => {
                self.extension(extensions::STATVFS, "2")?;
                serde_json::to_value(self.raw.statvfs(path).await?)?
            }
            Limits => {
                self.extension(extensions::LIMITS, "1")?;
                serde_json::to_value(self.raw.limits().await?)?
            }
            ExpandPath { path } => {
                self.extension(extensions::EXPAND_PATH, "1")?;
                json!({"entries":self.raw.expand_path(path).await?.files})
            }
        })
    }
    async fn get(
        &self,
        remote: String,
        local: PathBuf,
        overwrite: bool,
        max_bytes: Option<u64>,
        progress: Option<&watch::Sender<TransferProgress>>,
    ) -> Result<Value> {
        ensure!(local.is_absolute(), "local path must be absolute");
        if !overwrite {
            ensure!(
                !local.try_exists()?,
                "local destination exists; use --overwrite"
            );
        }
        let parent = local
            .parent()
            .context("local destination has no parent")?
            .to_owned();
        let temp =
            tokio::task::spawn_blocking(move || tempfile::NamedTempFile::new_in(parent)).await??;
        let mut file = tokio::fs::File::from_std(temp.reopen()?);
        let h = self
            .raw
            .open(&remote, OpenFlags::READ, FileAttributes::empty())
            .await?
            .handle;
        let copied = async {
            // Some servers omit size or disallow FSTAT on readable files.
            let total = if progress.is_some() {
                match self.raw.fstat(&h).await {
                    Ok(reply) => reply.attrs.size,
                    Err(error) if transport_error(&error) => return Err(error.into()),
                    Err(_) => None,
                }
            } else {
                None
            };
            report(progress, 0, total);
            let mut offset = 0u64;
            loop {
                match self.raw.read(&h, offset, self.read_size as u32).await {
                    Ok(data) => {
                        ensure!(
                            data.data.len() <= self.read_size,
                            "server returned more SFTP data than requested"
                        );
                        ensure!(
                            !data.data.is_empty(),
                            "server returned empty data without EOF"
                        );
                        let next = offset
                            .checked_add(data.data.len() as u64)
                            .context("file size overflow")?;
                        ensure!(
                            max_bytes.is_none_or(|limit| next <= limit),
                            "download exceeds --max-bytes limit"
                        );
                        file.write_all(&data.data).await?;
                        offset = next;
                        report(progress, offset, total);
                    }
                    Err(e) if eof(&e) => break,
                    Err(e) => return Err(e.into()),
                }
            }
            file.sync_all().await?;
            Ok::<u64, anyhow::Error>(offset)
        }
        .await;
        let closed = self.raw.close(h).await;
        let bytes = copied?;
        closed?;
        drop(file);
        let destination = local.clone();
        tokio::task::spawn_blocking(move || {
            if overwrite {
                temp.persist(destination)
            } else {
                temp.persist_noclobber(destination)
            }
        })
        .await??;
        Ok(json!({"local":local,"remote":remote,"bytes_transferred":bytes}))
    }
    async fn put(
        &self,
        local: PathBuf,
        remote: String,
        overwrite: bool,
        progress: Option<&watch::Sender<TransferProgress>>,
    ) -> Result<Value> {
        ensure!(local.is_absolute(), "local path must be absolute");
        let mut file = tokio::fs::File::open(&local)
            .await
            .context("opening local source")?;
        let metadata = file.metadata().await?;
        ensure!(metadata.is_file(), "local source must be a regular file");
        let total = Some(metadata.len());
        report(progress, 0, total);
        let flags = OpenFlags::WRITE
            | OpenFlags::CREATE
            | if overwrite {
                OpenFlags::TRUNCATE
            } else {
                OpenFlags::EXCLUDE
            };
        let h = self
            .raw
            .open(&remote, flags, FileAttributes::empty())
            .await?
            .handle;
        let copied = async {
            let mut buf = vec![0; self.write_size];
            let mut offset = 0u64;
            loop {
                let len = file.read(&mut buf).await?;
                if len == 0 {
                    break;
                }
                self.raw.write(&h, offset, buf[..len].to_vec()).await?;
                offset = offset
                    .checked_add(len as u64)
                    .context("file size overflow")?;
                report(progress, offset, total);
            }
            Ok::<u64, anyhow::Error>(offset)
        }
        .await;
        let closed = self.raw.close(h).await;
        let bytes =
            copied.context("upload failed; remote destination may contain a partial file")?;
        closed.context("upload close failed; remote contents are uncertain")?;
        Ok(json!({"local":local,"remote":remote,"bytes_transferred":bytes}))
    }
}
fn eof(error: &Error) -> bool {
    matches!(error,Error::Status(s) if s.status_code==StatusCode::Eof)
}
fn transport_error(error: &Error) -> bool {
    matches!(
        error,
        Error::Timeout | Error::IO(_) | Error::UnexpectedPacket | Error::UnexpectedBehavior(_)
    ) || matches!(error,Error::Status(s) if matches!(s.status_code,StatusCode::ConnectionLost|StatusCode::NoConnection))
}

#[cfg(test)]
mod tests {
    use super::EntryKind;

    #[test]
    fn drawer_deletion_requires_explicit_regular_directory_or_link_type_bits() {
        for (mode, kind) in [
            (0o100000, EntryKind::File),
            (0o040000, EntryKind::Directory),
            (0o120000, EntryKind::Symlink),
        ] {
            assert_eq!(EntryKind::from_permissions(Some(mode | 0o7755)), Some(kind));
        }
        for mode in [
            None,
            Some(0o755),
            Some(0o010644),
            Some(0o020640),
            Some(0o060660),
            Some(0o140777),
        ] {
            assert_eq!(EntryKind::from_permissions(mode), None);
        }
    }
}
