//! Transport pilot only. Peer paths select one of two virtual entries; none are opened on Windows.
use super::{ConnectionShare, durability::bounded_command, sharing::ShellChannel};
use crate::{
    options::{Authentication, Source, SshOptions},
    session::{Command, Session},
};
use anyhow::{Context, Result, bail, ensure};
use bytes::{Buf, Bytes, BytesMut};
use russh::{
    ChannelMsg,
    keys::{HashAlg, PublicKey},
};
use russh_sftp::protocol::*;
use std::{collections::HashMap, env, fs, os::windows::fs::FileExt, sync::Arc, time::Duration};
use tokio::{
    task::JoinSet,
    time::{Instant, sleep, timeout},
};

const NAME: &str = "windows-binary.bin";
const CONTENT: &[u8] = b"w11term\x00\xff\x80\r\npassive SSHFS\x1b\xfe\x02";
const MAX_PACKET: usize = 64 * 1024;

#[derive(Clone, Copy)]
enum Node {
    Root,
    File,
}
impl Node {
    fn lookup(path: &str) -> Option<Self> {
        match path {
            "." | "/" => Some(Self::Root),
            "/windows-binary.bin" => Some(Self::File),
            _ => None,
        }
    }
    fn attrs(self) -> FileAttributes {
        let (permissions, size) = match self {
            Self::Root => (0o040555, 0),
            Self::File => (0o100444, CONTENT.len() as u64),
        };
        FileAttributes {
            permissions: Some(permissions),
            size: Some(size),
            ..FileAttributes::empty()
        }
    }
}

struct FixedExport {
    file: fs::File,
    handles: HashMap<String, (Node, bool)>,
    next: u64,
    read_bytes: usize,
}

impl FixedExport {
    fn new(file: fs::File) -> Self {
        Self {
            file,
            handles: HashMap::new(),
            next: 1,
            read_bytes: 0,
        }
    }
    fn open(&mut self, id: u32, node: Node) -> Result<Packet, StatusCode> {
        if self.handles.len() == 16 {
            return Err(StatusCode::Failure);
        }
        let handle = self.next.to_string();
        self.next += 1;
        self.handles.insert(handle.clone(), (node, false));
        Ok(Handle { id, handle }.into())
    }
    fn request(&mut self, packet: Packet) -> Result<Packet, StatusCode> {
        let id = packet.get_request_id();
        let lookup = |path: &str| Node::lookup(path).ok_or(StatusCode::NoSuchFile);
        match packet {
            Packet::Init(init) if init.version == 3 => Ok(Version::new().into()),
            Packet::RealPath(p) => {
                let node = lookup(&p.path)?;
                let name = match node {
                    Node::Root => "/".into(),
                    Node::File => format!("/{NAME}"),
                };
                Ok(Name {
                    id,
                    files: vec![File::new(name, node.attrs())],
                }
                .into())
            }
            Packet::Stat(p) => Ok(Attrs {
                id,
                attrs: lookup(&p.path)?.attrs(),
            }
            .into()),
            Packet::Lstat(p) => Ok(Attrs {
                id,
                attrs: lookup(&p.path)?.attrs(),
            }
            .into()),
            Packet::Fstat(p) => {
                let (node, _) = self.handles.get(&p.handle).ok_or(StatusCode::Failure)?;
                Ok(Attrs {
                    id,
                    attrs: node.attrs(),
                }
                .into())
            }
            Packet::Open(p) => {
                if p.pflags.bits() != OpenFlags::READ.bits() {
                    return Err(StatusCode::PermissionDenied);
                }
                if !matches!(lookup(&p.filename)?, Node::File) {
                    return Err(StatusCode::Failure);
                }
                self.open(id, Node::File)
            }
            Packet::OpenDir(p) if matches!(lookup(&p.path)?, Node::Root) => {
                self.open(id, Node::Root)
            }
            Packet::ReadDir(p) => match self.handles.get_mut(&p.handle) {
                Some((Node::Root, sent)) if !*sent => {
                    *sent = true;
                    Ok(Name {
                        id,
                        files: vec![File::new(NAME, Node::File.attrs())],
                    }
                    .into())
                }
                Some((Node::Root, _)) => Err(StatusCode::Eof),
                _ => Err(StatusCode::Failure),
            },
            Packet::Read(p) if matches!(self.handles.get(&p.handle), Some((Node::File, _))) => {
                if p.offset >= CONTENT.len() as u64 {
                    return Err(StatusCode::Eof);
                }
                let mut bytes = vec![0; (p.len as usize).min(CONTENT.len())];
                let n = self
                    .file
                    .seek_read(&mut bytes, p.offset)
                    .map_err(|_| StatusCode::Failure)?;
                bytes.truncate(n);
                self.read_bytes += n;
                Ok(Data { id, data: bytes }.into())
            }
            Packet::Close(p) if self.handles.remove(&p.handle).is_some() => {
                Ok(status(id, StatusCode::Ok))
            }
            Packet::Write(_)
            | Packet::SetStat(_)
            | Packet::FSetStat(_)
            | Packet::Remove(_)
            | Packet::Rename(_)
            | Packet::MkDir(_)
            | Packet::RmDir(_)
            | Packet::Symlink(_) => Err(StatusCode::PermissionDenied),
            _ => Err(StatusCode::OpUnsupported),
        }
    }
}

fn status(id: u32, code: StatusCode) -> Packet {
    Status {
        id,
        status_code: code,
        error_message: String::new(),
        language_tag: String::new(),
    }
    .into()
}

async fn serve(mut shell: ShellChannel, mount: &str, mut export: FixedExport) -> Result<usize> {
    let channel = shell.channel();
    channel
        .exec(true, format!("exec sshfs -f -o passive,ro :/ {mount}"))
        .await?;
    let mut frames = BytesMut::new();
    let mut stderr = Vec::new();
    let mut accepted = false;
    let mut status_code = None;
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => accepted = true,
            Some(ChannelMsg::Failure) => bail!("sshd refused the SSHFS exec channel"),
            Some(ChannelMsg::ExtendedData { data, .. }) => {
                let take = data.len().min(4096 - stderr.len());
                stderr.extend_from_slice(&data[..take]);
            }
            Some(ChannelMsg::Data { data }) => {
                ensure!(
                    accepted && frames.len() + data.len() <= 2 * MAX_PACKET,
                    "invalid SSHFS input window"
                );
                frames.extend_from_slice(&data);
                while frames.len() >= 4 {
                    let len = u32::from_be_bytes(frames[..4].try_into()?) as usize;
                    ensure!(
                        (1..=MAX_PACKET).contains(&len),
                        "invalid SFTP packet length"
                    );
                    if frames.len() < 4 + len {
                        break;
                    }
                    frames.advance(4);
                    let request = Packet::try_from(&mut frames.split_to(len).freeze())?;
                    let id = request.get_request_id();
                    let reply = export
                        .request(request)
                        .unwrap_or_else(|code| status(id, code));
                    channel.data_bytes(Bytes::try_from(reply)?).await?;
                }
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => status_code = Some(exit_status),
            Some(ChannelMsg::Close) | None => break,
            _ => {}
        }
    }
    ensure!(
        accepted && status_code == Some(0) && frames.is_empty(),
        "SSHFS stopped with {status_code:?}: {}",
        String::from_utf8_lossy(&stderr)
    );
    Ok(export.read_bytes)
}

async fn checked(link: &ConnectionShare, command: &str) -> Result<Vec<u8>> {
    let result = bounded_command(link, command, 8192).await?;
    ensure!(
        result.status == 0,
        "guest command exited {}: {command}\n{}",
        result.status,
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result.stdout)
}

async fn shell_pid(pane: &Session, label: &str) -> Result<u32> {
    let (ack, acknowledged) = tokio::sync::oneshot::channel();
    pane.input
        .send(Command::Raw(
            format!("printf '\\n{label}=%s\\n' \"$$\"\r").into_bytes(),
            ack,
        ))
        .map_err(|_| anyhow::anyhow!("interactive pane closed"))?;
    acknowledged.await?.map_err(anyhow::Error::msg)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = pane.frames.load();
        if let Some(pid) = frame.rows.iter().find_map(|row| {
            row.as_str()
                .trim()
                .strip_prefix(label)?
                .strip_prefix('=')?
                .parse()
                .ok()
        }) {
            return Ok(pid);
        }
        ensure!(
            pane.frames.changed(frame.generation, deadline).await,
            "missing shell PID marker {label}"
        );
    }
}

async fn check_mount(link: &ConnectionShare, mount: &str) -> Result<()> {
    let record = format!("awk '$5 == \"{mount}\" {{ print }}' /proc/self/mountinfo");
    loop {
        let bytes = checked(link, &record).await?;
        if !bytes.is_empty() {
            let line = std::str::from_utf8(&bytes)?;
            let (fields, fs) = line.split_once(" - ").context("malformed mountinfo")?;
            ensure!(
                fs.split_whitespace().next() == Some("fuse.sshfs"),
                "wrong mount: {line}"
            );
            ensure!(
                fields
                    .split_whitespace()
                    .nth(5)
                    .is_some_and(|opts| opts.split(',').any(|s| s == "ro")),
                "writable mount: {line}"
            );
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    ensure!(
        checked(link, &format!("ls -1A {mount}")).await? == format!("{NAME}\n").as_bytes(),
        "unexpected exported entries"
    );
    ensure!(
        checked(link, &format!("cat {mount}/{NAME}")).await? == CONTENT,
        "guest read different bytes"
    );
    checked(
        link,
        &format!("if (printf X >> {mount}/{NAME}); then exit 1; fi"),
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the disposable Linux FUSE guest and pinned loopback SSH supplied by the mount runner"]
async fn native_windows_passive_sshfs() -> Result<()> {
    let port = env::var("W11TERM_MOUNT_TEST_PORT")?.parse::<u16>()?;
    let key = PublicKey::from_openssh(&env::var("W11TERM_MOUNT_TEST_HOST_KEY")?)?;
    let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
    let options = SshOptions {
        host: "127.0.0.1".into(),
        port,
        user: "w11fixture".into(),
        authentication: Authentication::Identity {
            identity: env::var_os("W11TERM_MOUNT_TEST_IDENTITY")
                .context("missing temporary client key")?
                .into(),
        },
        host_key: Some(fingerprint.clone()),
        proxy_jump: vec![],
        terminal_type: Default::default(),
        durable: false,
        player: crate::options::PlayerOptions {
            reconnect: false,
            ..Default::default()
        },
    };
    let pane = Session::start_configured(Source::Ssh(options), Default::default(), false);
    let link = timeout(Duration::from_secs(20), async {
        loop {
            if let Some(link) = pane.ssh_connection() {
                return anyhow::Ok(link);
            }
            ensure!(
                pane.events.restart_token().is_none(),
                "{}",
                pane.frames.load().status
            );
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("loopback SSH login timed out")??;
    ensure!(
        link.fingerprint() == fingerprint,
        "unexpected SSH host identity"
    );
    let before = timeout(Duration::from_secs(6), shell_pid(&pane, "BEFORE_MOUNT")).await??;
    let root = tempfile::tempdir()?;
    let path = root.path().join(NAME);
    fs::write(&path, CONTENT)?;
    let export = FixedExport::new(fs::File::open(&path)?);
    let mount = format!("/tmp/w11term-mount-{:032x}", rand::random::<u128>());
    let shell = timeout(Duration::from_secs(6), link.open_shell()).await??;
    checked(&link, &format!("umask 077 && mkdir {mount}")).await?;
    let mut servers = JoinSet::new();
    let mount_arg = mount.clone();
    servers.spawn(async move { serve(shell, &mount_arg, export).await });
    let verified = timeout(Duration::from_secs(24), async {
        tokio::select! {
            result = check_mount(&link, &mount) => result,
            ended = servers.join_next() => bail!("SSHFS quit before mount verification: {ended:?}"),
        }
    })
    .await
    .context("SSHFS mount/read timed out")
    .and_then(|result| result);
    // Clean up only our freshly created, private directory, even when checks fail.
    let mounted =
        format!("awk '$5 == \"{mount}\" {{present=1}} END {{exit !present}}' /proc/self/mountinfo");
    let unmount = format!(
        "set -e
         if {mounted}; then fusermount3 -u {mount}; fi
         if {mounted}; then echo 'mount still present' >&2; exit 1; fi
         rmdir {mount}"
    );
    let cleanup = checked(&link, &unmount).await;
    verified?;
    cleanup?;
    let read = timeout(Duration::from_secs(6), servers.join_next())
        .await?
        .context("SSHFS task missing")???;
    ensure!(
        read >= CONTENT.len() && fs::read(path)? == CONTENT,
        "Windows fixture changed or was not read"
    );
    let after = timeout(Duration::from_secs(6), shell_pid(&pane, "AFTER_MOUNT")).await??;
    let still_live = pane
        .ssh_connection()
        .context("interactive SSH connection lost")?;
    ensure!(
        after == before && Arc::ptr_eq(&still_live.handle(), &link.handle()),
        "shell or transport replaced"
    );
    Ok(())
}

#[test]
fn fixed_mount_export_rejects_writes_and_never_maps_peer_paths_to_windows() {
    let mut export = FixedExport::new(tempfile::tempfile().unwrap());
    for path in [
        "/../other",
        r"C:\Windows",
        "/windows-binary.bin:stream",
        "/new-file",
    ] {
        assert!(Node::lookup(path).is_none());
    }
    for flags in [
        OpenFlags::WRITE,
        OpenFlags::READ | OpenFlags::TRUNCATE,
        OpenFlags::CREATE,
    ] {
        let request = Open {
            id: 7,
            filename: format!("/{NAME}"),
            pflags: flags,
            attrs: FileAttributes::empty(),
        };
        assert!(matches!(
            export.request(request.into()),
            Err(StatusCode::PermissionDenied)
        ));
    }
    assert!(export.handles.is_empty());
}

#[test]
fn fixed_mount_export_reads_exact_offsets_then_expires_the_handle() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join(NAME);
    fs::write(&path, CONTENT)?;
    let mut export = FixedExport::new(fs::File::open(path)?);
    let open = Open {
        id: 1,
        filename: format!("/{NAME}"),
        pflags: OpenFlags::READ,
        attrs: FileAttributes::empty(),
    };
    let Ok(Packet::Handle(Handle { handle, .. })) = export.request(open.into()) else {
        bail!("could not open the virtual file");
    };
    let read = |offset| Read {
        id: 2,
        handle: handle.clone(),
        offset,
        len: u32::MAX,
    };
    let Ok(Packet::Data(data)) = export.request(read(7).into()) else {
        bail!("could not read binary fixture");
    };
    ensure!(data.data == CONTENT[7..]);
    assert!(matches!(
        export.request(read(u64::MAX).into()),
        Err(StatusCode::Eof)
    ));
    assert!(matches!(
        export.request(
            Close {
                id: 3,
                handle: handle.clone()
            }
            .into()
        ),
        Ok(Packet::Status(Status {
            status_code: StatusCode::Ok,
            ..
        }))
    ));
    assert!(export.request(read(0).into()).is_err());
    Ok(())
}
