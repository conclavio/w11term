//! A real SSH peer checks the distinction between the game PTY and the private
//! resume subsystem. Tickets below are deterministic test data only.
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use russh::{
    Channel, ChannelId,
    keys::{Algorithm, HashAlg, PrivateKey},
    server,
};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
    time::timeout,
};
use w11term::{
    options::{Authentication, PlayerOptions, Source, SshOptions},
    session::{Command, Session},
};

struct Request {
    value: Value,
    peer: SocketAddr,
    channel: ChannelId,
    shell: ChannelId,
    handle: server::Handle,
    marker: String,
    size: (u32, u32),
}
impl Request {
    async fn answer(&self, msg: Value) -> Result<()> {
        let data = serde_json::to_vec(&msg)?;
        self.handle
            .data(self.channel, (data.len() as u32).to_be_bytes().to_vec())
            .await
            .map_err(|_| anyhow::anyhow!("sideband closed"))?;
        self.handle
            .data(self.channel, data)
            .await
            .map_err(|_| anyhow::anyhow!("sideband closed"))
    }
}
struct Peer {
    addr: SocketAddr,
    attempts: Arc<AtomicUsize>,
    req: mpsc::UnboundedSender<Request>,
    closed: mpsc::UnboundedSender<String>,
    shell: Option<ChannelId>,
    side: Option<ChannelId>,
    marker: String,
    frame: Vec<u8>,
    size: (u32, u32),
    refuse: bool,
}
impl server::Handler for Peer {
    type Error = anyhow::Error;
    async fn channel_close(&mut self, c: ChannelId, _: &mut server::Session) -> Result<()> {
        if self.side == Some(c) {
            self.side = None;
        }
        Ok(())
    }
    async fn channel_eof(&mut self, c: ChannelId, _: &mut server::Session) -> Result<()> {
        if self.shell == Some(c) {
            let _ = self.closed.send(self.marker.clone());
        }
        Ok(())
    }
    async fn auth_none(&mut self, user: &str) -> Result<server::Auth> {
        Ok(if user == "play" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn channel_open_session(
        &mut self,
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<()> {
        // As on a RustMUSH doorway, the primary PTY and a single auxiliary
        // channel can coexist. A new subsystem must follow the old CLOSE.
        if self.side.is_none() {
            reply.accept().await;
        }
        Ok(())
    }
    async fn pty_request(
        &mut self,
        c: ChannelId,
        _: &str,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        s: &mut server::Session,
    ) -> Result<()> {
        self.size = (cols, rows);
        self.shell = Some(c);
        s.channel_success(c)?;
        Ok(())
    }
    async fn env_request(
        &mut self,
        _: ChannelId,
        name: &str,
        value: &str,
        _: &mut server::Session,
    ) -> Result<()> {
        if name == "RUSTMUSH_RESUME_V1" {
            self.marker = value.to_string();
        }
        Ok(())
    }
    async fn shell_request(&mut self, c: ChannelId, s: &mut server::Session) -> Result<()> {
        s.channel_success(c)?;
        s.data(c, b"Welcome to the fixture\r\n".as_slice())?;
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        c: ChannelId,
        name: &str,
        s: &mut server::Session,
    ) -> Result<()> {
        if name == "rustmush-resume-v1" {
            self.attempts.fetch_add(1, Ordering::Relaxed);
        }
        if self.refuse || name != "rustmush-resume-v1" {
            s.channel_failure(c)?;
        } else {
            self.side = Some(c);
            s.channel_success(c)?;
        }
        Ok(())
    }
    async fn data(&mut self, c: ChannelId, data: &[u8], s: &mut server::Session) -> Result<()> {
        if Some(c) == self.shell {
            if data == b"drop-link" {
                anyhow::bail!("test dropped SSH connection");
            }
            s.data(c, data.to_vec())?;
        } else if Some(c) == self.side {
            self.frame.extend_from_slice(data);
            if self.frame.len() >= 4 {
                let len = u32::from_be_bytes(self.frame[..4].try_into()?) as usize;
                ensure!(len <= 1024);
                if self.frame.len() >= 4 + len {
                    let value = serde_json::from_slice(&self.frame[4..])?;
                    self.req.send(Request {
                        value,
                        peer: self.addr,
                        channel: c,
                        shell: self.shell.context("missing shell")?,
                        handle: s.handle(),
                        marker: self.marker.clone(),
                        size: self.size,
                    })?;
                    self.frame.clear();
                }
            }
        }
        Ok(())
    }
}
struct Fixture {
    source: Source,
    attempts: Arc<AtomicUsize>,
    requests: mpsc::UnboundedReceiver<Request>,
    closed: mpsc::UnboundedReceiver<String>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn start(refuse: bool) -> Result<Self> {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let (req, requests) = mpsc::unbounded_channel();
        let (closed_tx, closed) = mpsc::unbounded_channel();
        let attempts = Arc::new(AtomicUsize::new(0));
        let peer_attempts = attempts.clone();
        let server = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            let config = Arc::new(server::Config {
                keys: vec![key],
                auth_rejection_time: Duration::ZERO,
                ..Default::default()
            });
            while let Ok((stream, addr)) = listener.accept().await {
                let peer = Peer {
                    addr,
                    attempts: peer_attempts.clone(),
                    req: req.clone(),
                    closed: closed_tx.clone(),
                    shell: None,
                    side: None,
                    marker: String::new(),
                    frame: Vec::new(),
                    size: (0, 0),
                    refuse,
                };
                let config = config.clone();
                clients.spawn(async move {
                    if let Ok(s) = server::run_stream(config, stream, peer).await {
                        let _ = s.await;
                    }
                });
            }
        });
        Ok(Self {
            source: Source::Ssh(SshOptions {
                scrollback_lines: Default::default(),
                terminal_type: Default::default(),
                host: "127.0.0.1".into(),
                user: "play".into(),
                port,
                authentication: Authentication::None,
                host_key: Some(pin),
                proxy_jump: vec![],
                local_forwards: vec![],
                durable: false,
                player: PlayerOptions {
                    rustmush: true,
                    ..Default::default()
                },
            }),
            attempts,
            requests,
            closed,
            server,
        })
    }
    async fn request(&mut self, pane: &Session) -> Result<Request> {
        timeout(Duration::from_secs(8), self.requests.recv())
            .await
            .with_context(|| format!("waiting for sideband: {}", pane.frames.load().status))?
            .context("missing game sideband request")
    }
    async fn drop_and_redeem(&mut self, pane: &Session, ticket: &str) -> Result<Request> {
        let issue = self.request(pane).await?;
        assert_eq!(issue.value["op"], "issue");
        issue
            .answer(json!({"v":1,"status":"issued","ticket":ticket,"grace_seconds":120}))
            .await?;
        event(pane, "issued").await?;
        raw(pane, b"drop-link").await?;
        let redeem = self.request(pane).await?;
        assert_eq!(
            redeem.value,
            json!({"v":1,"op":"redeem","pane":issue.value["pane"],"ticket":ticket})
        );
        Ok(redeem)
    }
}
async fn event(session: &Session, status: &str) -> Result<()> {
    event_after(session, status, 0).await
}
async fn event_after(session: &Session, status: &str, mut cursor: u64) -> Result<()> {
    timeout(Duration::from_secs(7), async {
        loop {
            let batch = session.events.wait(cursor, Duration::from_secs(2)).await;
            if batch
                .events
                .iter()
                .any(|e| e.kind == "game-resume" && e.data["status"] == status)
            {
                break;
            }
            cursor = batch.next;
        }
    })
    .await
    .with_context(|| format!("missing {status} status: {}", session.frames.load().status))?;
    Ok(())
}
async fn raw(session: &Session, data: &[u8]) -> Result<()> {
    let (tx, rx) = oneshot::channel();
    session.input.send(Command::Raw(data.to_vec(), tx))?;
    timeout(Duration::from_secs(3), rx)
        .await
        .context("raw input acknowledgement timed out")??
        .map_err(anyhow::Error::msg)
}
async fn record(session: &Session, path: Option<PathBuf>) -> Result<()> {
    let (tx, rx) = oneshot::channel();
    session.input.send(Command::Record { path, reply: tx })?;
    rx.await?.map_err(anyhow::Error::msg)?;
    Ok(())
}
async fn visible(session: &Session, needle: &str) -> Result<()> {
    timeout(Duration::from_secs(4), async {
        loop {
            if session
                .frames
                .load()
                .rows
                .iter()
                .any(|l| l.as_str().contains(needle))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    })
    .await
    .context("terminal output did not arrive")?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn redeemed_ticket_stays_on_its_pane_and_out_of_vt_and_input() -> Result<()> {
    let mut f = Fixture::start(false).await?;
    let pane = Session::start(f.source.clone());
    let issue = f.request(&pane).await?;
    let nonce = issue.value["pane"].as_str().unwrap();
    assert_eq!(issue.value, json!({"v":1,"op":"issue","pane":nonce}));
    assert_eq!(issue.marker, format!("issue:{nonce}"));
    let dir = tempfile::tempdir()?;
    let vt = dir.path().join("capture.vt");
    record(&pane, Some(vt.clone())).await?;
    let first = URL_SAFE_NO_PAD.encode([1; 32]);
    let second = URL_SAFE_NO_PAD.encode([2; 32]);
    issue
        .answer(json!({"v":1,"status":"issued","ticket":first,"grace_seconds":120}))
        .await?;
    event(&pane, "issued").await?;
    // Opening another interactive pane never offers this one's credential.
    let adjacent = Session::start(f.source.clone());
    let other = f.request(&adjacent).await?;
    assert_eq!(other.value["op"], "issue");
    assert_ne!(other.value["pane"], nonce);
    assert!(other.value.get("ticket").is_none());
    drop(adjacent);
    assert_eq!(
        timeout(Duration::from_secs(3), f.closed.recv())
            .await
            .context("peer did not observe primary shell EOF")?
            .as_deref(),
        Some(other.marker.as_str())
    );
    raw(&pane, b"drop-link").await?;
    pane.input
        .send(Command::Resize(wezterm_term::TerminalSize {
            cols: 144,
            rows: 41,
            ..Default::default()
        }))?;
    let resume = f.request(&pane).await?;
    assert_eq!(resume.marker, format!("redeem:{nonce}"));
    assert_eq!(
        resume.value,
        json!({"v":1,"op":"redeem","pane":nonce,"ticket":first})
    );
    assert_eq!(resume.size, (144, 41));
    assert!(
        raw(&pane, b"old-turn")
            .await
            .unwrap_err()
            .to_string()
            .contains("not queued")
    );
    resume
        .answer(json!({"v":1,"status":"auth_required"}))
        .await?;
    event(&pane, "auth_required").await?;
    raw(&pane, b"fixture-login").await?;
    resume
        .handle
        .data(resume.shell, b"\x1b[2J\x1b[Hfresh cockpit".to_vec())
        .await
        .map_err(|_| anyhow::anyhow!("shell closed"))?;
    resume
        .answer(json!({"v":1,"status":"redeemed","ticket":second,"grace_seconds":120}))
        .await?;
    event(&pane, "redeemed").await?;
    raw(&pane, b"first-new-key").await?;
    visible(&pane, "first-new-key").await?;
    record(&pane, None).await?;
    let recording = String::from_utf8_lossy(&std::fs::read(vt)?).into_owned();
    assert!(recording.contains("first-new-key"));
    assert!(recording.contains("fresh cockpit"));
    let journal = serde_json::to_string(&pane.events.since(0))?;
    for secret in [first.as_str(), second.as_str(), "old-turn"] {
        assert!(!recording.contains(secret));
        assert!(!journal.contains(secret));
        assert!(
            !pane
                .frames
                .load()
                .rows
                .iter()
                .any(|l| l.as_str().contains(secret))
        );
    }
    raw(&pane, b"drop-link").await?;
    let next = f.request(&pane).await?;
    assert_eq!(next.value["ticket"], second);
    next.answer(json!({"v":1,"status":"expired"})).await?;
    event(&pane, "expired").await?;
    raw(&pane, b"welcome-input").await?;
    visible(&pane, "welcome-input").await?;
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_subsystem_and_silent_redeem_leave_an_interactive_terminal() -> Result<()> {
    let unsupported = Fixture::start(true).await?;
    let pane = Session::start(unsupported.source.clone());
    event(&pane, "unsupported").await?;
    raw(&pane, b"ordinary-login").await?;
    visible(&pane, "ordinary-login").await?;
    assert_eq!(unsupported.attempts.load(Ordering::Relaxed), 1);
    let mut f = Fixture::start(false).await?;
    let pane = Session::start(f.source.clone());
    let issue = f.request(&pane).await?;
    issue.answer(json!({"v":1,"status":"issued","ticket":URL_SAFE_NO_PAD.encode([3;32]),"grace_seconds":10})).await?;
    event(&pane, "issued").await?;
    raw(&pane, b"drop-link").await?;
    let _silent = f.request(&pane).await?;
    event(&pane, "unavailable").await?;
    raw(&pane, b"shell-restored").await?;
    visible(&pane, "shell-restored").await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_redeem_reissues_on_the_same_shell_and_uses_the_new_ticket() -> Result<()> {
    let mut f = Fixture::start(false).await?;
    let pane = Session::start(f.source.clone());
    let old = URL_SAFE_NO_PAD.encode([4; 32]);
    let fresh = URL_SAFE_NO_PAD.encode([5; 32]);
    let redeem = f.drop_and_redeem(&pane, &old).await?;
    assert!(raw(&pane, b"stale-turn").await.is_err());
    let cursor = pane.events.since(0).next;
    redeem.answer(json!({"v":1,"status":"unavailable"})).await?;
    event_after(&pane, "unavailable", cursor).await?;

    let issue = f.request(&pane).await?;
    assert_eq!((issue.peer, issue.shell), (redeem.peer, redeem.shell));
    assert_ne!(issue.channel, redeem.channel);
    let nonce = &redeem.value["pane"];
    assert_eq!(issue.value, json!({"v":1,"op":"issue","pane":nonce}));
    assert_eq!(issue.marker, redeem.marker);
    issue
        .answer(json!({"v":1,"status":"auth_required"}))
        .await?;
    event_after(&pane, "auth_required", cursor).await?;
    raw(&pane, b"choose-pilot").await?;
    visible(&pane, "choose-pilot").await?;
    issue
        .answer(json!({"v":1,"status":"issued","ticket":fresh,"grace_seconds":120}))
        .await?;
    event_after(&pane, "issued", cursor).await?;

    raw(&pane, b"drop-link").await?;
    let next = f.request(&pane).await?;
    assert_ne!(next.peer, issue.peer);
    assert_eq!(
        next.value,
        json!({"v":1,"op":"redeem","pane":nonce,"ticket":fresh})
    );
    next.answer(json!({"v":1,"status":"denied"})).await?;
    let followup = f.request(&pane).await?;
    assert_eq!(followup.value, json!({"v":1,"op":"issue","pane":nonce}));
    let cursor = pane.events.since(0).next;
    followup.answer(json!({"v":1,"status":"busy"})).await?;
    event_after(&pane, "busy", cursor).await?;
    raw(&pane, b"still-playing").await?;
    visible(&pane, "still-playing").await?;
    raw(&pane, b"drop-link").await?;
    let next = f.request(&pane).await?;
    assert_ne!(next.peer, followup.peer);
    assert_eq!(next.value, json!({"v":1,"op":"issue","pane":nonce}));
    assert_eq!(f.attempts.load(Ordering::Relaxed), 6);
    let journal = serde_json::to_string(&pane.events.since(0))?;
    for secret in [old.as_str(), fresh.as_str(), "stale-turn"] {
        assert!(!journal.contains(secret));
        assert!(
            !pane
                .frames
                .load()
                .rows
                .iter()
                .any(|line| line.as_str().contains(secret))
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncated_rotation_reply_preserves_the_ticket_for_the_next_connection() -> Result<()> {
    let mut f = Fixture::start(false).await?;
    let pane = Session::start(f.source.clone());
    let old = URL_SAFE_NO_PAD.encode([6; 32]);
    let rotated = URL_SAFE_NO_PAD.encode([7; 32]);
    let redeem = f.drop_and_redeem(&pane, &old).await?;
    // Lose the end of a real rotation reply, then observe EOF on the subsystem
    // while the primary channel can still deliver ordinary shell traffic.
    let grant = serde_json::to_vec(
        &json!({"v":1,"status":"redeemed","ticket":rotated,"grace_seconds":120}),
    )?;
    let packet = [
        (grant.len() as u32).to_be_bytes().as_slice(),
        &grant[..grant.len() - 1],
    ]
    .concat();
    redeem
        .handle
        .data(redeem.channel, packet)
        .await
        .map_err(|_| anyhow::anyhow!("sideband closed"))?;
    redeem
        .handle
        .eof(redeem.channel)
        .await
        .map_err(|_| anyhow::anyhow!("could not send sideband EOF"))?;
    redeem
        .handle
        .close(redeem.channel)
        .await
        .map_err(|_| anyhow::anyhow!("could not close sideband"))?;
    event(&pane, "unavailable").await?;
    raw(&pane, b"shell-is-alive").await?;
    visible(&pane, "shell-is-alive").await?;
    assert_eq!(f.attempts.load(Ordering::Relaxed), 2);
    raw(&pane, b"drop-link").await?;
    let retry = f.request(&pane).await?;
    assert_ne!(retry.peer, redeem.peer);
    assert_eq!(retry.value, redeem.value);
    retry
        .answer(json!({"v":1,"status":"redeemed","ticket":rotated,"grace_seconds":120}))
        .await?;
    event(&pane, "redeemed").await?;
    Ok(())
}
