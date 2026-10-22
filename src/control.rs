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
    net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions},
    sync::{mpsc, oneshot},
};
use wezterm_term::TerminalSize;

pub const VERSION: u32 = 1;
pub const MAX_BYTES: usize = 1024 * 1024;
const MAX_LINE: usize = MAX_BYTES * 2;
const TIMEOUT: Duration = Duration::from_secs(10);
pub type CapturePng =
    Arc<dyn Fn(&Snapshot, TerminalSize, &Path) -> Result<(u32, u32)> + Send + Sync>;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Request {
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
pub struct Registry(Arc<Mutex<BTreeMap<u64, Pane>>>);
impl Registry {
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
                    if let Ok((frame, size)) = Self::capture(&pane).await {
                        sessions.push(json!({"id":pane.id,"label":pane.label,"active":pane.active,"title":frame.title,"status":frame.status,"cols":size.cols,"rows":size.rows}));
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
        let capture_png: CapturePng = Arc::new(capture_png);
        let directory = discovery_dir()?;
        std::fs::create_dir_all(&directory)?;
        let pid = std::process::id();
        let manifest = directory.join(format!("{pid}.json"));
        let (stop_tx, stop_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new().name("terminal-control".into()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            runtime.block_on(async move {
                let security = match PipeSecurity::new() { Ok(s)=>s, Err(e)=>{let _=ready_tx.send(Err(e)); return;} };
                let name = pipe_name(pid);
                let mut listener = match security.create(&name, true) {Ok(p)=>p,Err(e)=>{let _=ready_tx.send(Err(e));return;}};
                let _ = ready_tx.send(Ok(()));
                let permits = Arc::new(tokio::sync::Semaphore::new(8));
                tokio::pin!(stop_rx);
                loop {
                    tokio::select! {
                        _ = &mut stop_rx => break,
                        connected = listener.connect() => {
                            if let Err(e) = connected { log::error!("control pipe: {e}"); break; }
                            let next = match security.create(&name, false) {Ok(p)=>p,Err(e)=>{log::error!("control pipe: {e:#}");break;}};
                            let client = std::mem::replace(&mut listener, next);
                            if let Ok(permit) = permits.clone().try_acquire_owned() {
                                let registry = registry.clone();
                                let capture_png=capture_png.clone();
                                tokio::spawn(async move { let _permit=permit; let _=serve(client, registry, capture_png).await; });
                            }
                        }
                    }
                }
            });
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

async fn serve(pipe: NamedPipeServer, registry: Registry, capture_png: CapturePng) -> Result<()> {
    let (read, mut write) = tokio::io::split(pipe);
    let mut read = BufReader::new(read).take((MAX_LINE + 1) as u64);
    let mut line = Vec::new();
    tokio::time::timeout(TIMEOUT, read.read_until(b'\n', &mut line)).await??;
    let result = if line.len() > MAX_LINE {
        Err(anyhow::anyhow!("request exceeds 2 MiB"))
    } else if !line.ends_with(b"\n") {
        Err(anyhow::anyhow!("request must end with newline"))
    } else {
        match serde_json::from_slice::<Request>(&line) {
            Ok(r) => registry.execute(r, capture_png).await,
            Err(e) => Err(e.into()),
        }
    };
    let mut bytes = serde_json::to_vec(&Reply::from_result(result))?;
    bytes.push(b'\n');
    tokio::time::timeout(TIMEOUT, write.write_all(&bytes)).await??;
    Ok(())
}

async fn connect(pid: u32) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    let start = std::time::Instant::now();
    loop {
        match ClientOptions::new().open(pipe_name(pid)) {
            Ok(pipe) => return Ok(pipe),
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

// The pipe grants access to the process user and SYSTEM, and rejects remote clients.
// The descriptor is kept alive through each synchronous CreateNamedPipe call.
struct PipeSecurity(windows::Win32::Security::PSECURITY_DESCRIPTOR);
unsafe impl Send for PipeSecurity {}
unsafe impl Sync for PipeSecurity {}
impl PipeSecurity {
    fn new() -> Result<Self> {
        use windows::{
            Win32::{
                Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree},
                Security::{
                    Authorization::{
                        ConvertSidToStringSidW,
                        ConvertStringSecurityDescriptorToSecurityDescriptorW,
                    },
                    GetTokenInformation, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser,
                },
                System::Threading::{GetCurrentProcess, OpenProcessToken},
            },
            core::{PCWSTR, PWSTR},
        };
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
            let mut length = 0;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut length);
            let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
            let result = GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                length,
                &mut length,
            );
            let _ = CloseHandle(token);
            result?;
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let mut sid = PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut sid)?;
            let sid_text = sid.to_string();
            LocalFree(Some(HLOCAL(sid.0.cast())));
            let sddl: Vec<u16> = format!("D:P(A;;GA;;;SY)(A;;GA;;;{})", sid_text?)
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                1,
                &mut descriptor,
                None,
            )?;
            Ok(Self(descriptor))
        }
    }
    fn create(&self, name: &str, first: bool) -> Result<NamedPipeServer> {
        let mut attributes = windows::Win32::Security::SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<windows::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        };
        // SAFETY: attributes/descriptor remain valid throughout the synchronous call.
        Ok(unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    name,
                    (&mut attributes as *mut windows::Win32::Security::SECURITY_ATTRIBUTES).cast(),
                )?
        })
    }
}
impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe {
            windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
                self.0.0,
            )));
        }
    }
}
