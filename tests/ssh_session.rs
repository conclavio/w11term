//! A real SSH handshake and remote-PTY protocol on loopback. No user server/credentials involved.
use russh::{
    Channel, ChannelId, Pty,
    keys::{Algorithm, HashAlg, PrivateKey, PublicKey},
    server,
};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::mpsc};
use w11term::{
    control::{self, Pane, Registry, Request},
    options::{Authentication, Source, SshOptions},
    session::{Command, Session},
};
use wezterm_term::{KeyCode, KeyModifiers, TerminalSize};

#[derive(Debug, PartialEq)]
enum Observed {
    Pty(String, u32, u32),
    Resize(u32, u32),
    Data(Vec<u8>),
}

struct Peer {
    identity: PublicKey,
    allow_none: bool,
    events: mpsc::UnboundedSender<Observed>,
}

impl server::Handler for Peer {
    type Error = russh::Error;
    async fn auth_none(&mut self, user: &str) -> Result<server::Auth, Self::Error> {
        Ok(if self.allow_none && user == "terminal-test" {
            server::Auth::Accept
        } else {
            server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        })
    }
    async fn auth_publickey(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> Result<server::Auth, Self::Error> {
        Ok(if user == "terminal-test" && key == &self.identity {
            server::Auth::Accept
        } else {
            server::Auth::Reject {
                proceed_with_methods: None,
                partial_success: false,
            }
        })
    }
    async fn channel_open_session(
        &mut self,
        _channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _width: u32,
        _height: u32,
        _modes: &[(Pty, u32)],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.events
            .send(Observed::Pty(term.into(), cols, rows))
            .unwrap();
        session.channel_success(channel)?;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(
            channel,
            b"\x1b[?2004h\x1b[2J\x1b[Hremote ready\r\n\x1b[6n".as_slice(),
        )?;
        Ok(())
    }
    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        cols: u32,
        rows: u32,
        _width: u32,
        _height: u32,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.events.send(Observed::Resize(cols, rows)).unwrap();
        Ok(())
    }
    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.events.send(Observed::Data(data.to_vec())).unwrap();
        if data == b"x" {
            session.data(channel, b"x".as_slice())?;
        } else if let Some(bytes) = data.strip_prefix(b"\0sync:") {
            session.data(channel, bytes.to_vec())?;
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_pty_roundtrip_queries_input_and_resize() {
    roundtrip(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keyless_ssh_roundtrip_without_an_agent() {
    roundtrip(true).await;
}

async fn roundtrip(allow_none: bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let identity = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let key_file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            key_file.path(),
            identity
                .to_openssh(russh::keys::ssh_key::LineEnding::LF)
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        let pin = host_key
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();
        let config = Arc::new(server::Config {
            keys: vec![host_key],
            auth_rejection_time: Duration::ZERO,
            ..Default::default()
        });
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (events, mut observed) = mpsc::unbounded_channel();
        let peer = Peer {
            identity: identity.public_key().clone(),
            allow_none,
            events,
        };
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let running = server::run_stream(config, stream, peer).await.unwrap();
            let _ = running.await;
        });
        let initial_size = if allow_none {
            TerminalSize {
                cols: 170,
                rows: 48,
                ..TerminalSize::default()
            }
        } else {
            TerminalSize::default()
        };
        let session = Session::start_sized(
            Source::Ssh(SshOptions {
                host: "127.0.0.1".into(),
                user: "terminal-test".into(),
                port,
                authentication: if allow_none {
                    Authentication::None
                } else {
                    Authentication::Identity {
                        identity: key_file.path().into(),
                    }
                },
                host_key: Some(pin),
                player: Default::default(),
            }),
            initial_size,
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Pty(
                "xterm-256color".into(),
                initial_size.cols as u32,
                initial_size.rows as u32
            )
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"\x1b[2;1R".to_vec())
        );
        session
            .input
            .send(Command::Resize(TerminalSize {
                cols: 100,
                rows: 30,
                pixel_width: 1000,
                pixel_height: 600,
                dpi: 96,
            }))
            .unwrap();
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Resize(100, 30)
        );
        session
            .input
            .send(Command::Key {
                key: KeyCode::Char('x'),
                modifiers: KeyModifiers::NONE,
                down: true,
                repeat: false,
            })
            .unwrap();
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"x".to_vec())
        );
        let started = std::time::Instant::now();
        loop {
            let frame = session.frames.load();
            let text = frame
                .rows
                .iter()
                .map(|l| l.as_str().trim_end().to_owned())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "echo not published: gen={} bytes={} cursor={:?}, text={text:?}, {}",
                frame.generation,
                frame.bytes_received,
                frame.cursor,
                frame.status
            );
            if text.contains("remote ready\nx") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if allow_none {
            use base64::Engine;
            session
                .input
                .send(Command::Paste("one\ntwo".into()))
                .unwrap();
            let Observed::Data(pasted) = next(&mut observed, &session).await else {
                panic!("missing bracketed paste");
            };
            assert!(pasted.starts_with(b"\x1b[200~"));
            assert!(pasted.ends_with(b"\x1b[201~"));
            let registry = Registry::default();
            registry.replace([Pane {
                id: 7,
                label: "loopback".into(),
                active: false,
                input: session.input.clone(),
            }]);
            let discovery = tempfile::tempdir().unwrap();
            let server = control::Server::start_in_directory(
                registry.clone(),
                |_, _, _| anyhow::bail!("capture unavailable in transport test"),
                discovery.path().into(),
            )
            .unwrap();
            let pid = Some(std::process::id());
            let reply = control::call(pid, &Request::List).await.unwrap();
            assert!(reply.ok, "{reply:?}");
            let sessions = &reply.result.unwrap()["sessions"];
            assert_eq!(sessions[0]["id"], 7);
            assert_eq!(sessions[0]["cols"], 100);
            assert_eq!(sessions[0]["active"], false);
            let bytes = b"BOARD\r\x07\x1b[C\0\xff";
            let reply = control::call(
                pid,
                &Request::SendText {
                    pane: 7,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                    no_paste: true,
                },
            )
            .await
            .unwrap();
            assert!(reply.ok, "{reply:?}");
            assert_eq!(reply.result.unwrap()["bytes_written"], bytes.len());
            assert_eq!(
                next(&mut observed, &session).await,
                Observed::Data(bytes.to_vec())
            );
            let reply = control::call(pid, &Request::GetText { pane: 7 })
                .await
                .unwrap();
            assert!(reply.ok, "{reply:?}");
            assert!(
                reply.result.unwrap()["text"]
                    .as_str()
                    .unwrap()
                    .contains("remote ready\nx")
            );
            let reply = control::call(pid, &Request::GetText { pane: 999 })
                .await
                .unwrap();
            assert!(!reply.ok);
            assert!(reply.error.unwrap().contains("unknown pane"));
            let reply = control::call(
                pid,
                &Request::SendText {
                    pane: 7,
                    data_base64: "eA==".into(),
                    no_paste: false,
                },
            )
            .await
            .unwrap();
            assert!(!reply.ok);
            // Discovery is removed, the socket disappears, and no fallback server is started.
            drop(server);
            assert!(control::call(pid, &Request::List).await.is_err());
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        session.input.send(Command::Observe(tx)).unwrap();
        let mut output = rx.await.unwrap();
        async fn echoed(
            session: &Session,
            output: &mut tokio::sync::broadcast::Receiver<Vec<u8>>,
            bytes: &[u8],
        ) -> w11term::terminal::Snapshot {
            let mut request = b"\0sync:".to_vec();
            request.extend(bytes);
            let (tx, rx) = tokio::sync::oneshot::channel();
            session.input.send(Command::Raw(request, tx)).unwrap();
            rx.await.unwrap().unwrap();
            let received = tokio::time::timeout(Duration::from_secs(2), output.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(received, bytes);
            let (tx, rx) = tokio::sync::oneshot::channel();
            session.input.send(Command::Capture(tx)).unwrap();
            rx.await.unwrap().0
        }
        let old = echoed(&session, &mut output, b"\x1b[2J\x1b[HOLD").await;
        let held = echoed(&session, &mut output, b"\x1b[?2026h\rNEW").await;
        assert_eq!(held.generation, old.generation);
        assert!(held.rows[0].as_str().starts_with("OLD"));
        let done = echoed(&session, &mut output, b"\x1b[?2026l").await;
        assert_eq!(done.generation, old.generation + 1);
        assert!(done.rows[0].as_str().starts_with("NEW"));
        drop(session);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("server session did not close")
            .unwrap();
    })
    .await
    .expect("loopback SSH session timed out");
}

async fn next(observed: &mut mpsc::UnboundedReceiver<Observed>, session: &Session) -> Observed {
    tokio::time::timeout(Duration::from_secs(3), observed.recv())
        .await
        .unwrap_or_else(|_| panic!("SSH event timed out: {}", session.frames.load().status))
        .expect("server event channel closed")
}
