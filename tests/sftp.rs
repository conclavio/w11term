//! Real SSH + SFTP packets, a temporary server filesystem, and the public CLI/pipe.
use russh::{Channel, ChannelId, server};
use russh_sftp::protocol::*;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use w11term::{
    control::{self, Pane, Registry, Request},
    options::{Authentication, Source, SshOptions},
    session::{Command, Session},
};

struct Peer {
    channels: HashMap<ChannelId, Channel<server::Msg>>,
    root: PathBuf,
    opens: Arc<AtomicUsize>,
    reject: bool,
}
impl server::Handler for Peer {
    type Error = anyhow::Error;
    async fn auth_none(&mut self, _: &str) -> anyhow::Result<server::Auth> {
        Ok(server::Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> anyhow::Result<()> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn pty_request(
        &mut self,
        c: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        s: &mut server::Session,
    ) -> anyhow::Result<()> {
        s.channel_success(c)?;
        Ok(())
    }
    #[cfg(feature = "ui-smoke")]
    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        session: &mut server::Session,
    ) -> anyhow::Result<()> {
        if self.root.join("keyboard-input.bin").exists() {
            session.data(
                channel,
                format!("\r\nssh-grid-{cols}x{rows}\r\n").into_bytes(),
            )?;
        }
        Ok(())
    }
    async fn exec_request(
        &mut self,
        c: ChannelId,
        command: &[u8],
        s: &mut server::Session,
    ) -> anyhow::Result<()> {
        let trace = self.root.join("workspace-exec.bin");
        if !trace.exists() {
            s.channel_failure(c)?;
            return Ok(());
        }
        fs::OpenOptions::new()
            .append(true)
            .open(trace)?
            .write_all(command)?;
        let drop_once = self.root.join("drop-before-exec-reply");
        if drop_once.exists() {
            fs::remove_file(drop_once)?;
            s.disconnect(
                russh::Disconnect::ByApplication,
                "fixture lost link before exec reply",
                "en",
            )?;
            return Ok(());
        }
        s.channel_success(c)?;
        s.data(c, b"workspace-command-started\r\n".as_slice())?;
        Ok(())
    }
    async fn shell_request(&mut self, c: ChannelId, s: &mut server::Session) -> anyhow::Result<()> {
        s.channel_success(c)?;
        s.data(c, b"ready\r\n".as_slice())?;
        if self.root.join("shell directory").is_dir() {
            s.data(
                c,
                b"\x1b]133;A\x07$ \x1b]133;B\x07status\r\n\x1b]133;C\x07".as_slice(),
            )?;
            s.data(
                c,
                b"\x1b[35mfixture status\x1b[0m\r\n\x1b]133;D;7\x07".as_slice(),
            )?;
            s.data(
                c,
                b"src/trace.rs:11:1\r\n\x1b]7;file://127.0.0.1/shell%20directory\x1b\\".as_slice(),
            )?;
        }
        Ok(())
    }
    async fn data(
        &mut self,
        c: ChannelId,
        data: &[u8],
        s: &mut server::Session,
    ) -> anyhow::Result<()> {
        if self.channels.contains_key(&c) {
            #[cfg(feature = "ui-smoke")]
            if self.root.join("keyboard-input.bin").exists() {
                fs::OpenOptions::new()
                    .append(true)
                    .open(self.root.join("keyboard-input.bin"))?
                    .write_all(data)?;
            }
            s.data(c, data.to_vec())?;
        }
        Ok(())
    }
    async fn channel_eof(&mut self, c: ChannelId, s: &mut server::Session) -> anyhow::Result<()> {
        s.close(c)?;
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        c: ChannelId,
        name: &str,
        s: &mut server::Session,
    ) -> anyhow::Result<()> {
        if self.reject || name != "sftp" {
            s.channel_failure(c)?;
            return Ok(());
        }
        s.channel_success(c)?;
        let channel = self.channels.remove(&c).unwrap();
        let peer = Files {
            root: self.root.clone(),
            ssh: s.handle(),
            handles: HashMap::new(),
            next: 0,
        };
        tokio::spawn(async move {
            russh_sftp::server::run(channel.into_stream(), peer).await;
        });
        Ok(())
    }
}
enum Opened {
    File(fs::File),
    Dir(Vec<russh_sftp::protocol::File>),
}
struct Files {
    root: PathBuf,
    ssh: server::Handle,
    handles: HashMap<String, Opened>,
    next: u64,
}
fn status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: String::new(),
    }
}
fn io(e: std::io::Error) -> StatusCode {
    if e.kind() == std::io::ErrorKind::NotFound {
        StatusCode::NoSuchFile
    } else {
        StatusCode::Failure
    }
}
impl Files {
    fn path(&self, path: &str) -> PathBuf {
        assert!(!path.contains("..") && !path.contains(':'));
        self.root.join(path.trim_start_matches('/'))
    }
    fn add(&mut self, id: u32, h: Opened) -> Handle {
        self.next += 1;
        let handle = self.next.to_string();
        self.handles.insert(handle.clone(), h);
        Handle { id, handle }
    }
    fn file(&mut self, h: &str) -> Result<&mut fs::File, StatusCode> {
        match self.handles.get_mut(h) {
            Some(Opened::File(f)) => Ok(f),
            _ => Err(StatusCode::Failure),
        }
    }
}
impl russh_sftp::server::Handler for Files {
    type Error = StatusCode;
    async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, StatusCode> {
        let mut version = Version::new();
        if self.root.join("small-reads").exists() {
            version
                .extensions
                .insert("limits@openssh.com".into(), "1".into());
        }
        Ok(version)
    }
    async fn extended(
        &mut self,
        id: u32,
        request: String,
        _: Vec<u8>,
    ) -> Result<Packet, StatusCode> {
        if request != "limits@openssh.com" || !self.root.join("small-reads").exists() {
            return Err(StatusCode::OpUnsupported);
        }
        let data = [65536u64, 512, 32768, 128]
            .into_iter()
            .flat_map(u64::to_be_bytes)
            .collect();
        Ok(Packet::ExtendedReply(ExtendedReply { id, data }))
    }
    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }
    async fn open(
        &mut self,
        id: u32,
        path: String,
        flags: OpenFlags,
        _: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        let file = fs::OpenOptions::from(flags)
            .open(self.path(&path))
            .map_err(io)?;
        Ok(self.add(id, Opened::File(file)))
    }
    async fn close(&mut self, id: u32, h: String) -> Result<Status, StatusCode> {
        self.handles.remove(&h).ok_or(StatusCode::Failure)?;
        Ok(status(id))
    }
    async fn read(
        &mut self,
        id: u32,
        h: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        if self.root.join("small-reads").exists() && len > 512 {
            return Err(StatusCode::Failure);
        }
        fs::write(self.root.join("read-entered"), []).map_err(io)?;
        while offset > 0 && self.root.join("hold-next-chunk").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Keep the SFTP channel busy long enough to verify independent terminal input.
        tokio::time::sleep(Duration::from_millis(20)).await;
        if self.root.join("pause-reads").exists() {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let oversized = self.root.join("oversized-reads").exists();
        let file = self.file(&h)?;
        file.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut data = vec![0; len as usize + usize::from(oversized)];
        let n = file.read(&mut data).map_err(io)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        data.truncate(n);
        Ok(Data { id, data })
    }
    async fn write(
        &mut self,
        id: u32,
        h: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusCode> {
        if offset > 0 && self.root.join("disconnect-next-write").exists() {
            let _ = self
                .ssh
                .disconnect(
                    russh::Disconnect::ByApplication,
                    "test connection loss".into(),
                    "en".into(),
                )
                .await;
            return Err(StatusCode::ConnectionLost);
        }
        if offset > 0 && self.root.join("fail-next-write").exists() {
            return Err(StatusCode::Failure);
        }
        let f = self.file(&h)?;
        f.seek(SeekFrom::Start(offset)).map_err(io)?;
        f.write_all(&data).map_err(io)?;
        Ok(status(id))
    }
    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        Ok(Attrs {
            id,
            attrs: (&fs::metadata(self.path(&path)).map_err(io)?).into(),
        })
    }
    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.stat(id, path).await
    }
    async fn fstat(&mut self, id: u32, h: String) -> Result<Attrs, StatusCode> {
        if self.root.join("no-fstat").exists() {
            return Err(StatusCode::OpUnsupported);
        }
        Ok(Attrs {
            id,
            attrs: (&self.file(&h)?.metadata().map_err(io)?).into(),
        })
    }
    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        if let Some(size) = attrs.size {
            fs::OpenOptions::new()
                .write(true)
                .open(self.path(&path))
                .map_err(io)?
                .set_len(size)
                .map_err(io)?;
        }
        Ok(status(id))
    }
    async fn fsetstat(
        &mut self,
        id: u32,
        h: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        if let Some(size) = attrs.size {
            self.file(&h)?.set_len(size).map_err(io)?;
        }
        Ok(status(id))
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        let entries = fs::read_dir(self.path(&path))
            .map_err(io)?
            .map(|e| {
                let e = e.unwrap();
                russh_sftp::protocol::File::new(
                    e.file_name().to_string_lossy(),
                    (&e.metadata().unwrap()).into(),
                )
            })
            .collect();
        Ok(self.add(id, Opened::Dir(entries)))
    }
    async fn readdir(&mut self, id: u32, h: String) -> Result<Name, StatusCode> {
        match self.handles.get_mut(&h) {
            Some(Opened::Dir(entries)) if !entries.is_empty() => Ok(Name {
                id,
                files: entries.drain(..1).collect(),
            }),
            Some(Opened::Dir(_)) => Err(StatusCode::Eof),
            _ => Err(StatusCode::Failure),
        }
    }
    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _: FileAttributes,
    ) -> Result<Status, StatusCode> {
        fs::create_dir(self.path(&path)).map_err(io)?;
        Ok(status(id))
    }
    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        fs::remove_dir(self.path(&path)).map_err(io)?;
        Ok(status(id))
    }
    async fn remove(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        fs::remove_file(self.path(&path)).map_err(io)?;
        Ok(status(id))
    }
    async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, StatusCode> {
        fs::rename(self.path(&from), self.path(&to)).map_err(io)?;
        Ok(status(id))
    }
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        Ok(Name {
            id,
            files: vec![russh_sftp::protocol::File::dummy(if path == "." {
                "/".into()
            } else {
                path
            })],
        })
    }
}

type FixtureServer = (
    Source,
    tempfile::TempDir,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
);
async fn fixture_server(reject: bool) -> FixtureServer {
    fixture_connections(reject, 1).await
}
async fn fixture_connections(reject: bool, connections: usize) -> FixtureServer {
    use russh::keys::{Algorithm, HashAlg, PrivateKey};
    let root = tempfile::tempdir().unwrap();
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let opens = Arc::new(AtomicUsize::new(0));
    let root_path = root.path().to_owned();
    let opens_for_server = opens.clone();
    let server = tokio::spawn(async move {
        let mut peers = tokio::task::JoinSet::new();
        for _ in 0..connections {
            let (stream, _) = listener.accept().await.unwrap();
            let peer = Peer {
                channels: HashMap::new(),
                root: root_path.clone(),
                opens: opens_for_server.clone(),
                reject,
            };
            let config = config.clone();
            peers.spawn(async move {
                let _ = server::run_stream(config, stream, peer)
                    .await
                    .unwrap()
                    .await;
            });
        }
        while let Some(done) = peers.join_next().await {
            done.unwrap();
        }
    });
    let source = Source::Ssh(SshOptions {
        host: "127.0.0.1".into(),
        user: "test".into(),
        port,
        authentication: Authentication::None,
        host_key: Some(pin),
        proxy_jump: Vec::new(),
        durable: false,
        player: Default::default(),
    });
    (source, root, opens, server)
}

async fn fixture(
    reject: bool,
) -> (
    Session,
    Source,
    tempfile::TempDir,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let (source, root, opens, server) = fixture_server(reject).await;
    let session = Session::start(source.clone());
    tokio::time::timeout(Duration::from_secs(5), async {
        while session.frames.load().connection_generation == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("SSH fixture did not connect");
    (session, source, root, opens, server)
}

#[cfg(feature = "ui-smoke")]
#[path = "sftp/native_gui.rs"]
mod native_gui;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_workspace_exec_reply_does_not_dispatch_command_again() {
    let (Source::Ssh(mut ssh), remote, opens, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    ssh.player.reconnect = true;
    let trace = remote.path().join("workspace-exec.bin");
    fs::write(&trace, []).unwrap();
    fs::write(remote.path().join("drop-before-exec-reply"), []).unwrap();
    let command = "make deploy";
    let startup = w11term::workspace::SshStartup::new(&ssh, String::new(), command.into()).unwrap();
    let session = Session::start_workspace_ssh(ssh, Default::default(), false, startup);
    tokio::time::timeout(Duration::from_secs(10), async {
        while !session
            .frames
            .load()
            .status
            .contains("Workspace command interrupted")
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "Expected final interruption status, got {:?}; channels {}, recorded {:?}",
            session.frames.load().status,
            opens.load(Ordering::Relaxed),
            fs::read_to_string(&trace)
        )
    });
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    assert_eq!(fs::read(trace).unwrap(), command.as_bytes());
    drop(session);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directory_only_workspace_reconnects_to_an_interactive_shell() {
    let (Source::Ssh(mut ssh), remote, opens, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    ssh.player.reconnect = true;
    let trace = remote.path().join("workspace-exec.bin");
    fs::write(&trace, []).unwrap();
    fs::write(remote.path().join("drop-before-exec-reply"), []).unwrap();
    let startup = w11term::workspace::SshStartup::new(&ssh, "/work".into(), String::new()).unwrap();
    let session = Session::start_workspace_ssh(ssh, Default::default(), false, startup);
    tokio::time::timeout(Duration::from_secs(10), async {
        while session.frames.load().connection_generation != 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("directory-only shell should reconnect after the first exec loses its reply");
    assert_eq!(opens.load(Ordering::Relaxed), 2);
    assert_eq!(
        fs::read_to_string(trace).unwrap(),
        r#"sh -c 'cd "$1" && exec "${SHELL:-/bin/sh}" -i' w11term '/work'"#.repeat(2)
    );
    drop(session);
    server.abort();
}

async fn request(op: Value) -> control::Reply {
    let request: Request =
        serde_json::from_value(json!({"verb":"sftp","pane":1,"timeout_seconds":10,"operation":op}))
            .unwrap();
    control::call(Some(std::process::id()), &request)
        .await
        .unwrap()
}
async fn ok(op: Value) -> Value {
    let r = request(op).await;
    assert!(r.ok, "{r:?}");
    r.result.unwrap()["sftp"].clone()
}
fn registry(session: &Session) -> Registry {
    let r = Registry::default();
    r.replace([Pane {
        id: 1,
        tab_id: 4,
        label: "test".into(),
        active: false,
        visible: false,
        input: session.input.clone(),
    }]);
    r
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_panes_have_independent_sftp_handles_on_one_tcp_connection() {
    use w11term::sftp::{Job, Operation};

    async fn request(pane: &Session, operation: Operation) -> Result<Value, String> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        pane.input
            .send(Command::Sftp(Job {
                operation,
                deadline,
                reply,
                progress: None,
            }))
            .unwrap();
        tokio::time::timeout_at(deadline, response)
            .await
            .unwrap()
            .unwrap()
    }

    // The fixture accepts exactly one TCP connection.
    let (original, source, root, opens, server) = fixture(false).await;
    fs::write(root.path().join("from-second-pane"), b"").unwrap();
    let child = Session::start_configured_with_connection(
        source,
        Default::default(),
        false,
        original.ssh_connection(),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while child.ssh_connection().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("second pane must use the fixture's single connection");
    let remote = request(&child, Operation::Opendir { path: "/".into() })
        .await
        .unwrap();
    let handle = remote["handle"].as_u64().unwrap();
    assert_eq!(
        request(&original, Operation::Info).await.unwrap()["open_handles"],
        0
    );
    assert!(
        request(&original, Operation::Readdir { handle })
            .await
            .is_err()
    );
    assert_eq!(
        opens.load(Ordering::Relaxed),
        4,
        "two shell and two SFTP channels"
    );
    drop(original);
    let result = request(&child, Operation::Readdir { handle })
        .await
        .unwrap();
    assert_eq!(result["entries"][0]["filename"], "from-second-pane");
    request(&child, Operation::Close { handle }).await.unwrap();
    drop(child);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drawer_reports_confirmed_transfer_bytes_and_keeps_remote_errors() {
    use tokio::sync::{oneshot, watch};
    use w11term::sftp::{CHUNK, Job, Operation, TransferProgress};

    fn start(
        session: &Session,
        operation: Operation,
    ) -> (
        watch::Receiver<TransferProgress>,
        oneshot::Receiver<Result<Value, String>>,
    ) {
        let (sender, progress) = watch::channel(TransferProgress::default());
        let (reply, response) = oneshot::channel();
        session
            .input
            .send(Command::Sftp(Job {
                operation,
                deadline: tokio::time::Instant::now() + Duration::from_secs(5),
                reply,
                progress: Some(sender),
            }))
            .unwrap();
        (progress, response)
    }

    let (session, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let size = CHUNK * 2 + 123;
    let bytes: Vec<_> = (0..size).map(|i| i as u8).collect();
    fs::write(remote.path().join("source"), &bytes).unwrap();
    let destination = local.path().join("download");
    let download = || Operation::Get {
        remote: "/source".into(),
        local: destination.clone(),
        overwrite: true,
        max_bytes: None,
    };
    let hold = remote.path().join("hold-next-chunk");
    fs::write(&hold, []).unwrap();
    fs::write(&destination, b"previous copy").unwrap();
    let (mut progress, response) = start(&session, download());
    let expected_total = Some(size as u64);
    let partial = tokio::time::timeout(Duration::from_secs(5), progress.wait_for(|p| p.bytes > 0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        *partial,
        TransferProgress {
            bytes: CHUNK as u64,
            total: expected_total
        }
    );
    drop(partial);
    assert_eq!(fs::read(&destination).unwrap(), b"previous copy");
    fs::remove_file(hold).unwrap();
    assert_eq!(response.await.unwrap().unwrap()["bytes_transferred"], size);
    assert_eq!(
        *progress.borrow(),
        TransferProgress {
            bytes: size as u64,
            total: expected_total
        }
    );
    assert_eq!(fs::read(&destination).unwrap(), bytes);

    fs::write(remote.path().join("no-fstat"), []).unwrap();
    let (progress, response) = start(&session, download());
    assert_eq!(response.await.unwrap().unwrap()["bytes_transferred"], size);
    assert_eq!(
        *progress.borrow(),
        TransferProgress {
            bytes: size as u64,
            total: None
        }
    );

    let upload = || Operation::Put {
        local: destination.clone(),
        remote: "/upload".into(),
        overwrite: false,
    };
    let fail = remote.path().join("fail-next-write");
    fs::write(&fail, []).unwrap();
    let (progress, response) = start(&session, upload());
    assert!(
        response
            .await
            .unwrap()
            .unwrap_err()
            .contains("partial file")
    );
    assert_eq!(
        *progress.borrow(),
        TransferProgress {
            bytes: CHUNK as u64,
            total: expected_total
        }
    );
    let uploaded = remote.path().join("upload");
    assert_eq!(fs::read(&uploaded).unwrap(), &bytes[..CHUNK]);
    fs::remove_file(fail).unwrap();
    // A retry preserves a partial remote destination until overwrite is requested.
    let (_, response) = start(&session, upload());
    assert!(response.await.unwrap().is_err());
    assert_eq!(fs::read(&uploaded).unwrap(), &bytes[..CHUNK]);
    let (progress, response) = start(
        &session,
        Operation::Put {
            local: destination.clone(),
            remote: "/upload".into(),
            overwrite: true,
        },
    );
    assert_eq!(response.await.unwrap().unwrap()["bytes_transferred"], size);
    assert_eq!(
        *progress.borrow(),
        TransferProgress {
            bytes: size as u64,
            total: expected_total
        }
    );
    assert_eq!(fs::read(uploaded).unwrap(), bytes);
    fs::write(remote.path().join("disconnect-next-write"), []).unwrap();
    let (progress, response) = start(
        &session,
        Operation::Put {
            local: destination,
            remote: "/dropped".into(),
            overwrite: false,
        },
    );
    let error = response
        .await
        .expect("admitted upload lost its reply")
        .unwrap_err();
    assert!(error.contains("partial"), "{error}");
    assert_eq!(progress.borrow().bytes, CHUNK as u64);
    assert_eq!(
        fs::read(remote.path().join("dropped")).unwrap(),
        &bytes[..CHUNK]
    );
    drop(session);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asset_pack_sync_verifies_hashes_reuses_cache_and_preserves_old_pack_on_failure() {
    use sha2::{Digest, Sha256};
    let (session, _, root, opens, server) = fixture(false).await;
    let cache = tempfile::tempdir().unwrap();
    fs::write(root.path().join("small-reads"), []).unwrap();
    fs::create_dir_all(root.path().join("assets/sounds")).unwrap();
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&836u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&800u32.to_le_bytes());
    wav.extend_from_slice(&[128; 800]);
    let hash: String = Sha256::digest(&wav)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    fs::write(root.path().join("assets/sounds/shot.wav"), &wav).unwrap();
    let manifest = |version: &str, hash: &str| json!({"version":1,"pack_version":version,"files":[{"name":"shot","path":"sounds/shot.wav","kind":"sound","sha256":hash,"size":wav.len()}]});
    fs::write(
        root.path().join("assets/manifest.json"),
        serde_json::to_vec(&manifest("pack1", &hash)).unwrap(),
    )
    .unwrap();
    let (input, mut jobs) = tokio::sync::mpsc::unbounded_channel();
    let commands = session.input.clone();
    let bridge = tokio::spawn(async move {
        while let Some(w11term::terminal::TransportInput::Sftp(job)) = jobs.recv().await {
            commands.send(Command::Sftp(job)).unwrap();
        }
    });
    let help = b"pilot help";
    let help_hash: String = Sha256::digest(help)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    fs::write(root.path().join("assets/help.txt"), help).unwrap();
    let mut initial = manifest("pack1", &hash);
    initial["files"].as_array_mut().unwrap().push(json!({"name":"help","path":"docs/help.txt","kind":"help","sha256":help_hash,"size":help.len()}));
    fs::create_dir_all(root.path().join("assets/docs")).unwrap();
    fs::write(root.path().join("assets/docs/help.txt"), help).unwrap();
    fs::write(
        root.path().join("assets/manifest.json"),
        serde_json::to_vec(&initial).unwrap(),
    )
    .unwrap();
    let store = w11term::assets::Store::default();
    let sync = |version: &str| {
        w11term::assets::sync_into(
            input.clone(),
            "test@loopback:22".into(),
            version.into(),
            store.clone(),
            cache.path().into(),
        )
    };
    assert_eq!(sync("pack1").await.unwrap(), 2);
    let path = store.sound("shot").unwrap();
    assert_eq!(fs::read(&path).unwrap(), wav);
    fs::remove_file(root.path().join("assets/sounds/shot.wav")).unwrap();
    assert_eq!(
        sync("pack1").await.unwrap(),
        0,
        "unchanged content must not be fetched again"
    );
    fs::write(root.path().join("assets/sounds/shot.wav"), &wav).unwrap();
    fs::write(
        root.path().join("assets/manifest.json"),
        serde_json::to_vec(&manifest("pack2", &"0".repeat(64))).unwrap(),
    )
    .unwrap();
    assert!(
        sync("pack2")
            .await
            .unwrap_err()
            .to_string()
            .contains("mismatch")
    );
    let help_path = path.parent().unwrap().join(&help_hash);
    assert_eq!(
        fs::read(&help_path).unwrap(),
        help,
        "failed replacement must retain non-sound files"
    );
    fs::write(
        root.path().join("assets/manifest.json"),
        serde_json::to_vec(&manifest("pack3", &hash)).unwrap(),
    )
    .unwrap();
    let other = w11term::assets::Store::default();
    w11term::assets::sync_into(
        input.clone(),
        "test@loopback:22".into(),
        "pack3".into(),
        other,
        cache.path().into(),
    )
    .await
    .unwrap();
    assert!(
        help_path.exists(),
        "a second pane must not prune the first pane's generation"
    );
    assert_eq!(store.version(), "pack1");
    assert_eq!(store.sound("shot"), Some(path));
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "asset traffic must reuse the established SSH and SFTP channels"
    );
    bridge.abort();
    drop(session);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorder_events_and_wait_for_text_share_the_terminal_output() {
    let (session, _, _root, _opens, server) = fixture(false).await;
    session.player.write().unwrap().mute = true;
    session.player.write().unwrap().remote_sounds = Some(true);
    let registry = registry(&session);
    let capture: w11term::control::CapturePng = Arc::new(|_, _, _| anyhow::bail!("unused"));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bug.vt");
    registry
        .execute(
            Request::Record {
                pane: 1,
                path: Some(path.clone()),
            },
            capture.clone(),
        )
        .await
        .unwrap();
    let bytes=b"recorded fps=144\x07\x1b]0;Event title\x1b\\\x1b]9;Test notification\x1b\\\x1b]440;1;play;shot\x1b\\";
    let (reply, response) = tokio::sync::oneshot::channel();
    session
        .input
        .send(Command::Raw(bytes.to_vec(), reply))
        .unwrap();
    response.await.unwrap().unwrap();
    registry
        .execute(
            Request::WaitForText {
                pane: 1,
                pattern: "fps=144".into(),
                regex: false,
                timeout_seconds: 3,
            },
            capture.clone(),
        )
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut cursor = 0;
    let mut events = Vec::new();
    loop {
        let batch = session.events.wait(cursor, Duration::from_secs(1)).await;
        cursor = batch.next;
        events.extend(batch.events);
        if events.iter().any(|e| e.kind == "sound") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
    }
    assert!(events.iter().any(|e| e.kind == "bell"));
    assert!(
        events
            .iter()
            .any(|e| e.kind == "title" && e.data["title"] == "Event title")
    );
    assert!(
        events
            .iter()
            .any(|e| e.kind == "notification" && e.data["body"] == "Test notification")
    );
    assert!(
        events
            .iter()
            .filter(|e| e.kind == "sound")
            .all(|e| e.data["outcome"] == "muted")
    );
    let result = registry
        .execute(
            Request::Record {
                pane: 1,
                path: None,
            },
            capture.clone(),
        )
        .await
        .unwrap();
    assert_eq!(result["bytes"], bytes.len());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(path.with_extension("vt.json").is_file());
    assert!(
        registry
            .execute(
                Request::Record {
                    pane: 1,
                    path: Some(path)
                },
                capture
            )
            .await
            .is_err(),
        "recordings must not overwrite an existing bug capture"
    );
    drop(session);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfers_handles_and_rejected_subsystem_without_reconnect() {
    tokio::time::timeout(Duration::from_secs(30),async {
        let (session,_,root,opens,server)=fixture(false).await;
        let panes=registry(&session);
        let discovery=tempfile::tempdir().unwrap();
        let control=control::Server::start_in_directory(panes,|_,_,_|anyhow::bail!("unused"),discovery.path().into()).unwrap();
        let local=tempfile::tempdir().unwrap();let source=local.path().join("binary source.bin");let dest=local.path().join("download.bin");
        let bytes:Vec<u8>=(0..200_003).map(|i|(i%256) as u8).collect();fs::write(&source,&bytes).unwrap();
        assert_eq!(ok(json!({"op":"info"})).await["version"],3);
        ok(json!({"op":"mkdir","path":"/folder"})).await;
        ok(json!({"op":"put","local":source,"remote":"/folder/日本 file.bin"})).await;
        assert!(!request(json!({"op":"put","local":source,"remote":"/folder/日本 file.bin"})).await.ok);
        assert_eq!(ok(json!({"op":"stat","path":"/folder/日本 file.bin"})).await["attrs"]["size"],bytes.len());
        let download=tokio::spawn({let dest=dest.clone();async move{ok(json!({"op":"get","remote":"/folder/日本 file.bin","local":dest})).await}});
        tokio::time::sleep(Duration::from_millis(40)).await;
        let (tx,rx)=tokio::sync::oneshot::channel();session.input.send(Command::Raw(b"interactive".to_vec(),tx)).unwrap();
        tokio::time::timeout(Duration::from_millis(100),rx).await.expect("SFTP blocked terminal input").unwrap().unwrap();
        assert!(!download.is_finished(),"test transfer finished before input assertion");download.await.unwrap();assert_eq!(fs::read(&dest).unwrap(),bytes);
        assert!(session.frames.load().rows.iter().any(|l|l.as_str().contains("interactive")),"shell echo was not processed during SFTP");
        let limited=local.path().join("limited.bin");
        let reply=request(json!({"op":"get","remote":"/folder/日本 file.bin","local":limited,"max_bytes":100})).await;
        assert!(!reply.ok && reply.error.unwrap().contains("max-bytes")); assert!(!limited.exists());
        fs::write(root.path().join("oversized-reads"),[]).unwrap();
        let reply=request(json!({"op":"get","remote":"/folder/日本 file.bin","local":limited})).await;
        assert!(!reply.ok && reply.error.unwrap().contains("more SFTP data")); assert!(!limited.exists());
        fs::remove_file(root.path().join("oversized-reads")).unwrap();
        fs::write(&dest,b"preserved").unwrap();assert!(!request(json!({"op":"get","remote":"/folder/日本 file.bin","local":dest})).await.ok);assert_eq!(fs::read(&dest).unwrap(),b"preserved");
        ok(json!({"op":"get","remote":"/folder/日本 file.bin","local":dest,"overwrite":true})).await;assert_eq!(fs::read(&dest).unwrap(),bytes);
        let h=ok(json!({"op":"opendir","path":"/folder"})).await["handle"].as_u64().unwrap();
        assert_eq!(ok(json!({"op":"readdir","handle":h})).await["entries"][0]["filename"],"日本 file.bin");
        assert_eq!(ok(json!({"op":"readdir","handle":h})).await["eof"],true);ok(json!({"op":"close","handle":h})).await;
        assert!(!request(json!({"op":"readdir","handle":h})).await.ok);
        let h=ok(json!({"op":"open","path":"/folder/日本 file.bin","flags":["read","write"]})).await["handle"].as_u64().unwrap();
        fs::write(root.path().join("oversized-reads"),[]).unwrap();
        assert!(!request(json!({"op":"read","handle":h,"offset":0,"len":3})).await.ok);
        fs::remove_file(root.path().join("oversized-reads")).unwrap();
        // Abandon an admitted read without resetting the shared channel or its handles.
        let _=fs::remove_file(root.path().join("read-entered"));
        fs::write(root.path().join("pause-reads"),[]).unwrap();
        let (reply,response)=tokio::sync::oneshot::channel();
        session.input.send(Command::Sftp(w11term::sftp::Job{operation:w11term::sftp::Operation::Read{handle:h,offset:0,len:3},deadline:tokio::time::Instant::now()+Duration::from_secs(5),reply,progress:None})).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !root.path().join("read-entered").exists() { tokio::task::yield_now().await; }
        }).await.unwrap();
        drop(response);
        assert_eq!(ok(json!({"op":"info"})).await["open_handles"],1);
        fs::remove_file(root.path().join("pause-reads")).unwrap();
        ok(json!({"op":"write","handle":h,"offset":1,"data_base64":"AAf/"})).await;
        assert_eq!(ok(json!({"op":"read","handle":h,"offset":1,"len":3})).await["data_base64"],"AAf/");
        ok(json!({"op":"fsetstat","handle":h,"attrs":{"size":4}})).await;
        assert_eq!(ok(json!({"op":"fstat","handle":h})).await["attrs"]["size"],4);ok(json!({"op":"close","handle":h})).await;
        assert_eq!(ok(json!({"op":"ls","path":"/folder"})).await["entries"].as_array().unwrap().len(),1);
        ok(json!({"op":"rename","from":"/folder/日本 file.bin","to":"/folder/renamed"})).await;
        assert!(!request(json!({"op":"remove-dir","path":"/folder"})).await.ok);
        ok(json!({"op":"remove-file","path":"/folder/renamed"})).await;ok(json!({"op":"remove-dir","path":"/folder"})).await;
        assert!(!request(json!({"op":"stat","path":"/missing"})).await.ok);
        assert!(!request(json!({"op":"limits"})).await.ok);
        // Exercise the actual console parser/process through the same running pipe.
        let pid=std::process::id().to_string();
        let output=tokio::task::spawn_blocking(move||std::process::Command::new(env!("CARGO_BIN_EXE_w11term-cli")).args(["--pid",&pid,"sftp","--pane","1","info"]).output().unwrap()).await.unwrap();
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stdout));
        assert_eq!(opens.load(Ordering::Relaxed),2,"SFTP must reuse one sibling channel on the existing SSH connection");
        fs::remove_file(root.path().join("read-entered")).unwrap();
        assert_eq!(fs::read_dir(root.path()).unwrap().count(),0);
        fs::write(root.path().join("slow"),b"slow").unwrap();
        let handle=ok(json!({"op":"open","path":"/slow","flags":["read"]})).await["handle"].as_u64().unwrap();
        fs::write(root.path().join("pause-reads"),b"").unwrap();
        let timeout_request:Request=serde_json::from_value(json!({"verb":"sftp","pane":1,"timeout_seconds":1,"operation":{"op":"get","remote":"/slow","local":local.path().join("cancelled")}})).unwrap();
        let reply=control::call(Some(std::process::id()),&timeout_request).await.unwrap();
        assert!(!reply.ok);assert!(reply.error.unwrap().contains("timed out"));
        assert!(!local.path().join("cancelled").exists(),"failed download published a partial destination");
        fs::remove_file(root.path().join("pause-reads")).unwrap();
        assert!(!request(json!({"op":"fstat","handle":handle})).await.ok,"timeout must invalidate prior handles");
        ok(json!({"op":"info"})).await;
        assert_eq!(opens.load(Ordering::Relaxed),3,"recovery opens only a new SFTP channel, not a new SSH transport");
        drop(control);drop(session);server.await.unwrap();
        let (session,_,_root,_opens,server)=fixture(true).await;
        let control=control::Server::start_in_directory(registry(&session),|_,_,_|anyhow::bail!("unused"),discovery.path().into()).unwrap();
        let r=request(json!({"op":"info"})).await;assert!(!r.ok);assert!(r.error.unwrap().contains("rejected"));
        let (tx,rx)=tokio::sync::oneshot::channel();session.input.send(Command::Raw(b"still here".to_vec(),tx)).unwrap();rx.await.unwrap().unwrap();
        drop(control);drop(session);server.await.unwrap();
    }).await.expect("SFTP integration timed out");
}
