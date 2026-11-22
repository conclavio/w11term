//! Local, current-user-only automation. Control requests never focus a desktop window.
use crate::{session::Command, terminal::Snapshot};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer},
    sync::{mpsc, oneshot},
};
use wezterm_term::TerminalSize;
use windows::Win32::{
    Foundation::{ERROR_INVALID_PARAMETER, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};
mod command_history;
mod find;
mod registry;
mod security;
pub use registry::{PaneTransfer, Registry, WindowId, native_window_matches};
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
    Restart {
        pane: u64,
        exit_token: u64,
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
    WorkspaceIdentity,
    WorkspaceWindows {
        key: String,
    },
    SendText {
        pane: u64,
        data_base64: String,
        no_paste: bool,
    },
    GetText {
        pane: u64,
    },
    ListCommands {
        pane: u64,
    },
    GetCommand {
        pane: u64,
        id: u64,
        connection_generation: u64,
    },
    FindText {
        pattern: String,
        pane: Option<u64>,
        #[serde(default)]
        all_panes: bool,
    },
    SaveText {
        pane: u64,
        path: PathBuf,
        #[serde(default)]
        overwrite: bool,
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
    pub read_only: bool,
    pub id: u64,
    pub tab_id: u64,
    pub tab_title: Option<String>,
    pub label: String,
    pub active: bool,
    pub visible: bool,
    pub input: mpsc::UnboundedSender<Command>,
}
impl Registry {
    async fn pane_events(pane: &Pane) -> Result<crate::events::Events> {
        let (reply, response) = oneshot::channel();
        pane.input
            .send(Command::Events(reply))
            .context("pane is closed")?;
        tokio::time::timeout(TIMEOUT, response)
            .await
            .context("pane did not respond")?
            .context("pane is closed")
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
                self.active_ui()?(UiRequest::Open {
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
                let (reply, response) = oneshot::channel();
                self.resize(
                    pane,
                    UiRequest::Resize {
                        pane,
                        cols,
                        rows,
                        reply,
                    },
                );
                tokio::time::timeout(TIMEOUT, response)
                    .await
                    .context("resize timed out; geometry may have changed")?
                    .context("GUI is closed")?
                    .map_err(anyhow::Error::msg)
            }
            Request::Restart { pane, exit_token } => {
                let target = self.pane(pane)?;
                let events = Self::pane_events(&target).await?;
                events.restart_exited(&target.input, exit_token)?;
                Ok(json!({"pane":pane,"exit_token":exit_token,"accepted":true}))
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
                let events = Self::pane_events(&self.pane(pane)?).await?;
                ensure!(
                    since <= events.since(0).next,
                    "event cursor is ahead of this pane; check the instance/pane identity"
                );
                let journal = events
                    .wait(since, Duration::from_secs(timeout_seconds))
                    .await;
                Ok(json!({"pane":pane,"journal":journal,"restart_token":events.restart_token()}))
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
                        expected_generation: None,
                        operation,
                        deadline,
                        reply,
                        progress: None,
                        cancel: None,
                    }))
                    .context("pane is closed")?;
                let result = tokio::time::timeout_at(deadline + Duration::from_secs(1), response)
                    .await
                    .context("SFTP timed out; outcome may be unknown, do not blindly retry")?
                    .context("SSH connection closed during SFTP operation")?
                    .map_err(anyhow::Error::msg)?;
                Ok(json!({"pane":pane,"sftp":result}))
            }
            Request::WorkspaceIdentity => Ok(json!(self.legacy_workspace())),
            Request::WorkspaceWindows { key } => Ok(json!(self.workspace_windows(&key))),
            Request::List => {
                let mut sessions = Vec::new();
                for (window_id, pane) in self.listing() {
                    let mut session = match Self::capture(&pane).await {
                        Ok((frame, size)) => json!({
                            "title": frame.title,
                            "status": frame.status,
                            "cols": size.cols,
                            "rows": size.rows,
                            "prompt": frame.prompt,
                            "connection_generation": frame.connection_generation,
                        }),
                        Err(error) => {
                            json!({"status": "unavailable", "error": format!("{error:#}")})
                        }
                    };
                    session["id"] = pane.id.into();
                    session["window_id"] = window_id.into();
                    session["tab_id"] = pane.tab_id.into();
                    if let Some(title) = pane.tab_title {
                        session["tab_title"] = title.into();
                    }
                    session["label"] = pane.label.into();
                    session["active"] = pane.active.into();
                    session["read_only"] = pane.read_only.into();
                    session["visible"] = pane.visible.into();
                    sessions.push(session);
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
            Request::FindText {
                pattern,
                pane,
                all_panes,
            } => self.find_text(pattern, pane, all_panes).await,
            Request::ListCommands { pane } => self.list_commands(pane).await,
            Request::GetCommand {
                pane,
                id,
                connection_generation,
            } => self.get_command(pane, id, connection_generation).await,
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
            Request::SaveText {
                pane,
                path,
                overwrite,
            } => {
                ensure!(path.is_absolute(), "save-text path must be absolute");
                let (reply, snapshot) = oneshot::channel();
                self.pane(pane)?
                    .input
                    .send(Command::ExportText(reply))
                    .context("pane is closed")?;
                let text = tokio::time::timeout(TIMEOUT, snapshot)
                    .await
                    .context("scrollback capture timed out")?
                    .context("pane closed before capture")?
                    .map_err(anyhow::Error::msg)?;
                let bytes = text.len();
                let saved_path = tokio::task::spawn_blocking(move || {
                    crate::transcript::save(&path, &text, overwrite)?;
                    anyhow::Ok(path)
                })
                .await??;
                Ok(json!({"pane":pane,"path":saved_path,"bytes":bytes}))
            }
            Request::Screenshot { pane, path } => {
                ensure!(path.is_absolute(), "screenshot path must be absolute");
                let (target, capture_png) = self.capture_for(pane, capture_png)?;
                let (frame, size) = Self::capture(&target).await?;
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
pub fn desktop_scope() -> Result<(String, u32, u32)> {
    security::current_scope()
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_windows: Option<Vec<WorkspaceHint>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceHint {
    pub window_id: WindowId,
    pub workspace: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkspaceMatch {
    window_id: WindowId,
    hwnd: usize,
}

/// Owns the listener and discovery record. Drop stops it and removes our record.
pub struct Server {
    stop: Option<oneshot::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
    manifest: PathBuf,
    registry: Registry,
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
        let process_registry = registry.clone();
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
            registry: process_registry,
        };
        server.registry.set_manifest(server.manifest.clone())?;
        Ok(server)
    }

    pub fn publish_workspace(&self, workspace: Option<String>) -> Result<()> {
        self.publish_window_workspace(self.registry.window_id(), workspace)
    }

    pub fn publish_window_workspace(&self, id: WindowId, workspace: Option<String>) -> Result<()> {
        self.registry.publish_workspace(id, workspace)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.registry.stop_publishing();
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
    let mut instances = Vec::new();
    for instance in discovery_records(&discovery_dir()?)? {
        if connect(instance.pid).await.is_ok() {
            instances.push(instance);
        }
    }
    Ok(instances)
}

/// Discovery files are hints. Confirm the workspace with the authenticated server
/// before using a PID, including when Windows recycles a stale record's PID.
pub struct WorkspaceOwner {
    pub pid: u32,
    pub window_id: Option<WindowId>,
    pub hwnd: usize,
    process: Arc<OwnedHandle>,
}

impl WorkspaceOwner {
    pub fn is_running(&self) -> Result<bool> {
        match unsafe { WaitForSingleObject(HANDLE(self.process.as_raw_handle()), 0) } {
            WAIT_TIMEOUT => Ok(true),
            WAIT_OBJECT_0 => Ok(false),
            _ => Err(std::io::Error::last_os_error()).context("Checking workspace process"),
        }
    }
}

pub async fn workspace_owner(key: &str) -> Result<Option<WorkspaceOwner>> {
    workspace_owner_for(key, None).await
}

pub async fn workspace_owner_for(
    key: &str,
    caller: Option<WindowId>,
) -> Result<Option<WorkspaceOwner>> {
    workspace_owner_in(&discovery_dir()?, key, caller).await
}

async fn workspace_owner_in(
    directory: &Path,
    key: &str,
    caller: Option<WindowId>,
) -> Result<Option<WorkspaceOwner>> {
    let mut records = discovery_records(directory)?;
    let current = std::process::id();
    records.sort_by_key(|record| record.pid != current);
    let mut owners = Vec::new();
    for instance in records {
        let modern = instance.workspace_windows.is_some();
        if !instance.workspace_windows.as_ref().map_or_else(
            || instance.workspace.as_deref() == Some(key),
            |windows| windows.iter().any(|w| w.workspace == key),
        ) {
            continue;
        }
        let pipe = match connect(instance.pid).await {
            Ok(pipe) => pipe,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        // Keep this process object alive through activation. Windows cannot recycle
        // the PID between the reply on this very pipe and the HWND lookup.
        let handle = match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, instance.pid) } {
            Ok(handle) => handle,
            Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => continue,
            Err(error) => return Err(error).context("Opening workspace process"),
        };
        let process = Arc::new(unsafe { OwnedHandle::from_raw_handle(handle.0) });
        let owner = WorkspaceOwner {
            pid: instance.pid,
            window_id: None,
            hwnd: 0,
            process: process.clone(),
        };
        let request = if modern {
            Request::WorkspaceWindows {
                key: key.to_owned(),
            }
        } else {
            Request::WorkspaceIdentity
        };
        let reply = match call_on(pipe, &request).await {
            Ok(reply) => reply,
            Err(_) if !owner.is_running()? => continue,
            Err(error) => return Err(error),
        };
        ensure!(
            reply.ok,
            "Could not verify workspace owner: {}",
            reply.error.unwrap_or_default()
        );
        if !owner.is_running()? {
            continue;
        }
        if modern {
            let matches: Vec<WorkspaceMatch> = serde_json::from_value(
                reply
                    .result
                    .context("Missing workspace window identities")?,
            )?;
            for matched in matches {
                ensure!(matched.window_id != 0, "Invalid workspace window ID");
                let window = WorkspaceOwner {
                    pid: owner.pid,
                    process: process.clone(),
                    window_id: Some(matched.window_id),
                    hwnd: matched.hwnd,
                };
                if window.pid == current && window.window_id == caller {
                    return Ok(Some(window));
                }
                owners.push(window);
            }
        } else if reply.result.as_ref().and_then(Value::as_str) == Some(key) {
            owners.push(owner);
        }
    }
    ensure!(
        owners.len() < 2,
        "Multiple windows have this workspace open (PIDs {}). Switch from the window you want to use.",
        owners
            .iter()
            .map(|owner| match owner.window_id {
                Some(id) => format!("{} window {id}", owner.pid),
                None => owner.pid.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(owners.pop())
}

fn discovery_records(directory: &Path) -> Result<Vec<Instance>> {
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
        if entry.file_name() == std::ffi::OsString::from(format!("{}.json", instance.pid))
            && instance.version == VERSION
            && instance.pipe == pipe_name(instance.pid)
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
    call_on(connect(pid).await?, request).await
}

async fn call_on(mut pipe: NamedPipeClient, request: &Request) -> Result<Reply> {
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

#[cfg(test)]
mod workspace_tests {
    use super::*;

    #[tokio::test]
    async fn verifies_workspace_identity_against_live_server_and_ignores_dead_pid() {
        let directory = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let server = Server::start_in_directory(
            Registry::for_workspace(Some("desk".into())),
            |_, _, _| unreachable!(),
            directory.path().into(),
        )
        .unwrap();
        assert_eq!(
            workspace_owner_in(directory.path(), "desk", None)
                .await
                .unwrap()
                .map(|owner| owner.pid),
            Some(pid)
        );
        let record = directory.path().join(format!("{pid}.json"));
        let old_hint = std::fs::read(&record).unwrap();
        let mut legacy: Value = serde_json::from_slice(&old_hint).unwrap();
        legacy.as_object_mut().unwrap().remove("workspace_windows");
        std::fs::write(&record, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let older = workspace_owner_in(directory.path(), "desk", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((older.pid, older.window_id), (pid, None));

        let sibling = server
            .registry
            .register_window(2, Some("desk".into()))
            .unwrap();
        assert!(
            workspace_owner_in(directory.path(), "desk", None)
                .await
                .is_err()
        );
        for id in [1, 2] {
            let exact = workspace_owner_in(directory.path(), "desk", Some(id))
                .await
                .unwrap()
                .unwrap();
            assert_eq!((exact.pid, exact.window_id), (pid, Some(id)));
        }
        sibling.unregister().unwrap();
        server.publish_workspace(Some("other".into())).unwrap();
        assert_eq!(
            workspace_owner_in(directory.path(), "other", None)
                .await
                .unwrap()
                .map(|owner| owner.pid),
            Some(pid)
        );
        std::fs::write(&record, &old_hint).unwrap();
        assert_eq!(
            workspace_owner_in(directory.path(), "desk", None)
                .await
                .unwrap()
                .map(|owner| owner.pid),
            None
        );
        drop(server);
        std::fs::write(&record, old_hint).unwrap();
        assert_eq!(
            workspace_owner_in(directory.path(), "desk", None)
                .await
                .unwrap()
                .map(|owner| owner.pid),
            None
        );
    }
}
