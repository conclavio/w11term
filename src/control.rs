//! Local, current-user-only automation. This module never accesses a window or keyboard.
use crate::{session::Command, terminal::Snapshot};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{ClientOptions, NamedPipeServer},
    sync::{mpsc, oneshot},
};
use wezterm_term::TerminalSize;
mod security;
use security::PipeSecurity;

pub const VERSION: u32 = 1;
pub const MAX_BYTES: usize = 1024 * 1024;
const MAX_LINE: usize = MAX_BYTES * 2;
const TIMEOUT: Duration = Duration::from_secs(10);
pub type CapturePng =
    Arc<dyn Fn(&Snapshot, TerminalSize, &Path) -> Result<(u32, u32)> + Send + Sync>;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
    Record {
        pane: u64,
        path: Option<PathBuf>,
    },
    WaitForText {
        pane: u64,
        pattern: String,
        #[serde(default)]
        regex: bool,
        #[serde(default = "default_wait_timeout")]
        timeout_seconds: u64,
    },
    Events {
        pane: u64,
        #[serde(default)]
        since: u64,
        #[serde(default)]
        timeout_seconds: u64,
    },
    Resize {
        pane: u64,
        cols: usize,
        rows: usize,
    },
    Open {
        profile: String,
        cols: Option<usize>,
        rows: Option<usize>,
        #[serde(default)]
        activate: bool,
    },
    Sftp {
        pane: u64,
        #[serde(default = "sftp_default_timeout")]
        timeout_seconds: u64,
        operation: crate::sftp::Operation,
    },
    List,
    SendText {
        pane: u64,
        data_base64: String,
        no_paste: bool,
    },
    GetText {
        pane: u64,
    },
    Screenshot {
        pane: u64,
        path: PathBuf,
    },
}
fn default_wait_timeout() -> u64 {
    30
}
#[derive(Debug)]
pub enum UiRequest {
    Open {
        profile: String,
        cols: Option<usize>,
        rows: Option<usize>,
        activate: bool,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    Resize {
        pane: u64,
        cols: usize,
        rows: usize,
        reply: oneshot::Sender<Result<Value, String>>,
    },
}
impl UiRequest {
    pub fn fail(self, message: &str) {
        let reply = match self {
            Self::Open { reply, .. } | Self::Resize { reply, .. } => reply,
        };
        let _ = reply.send(Err(message.into()));
    }
}
type UiHandler = Arc<dyn Fn(UiRequest) + Send + Sync>;
fn sftp_default_timeout() -> u64 {
    crate::sftp::DEFAULT_TIMEOUT
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    pub version: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
impl Reply {
    pub fn from_result(result: Result<Value>) -> Self {
        match result {
            Ok(value) => Self {
                version: VERSION,
                ok: true,
                result: Some(value),
                error: None,
            },
            Err(error) => Self {
                version: VERSION,
                ok: false,
                result: None,
                error: Some(format!("{error:#}")),
            },
        }
    }
}

#[derive(Clone)]
pub struct Pane {
    pub id: u64,
    pub label: String,
    pub active: bool,
    pub input: mpsc::UnboundedSender<Command>,
}
#[derive(Clone, Default)]
pub struct Registry(
    Arc<Mutex<BTreeMap<u64, Pane>>>,
    Arc<Mutex<Option<UiHandler>>>,
);
impl Registry {
    pub fn set_ui(&self, handler: impl Fn(UiRequest) + Send + Sync + 'static) {
        *self.1.lock().unwrap() = Some(Arc::new(handler));
    }
    fn ui(&self) -> Result<UiHandler> {
        self.1
            .lock()
            .unwrap()
            .clone()
            .context("this instance has no GUI control handler")
    }
    pub fn replace(&self, panes: impl IntoIterator<Item = Pane>) {
        *self.0.lock().unwrap() = panes.into_iter().map(|p| (p.id, p)).collect();
    }
    fn pane(&self, id: u64) -> Result<Pane> {
        self.0
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .with_context(|| format!("unknown pane {id}"))
    }
    async fn capture(pane: &Pane) -> Result<(Snapshot, TerminalSize)> {
        let (tx, rx) = oneshot::channel();
        pane.input
            .send(Command::Capture(tx))
            .context("pane is closed")?;
        tokio::time::timeout(TIMEOUT, rx)
            .await
            .context("pane capture timed out")?
            .context("pane is closed")
    }
    pub async fn execute(&self, request: Request, capture_png: CapturePng) -> Result<Value> {
        match request {
            Request::Record { pane, path } => {
                ensure!(
                    path.as_ref().is_none_or(|p| p.is_absolute()),
                    "recording path must be absolute"
                );
                let (reply, response) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::Record { path, reply })
                    .context("pane is closed")?;
                tokio::time::timeout(TIMEOUT, response)
                    .await??
                    .map_err(anyhow::Error::msg)
            }
            Request::Open {
                profile,
                cols,
                rows,
                activate,
            } => {
                validate_grid(cols.unwrap_or(120), rows.unwrap_or(32))?;
                let (reply, response) = oneshot::channel();
                self.ui()?(UiRequest::Open {
                    profile,
                    cols,
                    rows,
                    activate,
                    reply,
                });
                tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("open timed out; tab may have been created")?
                    .context("GUI is closed")?
                    .map_err(anyhow::Error::msg)
            }
            Request::Resize { pane, cols, rows } => {
                validate_grid(cols, rows)?;
                self.pane(pane)?;
                let (reply, response) = oneshot::channel();
                self.ui()?(UiRequest::Resize {
                    pane,
                    cols,
                    rows,
                    reply,
                });
                tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("resize timed out; geometry may have changed")?
                    .context("GUI is closed")?
                    .map_err(anyhow::Error::msg)
            }
            Request::Events {
                pane,
                since,
                timeout_seconds,
            } => {
                ensure!(
                    timeout_seconds <= 3600,
                    "events timeout must be 0..=3600 seconds"
                );
                let (reply, response) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::Events(reply))
                    .context("pane is closed")?;
                let events = tokio::time::timeout(TIMEOUT, response).await??;
                ensure!(
                    since <= events.since(0).next,
                    "event cursor is ahead of this pane; check the instance/pane identity"
                );
                Ok(
                    json!({"pane":pane,"journal":events.wait(since,Duration::from_secs(timeout_seconds)).await}),
                )
            }
            Request::WaitForText {
                pane,
                pattern,
                regex,
                timeout_seconds,
            } => {
                ensure!(
                    (1..=3600).contains(&timeout_seconds),
                    "wait timeout must be 1..=3600 seconds"
                );
                ensure!(pattern.len() <= 4096, "pattern exceeds 4096 bytes");
                let matcher = if regex {
                    Some(
                        regex::RegexBuilder::new(&pattern)
                            .size_limit(1024 * 1024)
                            .dfa_size_limit(8 * 1024 * 1024)
                            .build()
                            .context("invalid regular expression")?,
                    )
                } else {
                    None
                };
                let target = self.pane(pane)?;
                let (reply, response) = oneshot::channel();
                target
                    .input
                    .send(Command::Frames(reply))
                    .context("pane is closed")?;
                let frames = tokio::time::timeout(TIMEOUT, response).await??;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
                loop {
                    let frame = frames.load();
                    let generation = frame.generation;
                    let text = frame
                        .rows
                        .iter()
                        .map(|line| line.as_str().trim_end_matches(' ').to_owned())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if matcher
                        .as_ref()
                        .map_or_else(|| text.contains(&pattern), |r| r.is_match(&text))
                    {
                        return Ok(
                            json!({"pane":pane,"matched":true,"generation":frame.generation,"cols":frame.cols,"rows":frame.rows.len(),"text":text}),
                        );
                    }
                    ensure!(
                        frames.changed(generation, deadline).await,
                        "wait-for-text timed out without a match"
                    );
                }
            }
            Request::Sftp {
                pane,
                timeout_seconds,
                operation,
            } => {
                ensure!(
                    (1..=3600).contains(&timeout_seconds),
                    "SFTP timeout must be 1..=3600 seconds"
                );
                let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
                let (reply, response) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::Sftp(crate::sftp::Job {
                        operation,
                        deadline,
                        reply,
                    }))
                    .context("pane is closed")?;
                let result = tokio::time::timeout_at(deadline + Duration::from_secs(1), response)
                    .await
                    .context("SFTP timed out; outcome may be unknown, do not blindly retry")?
                    .context("SSH connection closed during SFTP operation")?
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"pane":pane,"sftp":result}))
            }
            Request::List => {
                let panes: Vec<_> = self.0.lock().unwrap().values().cloned().collect();
                let mut sessions = Vec::new();
                for pane in panes {
                    match Self::capture(&pane).await {
                        Ok((frame, size)) => sessions.push(json!({"id":pane.id,"label":pane.label,"active":pane.active,"title":frame.title,"status":frame.status,"cols":size.cols,"rows":size.rows,"prompt":frame.prompt,"connection_generation":frame.connection_generation})),
                        Err(error) => sessions.push(json!({"id":pane.id,"label":pane.label,"active":pane.active,"status":"unavailable","error":format!("{error:#}")})),
                    }
                }
                Ok(json!({"pid":std::process::id(),"sessions":sessions}))
            }
            Request::SendText {
                pane,
                data_base64,
                no_paste,
            } => {
                ensure!(
                    no_paste,
                    "only raw input is supported; specify --no-paste / no_paste:true"
                );
                ensure!(
                    data_base64.len() <= MAX_BYTES.div_ceil(3) * 4,
                    "input exceeds 1 MiB"
                );
                let bytes = STANDARD
                    .decode(data_base64)
                    .context("invalid data_base64")?;
                ensure!(bytes.len() <= MAX_BYTES, "input exceeds 1 MiB");
                let count = bytes.len();
                let (tx, rx) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::Raw(bytes, tx))
                    .context("pane is closed")?;
                tokio::time::timeout(TIMEOUT, rx)
                    .await
                    .context("SSH write timed out; delivery is unknown, do not blindly retry")?
                    .context("SSH connection closed before acknowledging the write")?
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"pane":pane,"bytes_written":count}))
            }
            Request::GetText { pane } => {
                let (frame, size) = Self::capture(&self.pane(pane)?).await?;
                let lines: Vec<_> = frame
                    .rows
                    .iter()
                    .map(|line| line.as_str().trim_end_matches(' ').to_owned())
                    .collect();
                Ok(
                    json!({"pane":pane,"generation":frame.generation,"cols":size.cols,"rows":size.rows,"lines":lines,"text":lines.join("\n"),"title":frame.title,"status":frame.status,"alternate_screen":frame.alternate}),
                )
            }
            Request::Screenshot { pane, path } => {
                ensure!(path.is_absolute(), "screenshot path must be absolute");
                let (frame, size) = Self::capture(&self.pane(pane)?).await?;
                let generation = frame.generation;
                let saved_path = path.clone();
                let (width, height) =
                    tokio::task::spawn_blocking(move || capture_png(&frame, size, &path)).await??;
                Ok(
                    json!({"pane":pane,"generation":generation,"path":saved_path,"width":width,"height":height}),
                )
            }
        }
    }
}
fn validate_grid(cols: usize, rows: usize) -> Result<()> {
    ensure!(
        (1..=1000).contains(&cols) && (1..=500).contains(&rows),
        "grid must be 1..=1000 columns and 1..=500 rows"
    );
    Ok(())
}

pub fn pipe_name(pid: u32) -> String {
    format!(r"\\.\pipe\w11term-{pid}")
}
pub fn discovery_dir() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
            .join("w11term")
            .join("instances"),
    )
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Instance {
    pub version: u32,
    pub pid: u32,
    pub pipe: String,
}

/// Owns the listener and discovery record. Drop stops it and removes our record.
pub struct Server {
    stop: Option<oneshot::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
    manifest: PathBuf,
}
impl Server {
    pub fn start(
        registry: Registry,
        capture_png: impl Fn(&Snapshot, TerminalSize, &Path) -> Result<(u32, u32)>
        + Send
        + Sync
        + 'static,
    ) -> Result<Self> {
        Self::start_in_directory(registry, capture_png, discovery_dir()?)
    }
    /// Embedders/tests can isolate discovery records without changing process environment.
    pub fn start_in_directory(
        registry: Registry,
        capture_png: impl Fn(&Snapshot, TerminalSize, &Path) -> Result<(u32, u32)>
        + Send
        + Sync
        + 'static,
        directory: PathBuf,
    ) -> Result<Self> {
        let capture_png: CapturePng = Arc::new(capture_png);
        std::fs::create_dir_all(&directory)?;
        let pid = std::process::id();
        let manifest = directory.join(format!("{pid}.json"));
        let worker_manifest = manifest.clone();
        let (stop_tx, stop_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new().name("terminal-control".into()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            runtime.block_on(async move {
                let security = match PipeSecurity::new() { Ok(s)=>Arc::new(s), Err(e)=>{let _=ready_tx.send(Err(e)); return;} };
                let name = pipe_name(pid);
                let mut listener = match security.create(&name, true) {Ok(p)=>p,Err(e)=>{let _=ready_tx.send(Err(e));return;}};
                let _ = ready_tx.send(Ok(()));
                let ingress = Arc::new(tokio::sync::Semaphore::new(32));
                let commands = Arc::new(tokio::sync::Semaphore::new(8));
                let waits = Arc::new(tokio::sync::Semaphore::new(16));
                tokio::pin!(stop_rx);
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => break,
                        connected = listener.connect() => {
                            if let Err(e) = connected { log::error!("control pipe: {e}"); break; }
                            let next = match security.create(&name, false) {Ok(p)=>p,Err(e)=>{log::error!("control pipe: {e:#}");break;}};
                            let client = std::mem::replace(&mut listener, next);
                            if let Ok(permit) = ingress.clone().try_acquire_owned() {
                                let registry = registry.clone();
                                let capture_png=capture_png.clone(); let security=security.clone();
                                let commands=commands.clone(); let waits=waits.clone();
                                tokio::spawn(async move { let _permit=permit; if let Err(error)=serve(client, registry, capture_png, security, commands, waits).await { log::warn!("control request failed: {error:#}"); } });
                            } else {
                                // Bound overload handling too: never spawn an unbounded rejection task.
                                let mut client=client;
                                let _=tokio::time::timeout(Duration::from_millis(250), write_reply(&mut client,
                                    Err(anyhow::anyhow!("busy: control connection limit reached")))).await;
                            }
                        }
                    }
                }
            });
            let _=std::fs::remove_file(worker_manifest);
        })?;
        ready_rx.recv().context("control server startup failed")??;
        let server = Self {
            stop: Some(stop_tx),
            worker: Some(worker),
            manifest,
        };
        let instance = Instance {
            version: VERSION,
            pid,
            pipe: pipe_name(pid),
        };
        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        serde_json::to_writer(&mut file, &instance)?;
        file.persist(&server.manifest)?;
        Ok(server)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.manifest);
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

async fn serve(
    mut pipe: NamedPipeServer,
    registry: Registry,
    capture_png: CapturePng,
    security: Arc<PipeSecurity>,
    commands: Arc<tokio::sync::Semaphore>,
    waits: Arc<tokio::sync::Semaphore>,
) -> Result<()> {
    let mut read = BufReader::new(&mut pipe).take((MAX_LINE + 1) as u64);
    let mut line = Vec::new();
    tokio::time::timeout(TIMEOUT, read.read_until(b'\n', &mut line)).await??;
    drop(read);
    let result = if let Err(error) = security.authorize(&pipe) {
        Err(error)
    } else if line.len() > MAX_LINE {
        Err(anyhow::anyhow!("request exceeds 2 MiB"))
    } else if !line.ends_with(b"\n") {
        Err(anyhow::anyhow!("request must end with newline"))
    } else {
        match serde_json::from_slice::<Request>(&line) {
            Ok(r) => {
                let pool = if matches!(
                    r,
                    Request::WaitForText { .. }
                        | Request::Events {
                            timeout_seconds: 1..,
                            ..
                        }
                ) {
                    waits
                } else {
                    commands
                };
                match pool.try_acquire_owned() {
                    Ok(_permit) => registry.execute(r, capture_png).await,
                    Err(_) => Err(anyhow::anyhow!(
                        "busy: control request capacity reached; request was not executed"
                    )),
                }
            }
            Err(e) => Err(e.into()),
        }
    };
    write_reply(&mut pipe, result).await
}
async fn write_reply(pipe: &mut NamedPipeServer, result: Result<Value>) -> Result<()> {
    let mut bytes = serde_json::to_vec(&Reply::from_result(result))?;
    bytes.push(b'\n');
    tokio::time::timeout(TIMEOUT, pipe.write_all(&bytes)).await??;
    Ok(())
}

async fn connect(pid: u32) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    let start = std::time::Instant::now();
    loop {
        match ClientOptions::new()
            .security_qos_flags(windows::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION.0)
            .open(pipe_name(pid))
        {
            Ok(pipe) => {
                security::verify_server(&pipe, pid)?;
                return Ok(pipe);
            }
            Err(e) if e.raw_os_error() == Some(231) && start.elapsed() < Duration::from_secs(2) => {
                tokio::time::sleep(Duration::from_millis(20)).await
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("no accessible w11term instance at PID {pid}; nothing was started")
                });
            }
        }
    }
}
pub async fn discover() -> Result<Vec<Instance>> {
    let directory = discovery_dir()?;
    let entries = match std::fs::read_dir(directory) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut instances = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.path().extension().is_none_or(|s| s != "json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(instance) = serde_json::from_slice::<Instance>(&bytes) else {
            continue;
        };
        if instance.version == VERSION
            && instance.pipe == pipe_name(instance.pid)
            && connect(instance.pid).await.is_ok()
        {
            instances.push(instance);
        }
    }
    instances.sort_by_key(|i| i.pid);
    Ok(instances)
}
pub async fn call(pid: Option<u32>, request: &Request) -> Result<Reply> {
    let pid = match pid {
        Some(p) => p,
        None => {
            let instances = discover().await?;
            ensure!(
                !instances.is_empty(),
                "no running w11term instance; nothing was started"
            );
            ensure!(
                instances.len() == 1,
                "multiple w11term instances; specify --pid (running PIDs: {})",
                instances
                    .iter()
                    .map(|i| i.pid.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            instances[0].pid
        }
    };
    let mut pipe = connect(pid).await?;
    let mut bytes = serde_json::to_vec(request)?;
    bytes.push(b'\n');
    ensure!(bytes.len() <= MAX_LINE, "request exceeds 2 MiB");
    tokio::time::timeout(TIMEOUT, pipe.write_all(&bytes)).await??;
    let mut read = BufReader::new(pipe).take(32 * 1024 * 1024);
    let mut response = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(match request {
            Request::Sftp {
                timeout_seconds, ..
            } => (*timeout_seconds).clamp(1, 3600) + 5,
            Request::WaitForText {
                timeout_seconds, ..
            }
            | Request::Events {
                timeout_seconds, ..
            } => (*timeout_seconds).min(3600) + 15,
            _ => 30,
        }),
        read.read_until(b'\n', &mut response),
    )
    .await
    .context("control reply timed out; command may have completed")??;
    ensure!(response.ends_with(b"\n"), "incomplete control reply");
    let reply: Reply = serde_json::from_slice(&response)?;
    ensure!(
        reply.version == VERSION,
        "unsupported control protocol version"
    );
    Ok(reply)
}
