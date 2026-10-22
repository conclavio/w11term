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
    async fn shell_request(&mut self, c: ChannelId, s: &mut server::Session) -> anyhow::Result<()> {
        s.channel_success(c)?;
        s.data(c, b"ready\r\n".as_slice())?;
        Ok(())
    }
    async fn data(
        &mut self,
        c: ChannelId,
        data: &[u8],
        s: &mut server::Session,
    ) -> anyhow::Result<()> {
        if self.channels.contains_key(&c) {
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
        // Keep the SFTP channel busy long enough to verify independent terminal input.
        tokio::time::sleep(Duration::from_millis(20)).await;
        if self.root.join("pause-reads").exists() {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let file = self.file(&h)?;
        file.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut data = vec![0; len as usize];
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
            files: vec![russh_sftp::protocol::File::dummy(path)],
        })
    }
}

async fn fixture(
    reject: bool,
) -> (
    Session,
    tempfile::TempDir,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
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
    let peer = Peer {
        channels: HashMap::new(),
        root: root.path().into(),
        opens: opens.clone(),
        reject,
    };
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = server::run_stream(config, stream, peer)
            .await
            .unwrap()
            .await;
    });
    let session = Session::start(Source::Ssh(SshOptions {
        host: "127.0.0.1".into(),
        user: "test".into(),
        port,
        authentication: Authentication::None,
        host_key: Some(pin),
    }));
    (session, root, opens, server)
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
        label: "test".into(),
        active: false,
        input: session.input.clone(),
    }]);
    r
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfers_handles_and_rejected_subsystem_without_reconnect() {
    tokio::time::timeout(Duration::from_secs(30),async {
        let (session,root,opens,server)=fixture(false).await;
        let panes=registry(&session);
        let control=control::Server::start(panes,|_,_,_|anyhow::bail!("unused")).unwrap();
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
        fs::write(&dest,b"preserved").unwrap();assert!(!request(json!({"op":"get","remote":"/folder/日本 file.bin","local":dest})).await.ok);assert_eq!(fs::read(&dest).unwrap(),b"preserved");
        ok(json!({"op":"get","remote":"/folder/日本 file.bin","local":dest,"overwrite":true})).await;assert_eq!(fs::read(&dest).unwrap(),bytes);
        let h=ok(json!({"op":"opendir","path":"/folder"})).await["handle"].as_u64().unwrap();
        assert_eq!(ok(json!({"op":"readdir","handle":h})).await["entries"][0]["filename"],"日本 file.bin");
        assert_eq!(ok(json!({"op":"readdir","handle":h})).await["eof"],true);ok(json!({"op":"close","handle":h})).await;
        assert!(!request(json!({"op":"readdir","handle":h})).await.ok);
        let h=ok(json!({"op":"open","path":"/folder/日本 file.bin","flags":["read","write"]})).await["handle"].as_u64().unwrap();
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
        let (session,_root,_opens,server)=fixture(true).await;
        let control=control::Server::start(registry(&session),|_,_,_|anyhow::bail!("unused")).unwrap();
        let r=request(json!({"op":"info"})).await;assert!(!r.ok);assert!(r.error.unwrap().contains("rejected"));
        let (tx,rx)=tokio::sync::oneshot::channel();session.input.send(Command::Raw(b"still here".to_vec(),tx)).unwrap();rx.await.unwrap().unwrap();
        drop(control);drop(session);server.await.unwrap();
    }).await.expect("SFTP integration timed out");
}
