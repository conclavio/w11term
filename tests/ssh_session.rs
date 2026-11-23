//! A real SSH handshake and remote-PTY protocol on loopback. No user server/credentials involved.
use russh::{
    Channel, ChannelId, Pty,
    keys::{Algorithm, HashAlg, PrivateKey, PublicKey},
    server,
};
use std::{sync::Arc, time::Duration};
use tokio::{net::TcpListener, sync::mpsc};
use w11term::{
    clipboard::RemoteClipboardPolicy,
    control::{self, Pane, Registry, Request},
    options::{Authentication, Source, SshOptions},
    session::{Command, Session},
};
use wezterm_term::{KeyCode, KeyModifiers, TerminalSize};

#[derive(Debug, PartialEq)]
enum Observed {
    Pty(String, u32, u32),
    Env(String, String),
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
    async fn env_request(
        &mut self,
        _: ChannelId,
        name: &str,
        value: &str,
        _: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.events
            .send(Observed::Env(name.into(), value.into()))
            .unwrap();
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
            b"\x1b[?2004h\x1b[2J\x1b[Hremote ready\r\n\x1b[c\x1b[6n".as_slice(),
        )?;
        Ok(())
    }
    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        cols: u32,
        rows: u32,
        _width: u32,
        _height: u32,
        session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        self.events.send(Observed::Resize(cols, rows)).unwrap();
        if (cols, rows) == (113, 37) {
            session.data(
                channel,
                b"\x1b[?1003l\x1b[?1006l\x1b[?1049l\x1b[>0q".as_slice(),
            )?;
        }
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
        } else if data == b"\x1b[2;1R" {
            // An editor selects a blinking underline, disables blinking, then
            // asks for the exact style it can save and later restore.
            session.data(
                channel,
                b"\x1b[3 q\x1b[?12l\x1bP$q q\x1b\\\x1b[0 q".as_slice(),
            )?;
        } else if data == b"\x1bP1$r4 q\x1b\\" {
            // Stock Vim probes for undercurl and resets the pen in the same write.
            session.data(channel, b"\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\\x1b[0m".as_slice())?;
        } else if data == b"\x1bP1$r0;4:3m\x1b\\" {
            session.data(
                channel,
                b"\x1b[1;34mXY\x1b[0m\r\x1b[2;1;2;2#|\x1b[2K".as_slice(),
            )?;
        } else if data == b"\x1b[0;1;34m" {
            session.data(
                channel,
                concat!(
                    "\x1b7\x1b[3;1H\x1b[1\"qprotected header\x1b[0\"q transient\x1b[?2K",
                    "\x1b[4;1HLaXXXXfR\x1b[35;4;3;4;6$x\x1b[4;4;4;4$z",
                    "\x1b[4;1;4;8;1;5;2;1$v",
                    "\x1b[2*x\x1b[5;2;5;4;1;4$r\x1b[5;2;5;4#|\x1bP$q*x\x1b\\",
                    "\x1b[6;1HABCDEF\x1b[7;1H123456\x1b[6;7r\x1b[6;3H\x1b[2'}\x1b['~",
                    "\x1b[6;1H\x1b6\x1b9\x1b[2 A\x1b[ @\x1b[6n\x1b[r",
                    "\x1b[1\"q\x1bP$q\"q\x1b\\\x1b8",
                    "\x1bP2$t4/12\x1b\\\x1b[2$w",
                    "\x1b7\x1b[6G\x1b[5;0W\x1b[2$w\x1b[?5W\x1b[2$w\x1b8",
                )
                .as_bytes(),
            )?;
        } else if let Some(bytes) = data.strip_prefix(b"\0sync:") {
            session.data(channel, bytes.to_vec())?;
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ssh_pty_roundtrip_queries_input_and_resize() {
    roundtrip(false, None, RemoteClipboardPolicy::AnyPane).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keyless_ssh_roundtrip_without_an_agent() {
    roundtrip(true, Some("xterm-sixel"), RemoteClipboardPolicy::Disabled).await;
}

async fn roundtrip(
    allow_none: bool,
    terminal_type: Option<&str>,
    clipboard_policy: RemoteClipboardPolicy,
) {
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
        let session = Session::start_configured(
            Source::Ssh(SshOptions {
                scrollback_lines: Default::default(),
                color_scheme: Default::default(),
                tab_color: Default::default(),
                terminal_type: terminal_type
                    .map(|name| name.to_owned().try_into().unwrap())
                    .unwrap_or_default(),
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
                host_key_alias: None,
                proxy_jump: Vec::new(),
                local_forwards: Vec::new(),
                durable: false,
                player: Default::default(),
            }),
            initial_size,
            clipboard_policy,
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Pty(
                terminal_type.unwrap_or("xterm-256color").into(),
                initial_size.cols as u32,
                initial_size.rows as u32
            )
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Env("COLORTERM".into(), "truecolor".into())
        );
        let expected_da1 = if clipboard_policy == RemoteClipboardPolicy::Disabled {
            b"\x1b[?65;4;6;18;22c".as_slice()
        } else {
            b"\x1b[?65;4;6;18;22;52c".as_slice()
        };
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(expected_da1.to_vec())
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"\x1b[2;1R".to_vec())
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"\x1bP1$r4 q\x1b\\".to_vec())
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"\x1bP1$r0;4:3m\x1b\\".to_vec())
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"\x1b[0;1;34m".to_vec())
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"\x1b[0;1;4m".to_vec())
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"\x1bP1$r2*x\x1b\\".to_vec())
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"\x1b[6;2R".to_vec())
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"\x1bP1$r1\"q\x1b\\".to_vec())
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"\x1bP2$u4/12\x1b\\".to_vec())
        );
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"\x1bP2$u6\x1b\\".to_vec())
        );
        let default_stops = (9..=initial_size.cols)
            .step_by(8)
            .map(|column| column.to_string())
            .collect::<Vec<_>>()
            .join("/");
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(format!("\x1bP2$u{default_stops}\x1b\\").into_bytes())
        );
        let generation = session.events.shell_generation().expect("running shell");
        for (text, connection) in [
            ("stale input", generation + 1),
            ("current input", generation),
        ] {
            session
                .input
                .send(
                    Command::Composed(text.into())
                        .mirrored_keyboard(connection)
                        .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(
            next(&mut observed, &session).await,
            Observed::Data(b"current input".to_vec())
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
            if text.contains(
                "remote ready\nx\nprotected header\nLa# ##fR\n La# ##fR\n  AB CDEF\n  12 3456",
            ) {
                assert!(!text.contains("transient"));
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
                tab_id: 2,
                tab_title: None,
                pane_name: None,
                label: "loopback".into(),
                active: false,
                visible: false,
                read_only: false,
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
            assert_eq!(sessions[0]["tab_id"], 2);
            assert_eq!(sessions[0]["cols"], 100);
            assert_eq!(sessions[0]["active"], false);
            assert_eq!(sessions[0]["visible"], false);
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
            verify_read_only(&session, &mut observed, pid).await;
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
        let archived = echoed(&session, &mut output, b"\x1b[22J\x1b[HAFTER").await;
        assert!(archived.rows[0].as_str().starts_with("AFTER"));
        assert_eq!(archived.first_stable_row, done.first_stable_row + 1);
        session.input.send(Command::Scroll(1)).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        assert!(session.frames.changed(archived.generation, deadline).await);
        let history = session.frames.load();
        assert_eq!(history.scroll_offset, 1);
        assert!(history.rows[0].as_str().starts_with("NEW"));
        assert!(history.rows[1].as_str().starts_with("AFTER"));
        drop(session);
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("server session did not close")
            .unwrap();
    })
    .await
    .expect("loopback SSH session timed out");
}

async fn verify_read_only(
    session: &Session,
    observed: &mut mpsc::UnboundedReceiver<Observed>,
    pid: Option<u32>,
) {
    use wezterm_term::{MouseButton, MouseEvent, MouseEventKind};
    let modes = b"\0sync:\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[?2004h";
    let (reply, result) = tokio::sync::oneshot::channel();
    session
        .input
        .send(Command::Raw(modes.to_vec(), reply))
        .unwrap();
    result.await.unwrap().unwrap();
    assert_eq!(
        next(observed, session).await,
        Observed::Data(modes.to_vec())
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let f = session.frames.load();
        if f.alternate && f.mouse_grabbed {
            break;
        }
        assert!(session.frames.changed(f.generation, deadline).await);
    }
    session.input.send(Command::SetReadOnly(true)).unwrap();
    for input in [
        Command::Key {
            key: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            down: true,
            repeat: false,
        },
        Command::DetailedKey {
            key: KeyCode::Char('a'),
            details: Default::default(),
            modifiers: KeyModifiers::NONE,
            down: true,
            repeat: false,
        },
        Command::Text("text".into()),
        Command::Composed("変換".into()),
        Command::Paste("bracketed".into()),
        Command::Mouse(MouseEvent {
            kind: MouseEventKind::Press,
            button: MouseButton::Left,
            x: 1,
            y: 1,
            x_pixel_offset: 0,
            y_pixel_offset: 0,
            modifiers: KeyModifiers::NONE,
        }),
        Command::Wheel(-1, KeyModifiers::NONE),
    ] {
        session.input.send(input).unwrap();
    }
    let reply = control::call(
        pid,
        &Request::SendText {
            pane: 7,
            data_base64: "eA==".into(),
            no_paste: true,
        },
    )
    .await
    .unwrap();
    assert!(!reply.ok && reply.error.unwrap().contains("read-only"));
    session
        .input
        .send(Command::Resize(TerminalSize {
            cols: 113,
            rows: 37,
            ..Default::default()
        }))
        .unwrap();
    // Every blocked command preceded this resize; the server asks for a
    // terminal version after observing it, while the pane is still locked.
    assert_eq!(next(observed, session).await, Observed::Resize(113, 37));
    assert_eq!(
        next(observed, session).await,
        Observed::Data(format!("\x1bP>|w11term {}\x1b\\", env!("CARGO_PKG_VERSION")).into_bytes())
    );
    session.input.send(Command::SetReadOnly(false)).unwrap();
    session
        .input
        .send(Command::Composed("ready".into()))
        .unwrap();
    assert_eq!(
        next(observed, session).await,
        Observed::Data(b"ready".to_vec())
    );
}

async fn next(observed: &mut mpsc::UnboundedReceiver<Observed>, session: &Session) -> Observed {
    tokio::time::timeout(Duration::from_secs(3), observed.recv())
        .await
        .unwrap_or_else(|_| panic!("SSH event timed out: {}", session.frames.load().status))
        .expect("server event channel closed")
}
