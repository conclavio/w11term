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
    sync::{mpsc, oneshot},
    time::Instant,
};

pub const CHUNK: usize = 32 * 1024;
pub const DEFAULT_TIMEOUT: u64 = 600;
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

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

#[derive(Debug)]
pub struct Job {
    pub operation: Operation,
    pub deadline: Instant,
    pub reply: oneshot::Sender<Result<Value, String>>,
}

pub(crate) async fn run(
    session: Arc<russh::client::Handle<crate::ssh::Handler>>,
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
            operation,
            deadline,
            reply,
        } = job;
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
        // Finish an admitted request even if its caller disappears. Cancelling a
        // read must not reset the shared SFTP channel and invalidate CLI handles.
        let result = match tokio::time::timeout_at(deadline, async {
            if engine.is_none() {
                engine = Some(Engine::connect(&session).await?);
            }
            engine.as_mut().unwrap().execute(operation).await
        })
        .await
        {
            Ok(result) => {
                if result
                    .as_ref()
                    .err()
                    .and_then(|e| e.downcast_ref::<Error>())
                    .is_some_and(transport_error)
                {
                    engine = None;
                    result.context("SFTP channel failed; outcome may be unknown; handles invalidated, do not blindly retry")
                } else {
                    result
                }
            }
            Err(_) => {
                engine = None;
                Err(anyhow::anyhow!(
                    "SFTP timed out; partial uploads may remain and the operation may have completed; handles invalidated, do not blindly retry"
                ))
            }
        };
        if let Err(Ok(value)) = reply.send(result.map_err(|e| format!("SFTP: {e:#}")))
            && creates_handle
            && let Some(handle) = value["handle"].as_u64()
            && let Some(active) = engine.as_mut()
        {
            let closed = tokio::time::timeout(
                Duration::from_secs(5),
                active.execute(Operation::Close { handle }),
            )
            .await;
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
    async fn execute(&mut self, op: Operation) -> Result<Value> {
        use Operation::*;
        let ok = json!({});
        Ok(match op {
            Info => {
                json!({"version":self.version.version,"extensions":self.version.extensions,"open_handles":self.handles.len(),"read_chunk":self.read_size,"write_chunk":self.write_size})
            }
            Get {
                remote,
                local,
                overwrite,
                max_bytes,
            } => self.get(remote, local, overwrite, max_bytes).await?,
            Put {
                local,
                remote,
                overwrite,
            } => self.put(local, remote, overwrite).await?,
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
                let h = self.handle(handle, None)?;
                self.raw.close(h).await?;
                self.handles.remove(&handle);
                ok
            }
            Readdir { handle } => match self.raw.readdir(self.handle(handle, Some(true))?).await {
                Ok(names) => json!({"entries":names.files,"eof":false}),
                Err(e) if eof(&e) => json!({"entries":[],"eof":true}),
                Err(e) => return Err(e.into()),
            },
            Ls { path } => {
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
                self.raw.mkdir(path, attrs.protocol()?).await?;
                ok
            }
            Rename { from, to } => {
                self.raw.rename(from, to).await?;
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
                        offset = offset
                            .checked_add(data.data.len() as u64)
                            .context("file size overflow")?;
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
    async fn put(&self, local: PathBuf, remote: String, overwrite: bool) -> Result<Value> {
        ensure!(local.is_absolute(), "local path must be absolute");
        let mut file = tokio::fs::File::open(&local)
            .await
            .context("opening local source")?;
        ensure!(
            file.metadata().await?.is_file(),
            "local source must be a regular file"
        );
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
