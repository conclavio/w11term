//! Real isolated SSH peers exercise trust, private prompts, env, and reconnection.
use anyhow::Result;
use russh::{
    Channel, ChannelId,
    keys::{Algorithm, HashAlg, PrivateKey},
    server,
};
use std::{borrow::Cow, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot},
};
use w11term::{
    events::Events,
    options::{Authentication, Source, SshOptions},
    prompts::{Answer, Prompts, Question},
    session::{Command, Session},
    ssh::{self, Event},
    terminal::TransportInput,
};
#[derive(Clone, Debug, PartialEq)]
enum Seen {
    Password,
    Pty(u32, u32),
    Env(String, String),
    Data(Vec<u8>),
}
type OpenedShell = (Channel<server::Msg>, server::Handle);
struct Peer {
    seen: mpsc::UnboundedSender<Seen>,
    drop_password: Arc<std::sync::atomic::AtomicBool>,
    allowed_opens: usize,
    scripted_shells: Option<mpsc::UnboundedSender<OpenedShell>>,
}
impl server::Handler for Peer {
    type Error = anyhow::Error;
    async fn auth_none(&mut self, user: &str) -> Result<server::Auth> {
        Ok(if user == "none" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn auth_password(&mut self, user: &str, password: &str) -> Result<server::Auth> {
        self.seen.send(Seen::Password)?;
        if self
            .drop_password
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            anyhow::bail!("injected password transport failure");
        }
        Ok(if user == "pilot+character" && password == "secret" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        user: &str,
        _: &str,
        response: Option<server::Response<'a>>,
    ) -> Result<server::Auth> {
        Ok(if let Some(response) = response {
            let values: Vec<_> = response.map(|r| r.to_vec()).collect();
            if user == "pilot+character" && values == [b"pilot".to_vec(), b"123456".to_vec()] {
                server::Auth::Accept
            } else {
                server::Auth::reject()
            }
        } else {
            server::Auth::Partial {
                name: Cow::Borrowed("MFA"),
                instructions: Cow::Borrowed("Enter account and one-time code"),
                prompts: Cow::Owned(vec![
                    (Cow::Borrowed("Account"), true),
                    (Cow::Borrowed("Code"), false),
                ]),
            }
        })
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        session: &mut server::Session,
    ) -> Result<()> {
        if self.allowed_opens == 0 {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
        } else {
            self.allowed_opens -= 1;
            reply.accept().await;
            if let Some(shells) = &self.scripted_shells {
                shells
                    .send((channel, session.handle()))
                    .map_err(|_| anyhow::anyhow!("scripted peer closed"))?;
            }
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
        self.seen.send(Seen::Pty(cols, rows))?;
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
        self.seen.send(Seen::Env(name.into(), value.into()))?;
        Ok(())
    }
    async fn shell_request(&mut self, c: ChannelId, s: &mut server::Session) -> Result<()> {
        s.channel_success(c)?;
        s.data(c, b"game prompt> ".as_slice())?;
        Ok(())
    }
    async fn data(&mut self, c: ChannelId, data: &[u8], s: &mut server::Session) -> Result<()> {
        if self.scripted_shells.is_some() {
            return Ok(()); // The scripted peer reads each channel directly and supplies its output.
        }
        self.seen.send(Seen::Data(data.into()))?;
        if data == b"drop" {
            anyhow::bail!("simulated link failure");
        }
        if data == b"exit-with-tail" {
            s.exit_status_request(c, 7)?;
            s.data(
                c,
                b"\x1b[?2026h\r\nFINAL-AFTER-EXIT-STATUS\x1b[6n".as_slice(),
            )?;
            s.close(c)?;
        } else if data == b"quit" {
            s.exit_status_request(c, 0)?;
            s.close(c)?;
        } else {
            s.data(c, data.to_vec())?;
        }
        Ok(())
    }
}
struct Client {
    input: mpsc::UnboundedSender<TransportInput>,
    output: mpsc::Receiver<Event>,
    task: tokio::task::JoinHandle<Result<()>>,
}
fn start(options: SshOptions, prompts: Prompts) -> Client {
    let (input, rx) = mpsc::unbounded_channel();
    let (tx, output) = mpsc::channel(32);
    let task = tokio::spawn(ssh::run_configured(
        options,
        wezterm_term::TerminalSize::default(),
        rx,
        tx,
        Some(prompts),
        Events::default(),
    ));
    Client {
        input,
        output,
        task,
    }
}
async fn prompt(prompts: &Prompts) -> w11term::prompts::Prompt {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(p) = prompts.current() {
                break p;
            }
            prompts.changed().await;
        }
    })
    .await
    .unwrap()
}
async fn connected(client: &mut Client, generation: u64) {
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            match client.output.recv().await {
                Some(Event::Connected(n)) if n == generation => break,
                Some(_) => {}
                None => panic!(
                    "SSH closed before connected: {:?}",
                    client.task.is_finished()
                ),
            }
        }
    })
    .await
    .unwrap();
}
async fn fixture(
    keys: Vec<PrivateKey>,
) -> (
    u16,
    mpsc::UnboundedReceiver<Seen>,
    tokio::task::JoinHandle<()>,
) {
    fixture_with_password_drop(keys, Arc::new(std::sync::atomic::AtomicBool::new(false))).await
}
async fn fixture_with_password_drop(
    keys: Vec<PrivateKey>,
    drop_password: Arc<std::sync::atomic::AtomicBool>,
) -> (
    u16,
    mpsc::UnboundedReceiver<Seen>,
    tokio::task::JoinHandle<()>,
) {
    fixture_with_channel_limit(keys, drop_password, usize::MAX, None).await
}
async fn fixture_with_channel_limit(
    keys: Vec<PrivateKey>,
    drop_password: Arc<std::sync::atomic::AtomicBool>,
    allowed_opens: usize,
    scripted_shells: Option<mpsc::UnboundedSender<OpenedShell>>,
) -> (
    u16,
    mpsc::UnboundedReceiver<Seen>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, seen) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut count = 0;
        let mut peers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                stream = listener.accept() => {
                    let (stream, _) = stream.unwrap();
                    let config = Arc::new(server::Config {
                        keys: vec![keys[count.min(keys.len()-1)].clone()],
                        auth_rejection_time: Duration::ZERO,
                        ..Default::default()
                    });
                    count += 1;
                    let peer = Peer {
                        seen: tx.clone(),
                        drop_password: drop_password.clone(),
                        allowed_opens,
                        scripted_shells: scripted_shells.clone(),
                    };
                    peers.spawn(async move {
                        if let Ok(running) = server::run_stream(config, stream, peer).await {
                            let _ = running.await;
                        }
                    });
                }
                _ = peers.join_next(), if !peers.is_empty() => {}
            }
        }
    });
    (port, seen, task)
}
fn options(port: u16, authentication: Authentication, pin: Option<String>) -> SshOptions {
    SshOptions {
        scrollback_lines: Default::default(),
        color_scheme: Default::default(),
        terminal_type: Default::default(),
        host: "127.0.0.1".into(),
        user: if authentication == Authentication::None {
            "none"
        } else {
            "pilot+character"
        }
        .into(),
        port,
        authentication,
        host_key: pin,
        proxy_jump: Vec::new(),
        local_forwards: Vec::new(),
        durable: false,
        player: Default::default(),
    }
}
fn key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

async fn enter_password(session: &Session) {
    // The session worker consumes prompt notifications and publishes the prompt
    // for the UI. A second waiter on that notification can steal its wake-up.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let id = loop {
        let frame = session.frames.load();
        if let Some(question) = &frame.prompt {
            assert!(matches!(question.question, Question::Authentication { .. }));
            break question.id;
        }
        assert!(
            session.frames.changed(frame.generation, deadline).await,
            "password prompt did not reach the pane, status was {:?}",
            frame.status
        );
    };
    session
        .prompts
        .answer(
            id,
            Answer::Authentication(vec![zeroize::Zeroizing::new("secret".into())]),
        )
        .unwrap();
}

async fn connection(session: &Session) -> ssh::ConnectionShare {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(connection) = session.ssh_connection() {
                return connection;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("SSH pane did not establish a shell")
}

async fn wait_for_generation(session: &Session, previous: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = session.frames.load();
        if frame.connection_generation > previous {
            return;
        }
        assert!(
            session.frames.changed(frame.generation, deadline).await,
            "{}",
            frame.status
        );
    }
}

async fn send(session: &Session, text: &str) {
    let (reply, response) = oneshot::channel();
    session
        .input
        .send(Command::Raw(text.as_bytes().to_vec(), reply))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), response)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

async fn expect_echo(session: &Session, text: &str) {
    send(session, text).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while !session
            .frames
            .load()
            .rows
            .iter()
            .any(|row| row.as_str().contains(text))
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("expected echo on this pane");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn theme_reports_follow_local_scheme_on_the_subscribed_ssh_channel_only() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use w11term::options::ColorScheme::{self, OneHalfDark, OneHalfLight};

    // A standard operating-status DSR provides an ordered end marker. This checks
    // both the complete wire bytes and silence without a timer-based quiet period.
    async fn exchange(
        channel: &mut russh::ChannelStream<server::Msg>,
        command: &[u8],
        reports: &[u8],
    ) {
        channel
            .write_all(&[command, b"\x1b[5n"].concat())
            .await
            .unwrap();
        let expected = [reports, b"\x1b[0n"].concat();
        let mut received = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(3), async {
            let mut buf = [0; 128];
            while !received.ends_with(b"\x1b[0n") {
                let len = channel.read(&mut buf).await.unwrap();
                assert_ne!(len, 0, "SSH channel closed while awaiting replies");
                received.extend_from_slice(&buf[..len]);
                assert!(received.len() <= 1024, "unexpected report stream");
            }
        })
        .await;
        assert!(
            read.is_ok(),
            "missing DSR fence; command={command:?}, received={received:?}"
        );
        assert_eq!(received, expected, "command={command:?}");
    }
    async fn set_scheme(pane: &Session, scheme: ColorScheme) {
        pane.input.send(Command::SetColorScheme(scheme)).unwrap();
        // The SSH peer can send a DSR as soon as the worker has applied this change.
        let (tx, rx) = oneshot::channel();
        pane.input.send(Command::Capture(tx)).unwrap();
        rx.await.unwrap();
    }
    const DARK: &[u8] = b"\x1b[?997;1n";
    const LIGHT: &[u8] = b"\x1b[?997;2n";
    let key = key();
    let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let (tx, mut shells) = mpsc::unbounded_channel();
    let (port, _seen, server) =
        fixture_with_channel_limit(vec![key], Arc::default(), usize::MAX, Some(tx)).await;
    let source = Source::Ssh(options(port, Authentication::None, Some(pin)));
    let pane = Session::start(source.clone());
    let link = connection(&pane).await;
    wait_for_generation(&pane, 0).await;
    let first_generation = pane.frames.load().connection_generation;
    let (first, server_link) = shells.recv().await.unwrap();
    let sibling =
        Session::start_configured_with_connection(source, Default::default(), false, Some(link));
    drop(connection(&sibling).await);
    let (other, _) = shells.recv().await.unwrap();
    assert_ne!(
        first.id(),
        other.id(),
        "expected separate PTYs on one transport"
    );
    let mut subscribed = first.into_stream();
    let mut independent = other.into_stream();

    exchange(&mut subscribed, b"\x1b[?996n", DARK).await;
    exchange(
        &mut subscribed,
        b"\x1b[?2031h\x1b[?2031$p",
        b"\x1b[?2031;1$y",
    )
    .await;
    set_scheme(&pane, OneHalfLight).await;
    set_scheme(&sibling, OneHalfLight).await;
    exchange(&mut subscribed, b"", LIGHT).await;
    exchange(&mut independent, b"\x1b[?996n", LIGHT).await;
    set_scheme(&pane, OneHalfLight).await;
    exchange(&mut subscribed, b"", b"").await;
    set_scheme(&pane, OneHalfDark).await;
    set_scheme(&pane, ColorScheme::Default).await;
    exchange(&mut subscribed, b"", &[DARK, DARK].concat()).await;
    // Remote applications can change the appearance of the whole screen. Only a
    // local scheme change emits an unsolicited report; the explicit query still answers dark.
    exchange(
        &mut subscribed,
        b"\x1b]11;#ffffff\x07\x1b[7m\x1b[?5h\x1b[?996n",
        DARK,
    )
    .await;
    exchange(
        &mut subscribed,
        b"\x1b[?2031l\x1b[?2031$p",
        b"\x1b[?2031;2$y",
    )
    .await;
    set_scheme(&pane, OneHalfLight).await;
    exchange(&mut subscribed, b"", b"").await;
    exchange(&mut independent, b"", b"").await;

    // Reconnect with the original pane still subscribed and verify the new shell
    // must opt in for itself, even though the locally selected light scheme persists.
    exchange(
        &mut subscribed,
        b"\x1b[?2031h\x1b[?2031$p",
        b"\x1b[?2031;1$y",
    )
    .await;
    drop(sibling);
    let mut tail = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), independent.read_to_end(&mut tail))
        .await
        .unwrap()
        .unwrap();
    assert!(tail.is_empty());
    server_link
        .disconnect(
            russh::Disconnect::ByApplication,
            "fixture reconnect".into(),
            "en".into(),
        )
        .await
        .unwrap();
    wait_for_generation(&pane, first_generation).await;
    let (new_shell, _) = tokio::time::timeout(Duration::from_secs(3), shells.recv())
        .await
        .unwrap()
        .unwrap();
    let mut replacement = new_shell.into_stream();
    exchange(
        &mut replacement,
        b"\x1b[?2031$p\x1b[?996n",
        &[b"\x1b[?2031;2$y".as_slice(), LIGHT].concat(),
    )
    .await;
    set_scheme(&pane, OneHalfDark).await;
    exchange(&mut replacement, b"", b"").await;
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_channels_outlive_original_pane_and_sibling_shell_exit() {
    let key = key();
    let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let (port, mut seen, server) = fixture(vec![key]).await;
    let source = Source::Ssh(options(port, Authentication::Password, Some(pin)));
    let original = Session::start(source.clone());
    enter_password(&original).await;
    let child = Session::start_configured_with_connection(
        source.clone(),
        Default::default(),
        false,
        Some(connection(&original).await),
    );
    let sibling = Session::start_configured_with_connection(
        source.clone(),
        Default::default(),
        false,
        Some(connection(&child).await),
    );
    drop(connection(&sibling).await);
    drop(original);
    expect_echo(&child, "after-owner-closed").await;
    send(&sibling, "quit").await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while sibling.ssh_connection().is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("exited shell still offering its connection");
    expect_echo(&child, "after-peer-exited").await;
    assert_eq!(
        std::iter::from_fn(|| seen.try_recv().ok())
            .filter(|e| *e == Seen::Password)
            .count(),
        1
    );
    // An ordinary open of this exact profile still gets a separate login.
    let independent = Session::start(source);
    enter_password(&independent).await;
    drop(connection(&independent).await);
    drop((child, sibling, independent));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_split_channel_authenticates_independently_without_closing_original() {
    let key = key();
    let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let (port, mut seen, server) =
        fixture_with_channel_limit(vec![key], Default::default(), 1, None).await;
    let source = Source::Ssh(options(port, Authentication::Password, Some(pin)));
    let original = Session::start(source.clone());
    enter_password(&original).await;
    let child = Session::start_configured_with_connection(
        source,
        Default::default(),
        false,
        Some(connection(&original).await),
    );
    enter_password(&child).await;
    drop(connection(&child).await);
    expect_echo(&original, "still-first-login").await;
    expect_echo(&child, "second-login").await;
    assert_eq!(
        std::iter::from_fn(|| seen.try_recv().ok())
            .filter(|e| *e == Seen::Password)
            .count(),
        2
    );
    drop((original, child));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_with_different_host_pin_requires_fresh_verification() {
    let actual = key();
    let pin = actual.public_key().fingerprint(HashAlg::Sha256).to_string();
    let other = key().public_key().fingerprint(HashAlg::Sha256).to_string();
    let (port, _seen, server) = fixture(vec![actual]).await;
    let original = Session::start(Source::Ssh(options(port, Authentication::None, Some(pin))));
    let child = Session::start_configured_with_connection(
        Source::Ssh(options(port, Authentication::None, Some(other))),
        Default::default(),
        false,
        Some(connection(&original).await),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = child.frames.load();
            assert_eq!(
                frame.connection_generation, 0,
                "the mismatched pin was ignored"
            );
            if frame.status.contains("host verification failed") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("mismatched split pin should fail host verification");
    let (first, sequence) = exit_token(&child, 0).await;
    child.restart_exited(first).unwrap();
    let (second, _) = exit_token(&child, sequence).await;
    assert!(second > first);
    assert!(
        child
            .frames
            .load()
            .status
            .contains("host verification failed")
    );
    assert_eq!(child.frames.load().connection_generation, 0);
    expect_echo(&original, "still-trusted").await;
    drop((child, original));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_reconnects_using_its_own_authentication_after_link_loss() {
    let key = key();
    let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let (port, _seen, server) = fixture(vec![key]).await;
    let source = Source::Ssh(options(port, Authentication::Password, Some(pin)));
    let original = Session::start(source.clone());
    enter_password(&original).await;
    let child = Session::start_configured_with_connection(
        source,
        Default::default(),
        false,
        Some(connection(&original).await),
    );
    drop(connection(&child).await);
    send(&child, "\x1b]133;C\x07").await;
    expect_echo(&child, "old-command-running").await;
    assert!(child.events.set_command_watch(true));
    send(&child, "drop").await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while child.events.command_watch().is_some() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a disconnected pane should cancel its command watch");
    assert!(!child.events.set_command_watch(true));
    assert!(child.events.restart_token().is_none());
    assert!(child.restart_exited(1).is_err());
    enter_password(&child).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while [&original, &child]
            .iter()
            .any(|pane| pane.frames.load().connection_generation < 2)
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("both panes should independently reconnect");
    assert!(original.prompts.current().is_none());
    assert_eq!(child.events.command_watch(), Some(false));
    assert!(child.events.restart_token().is_none());
    send(&child, "\x1b]133;D;23\x07").await;
    expect_echo(&child, "ready-again").await;
    assert!(
        !child
            .events
            .since(0)
            .events
            .iter()
            .any(|e| e.kind == "command-finished")
    );
    drop((child, original));
    server.abort();
}

async fn exit_token(pane: &Session, after: u64) -> (u64, u64) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut cursor = after;
        loop {
            let batch = pane.events.wait(cursor, Duration::from_secs(4)).await;
            if let Some(event) = batch
                .events
                .iter()
                .find(|event| event.kind == "pane-exited")
            {
                return (event.data["exit_token"].as_u64().unwrap(), event.sequence);
            }
            cursor = batch.next;
        }
    })
    .await
    .expect("no terminal exit after final SSH data")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_restart_publishes_final_output_and_requires_fresh_authentication() {
    let key = key();
    let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let (port, mut seen, server) = fixture(vec![key]).await;
    let pane = Session::start(Source::Ssh(options(
        port,
        Authentication::Password,
        Some(pin),
    )));
    enter_password(&pane).await;
    wait_for_generation(&pane, 0).await;
    expect_echo(&pane, "first-login").await;
    send(&pane, "exit-with-tail").await;
    let (token, seq) = exit_token(&pane, 0).await;
    assert!(
        pane.frames
            .load()
            .rows
            .iter()
            .any(|row| row.as_str().contains("FINAL-AFTER-EXIT-STATUS"))
    );
    assert_eq!(pane.events.restart_token(), Some(token));
    let old_generation = pane.frames.load().connection_generation;
    while seen.try_recv().is_ok() {}
    let (reply, receipt) = oneshot::channel();
    pane.input
        .send(Command::Raw(b"stale raw input".to_vec(), reply))
        .unwrap();
    assert!(receipt.await.unwrap().is_err());
    let successes = std::thread::scope(|scope| {
        let one = scope.spawn(|| pane.restart_exited(token).is_ok());
        let two = pane.restart_exited(token).is_ok();
        usize::from(one.join().unwrap()) + usize::from(two)
    });
    assert_eq!(successes, 1);
    assert!(pane.events.restart_token().is_none());
    for _ in 0..256 {
        pane.input.send(Command::Text("offline".into())).unwrap();
    }
    enter_password(&pane).await;
    wait_for_generation(&pane, old_generation).await;
    expect_echo(&pane, "second-login").await;
    assert!(pane.frames.load().connection_generation > old_generation);
    let mut inputs = vec![];
    while let Ok(record) = seen.try_recv() {
        if let Seen::Data(bytes) = record {
            inputs.push(bytes);
        }
    }
    assert_eq!(inputs, [b"second-login".to_vec()]);
    send(&pane, "quit").await;
    let (new_token, new_seq) = exit_token(&pane, seq).await;
    assert!(new_token > token && new_seq > seq);
    assert!(pane.restart_exited(token).is_err());
    // Enrollment and revocation broadcast this even to exited sibling panes.
    pane.input
        .send(Command::Authenticate(Authentication::KeyboardInteractive))
        .unwrap();
    pane.restart_exited(new_token).unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = pane.frames.load();
        if let Some(prompt) = &frame.prompt {
            assert!(
                matches!(&prompt.question, Question::Authentication { challenges, .. } if challenges.len() == 2)
            );
            pane.prompts
                .answer(
                    prompt.id,
                    Answer::Authentication(
                        ["pilot", "123456"]
                            .map(|s| zeroize::Zeroizing::new(s.into()))
                            .into(),
                    ),
                )
                .unwrap();
            break;
        }
        assert!(pane.frames.changed(frame.generation, deadline).await);
    }
    wait_for_generation(&pane, old_generation + 1).await;
    expect_echo(&pane, "updated-auth").await;
    send(&pane, "quit").await;
    assert!(exit_token(&pane, new_seq).await.0 > new_token);
    let journal = pane.events.clone();
    let input = pane.input.clone();
    drop(pane);
    assert!(journal.restart_token().is_none());
    assert!(input.is_closed());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn password_reconnect_uses_memory_credential_latest_grid_and_never_replays_offline_input() {
    let key = key();
    let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let (port, mut seen, server) = fixture(vec![key]).await;
    let prompts = Prompts::default();
    let mut client = start(
        options(port, Authentication::Password, Some(pin)),
        prompts.clone(),
    );
    let question = prompt(&prompts).await;
    assert!(
        matches!(question.question,Question::Authentication{ref challenges,..}if !challenges[0].echo)
    );
    prompts
        .answer(
            question.id,
            Answer::Authentication(vec![zeroize::Zeroizing::new("secret".into())]),
        )
        .unwrap();
    connected(&mut client, 1).await;
    client
        .input
        .send(TransportInput::Data(b"drop".to_vec()))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Event::Status(text)) = client.output.recv().await
                && text.contains("reconnecting")
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    let (reply, response) = oneshot::channel();
    client
        .input
        .send(TransportInput::Raw(b"MUST_NOT_REPLAY".to_vec(), reply))
        .unwrap();
    let (reply, clipboard_response) = oneshot::channel();
    client
        .input
        .send(TransportInput::Clipboard {
            connection: 1,
            bytes: b"OFFLINE_PASTE".to_vec(),
            reply,
        })
        .unwrap();
    client
        .input
        .send(TransportInput::Resize(wezterm_term::TerminalSize {
            cols: 170,
            rows: 48,
            ..Default::default()
        }))
        .unwrap();
    assert!(response.await.unwrap().unwrap_err().contains("not queued"));
    assert!(clipboard_response.await.unwrap().is_err());
    connected(&mut client, 2).await;
    assert!(prompts.current().is_none());
    // The terminal actor may have approved the paste before the reconnect,
    // but its queued transport message now reaches the replacement shell.
    for (generation, paste, accepted) in [(1, "STALE_PASTE", false), (2, "CURRENT_PASTE", true)] {
        let (reply, response) = oneshot::channel();
        client
            .input
            .send(TransportInput::Clipboard {
                connection: generation,
                bytes: paste.as_bytes().into(),
                reply,
            })
            .unwrap();
        assert_eq!(response.await.unwrap().is_ok(), accepted);
    }
    client
        .input
        .send(TransportInput::Data(b"quit".to_vec()))
        .unwrap();
    let mut observations = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = seen.recv().await {
            let arrived = event == Seen::Data(b"quit".to_vec());
            observations.push(event);
            if arrived {
                return;
            }
        }
        panic!("SSH fixture ended before the last input arrived");
    })
    .await
    .unwrap();
    assert_eq!(
        observations
            .iter()
            .filter(|e| **e == Seen::Password)
            .count(),
        2
    );
    assert!(observations.contains(&Seen::Pty(170, 48)));
    assert_eq!(
        observations
            .iter()
            .filter(|e| **e == Seen::Env("COLORTERM".into(), "truecolor".into()))
            .count(),
        2
    );
    assert!(!observations.contains(&Seen::Data(b"MUST_NOT_REPLAY".to_vec())));
    assert!(!observations.contains(&Seen::Data(b"OFFLINE_PASTE".to_vec())));
    assert!(!observations.contains(&Seen::Data(b"STALE_PASTE".to_vec())));
    let paste = Seen::Data(b"CURRENT_PASTE".to_vec());
    assert_eq!(observations.iter().filter(|e| **e == paste).count(), 1);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), client.task)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    server.abort();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keyboard_interactive_preserves_echo_flags_and_roundtrips_answers() {
    let key = key();
    let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let (port, _seen, server) = fixture(vec![key]).await;
    let prompts = Prompts::default();
    let mut client = start(
        options(port, Authentication::KeyboardInteractive, Some(pin)),
        prompts.clone(),
    );
    let question = prompt(&prompts).await;
    assert!(
        matches!(question.question,Question::Authentication{ref challenges,..}if challenges.len()==2&&challenges[0].echo&&!challenges[1].echo)
    );
    prompts
        .answer(
            question.id,
            Answer::Authentication(vec![
                zeroize::Zeroizing::new("pilot".into()),
                zeroize::Zeroizing::new("123456".into()),
            ]),
        )
        .unwrap();
    connected(&mut client, 1).await;
    client.task.abort();
    server.abort();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_trust_is_saved_known_host_skips_prompt_and_changed_key_fails() {
    let first = key();
    let fingerprint = first.public_key().fingerprint(HashAlg::Sha256).to_string();
    let (port, _seen, server) = fixture(vec![first.clone(), first, key()]).await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("known_hosts");
    let prompts = Prompts::with_known_hosts(path.clone());
    let mut first = start(options(port, Authentication::None, None), prompts.clone());
    let question = prompt(&prompts).await;
    assert!(
        matches!(question.question,Question::HostKey{fingerprint:ref fp,..}if fp==&fingerprint)
    );
    prompts.answer(question.id, Answer::Trust(true)).unwrap();
    connected(&mut first, 1).await;
    first.task.abort();
    let saved = std::fs::read(&path).unwrap();
    let mut second = start(options(port, Authentication::None, None), prompts.clone());
    connected(&mut second, 1).await;
    assert!(prompts.current().is_none());
    second.task.abort();
    let third = start(options(port, Authentication::None, None), prompts.clone());
    let failure = tokio::time::timeout(Duration::from_secs(4), third.task)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(format!("{failure:#}").contains("host verification failed"));
    assert!(prompts.current().is_none());
    assert_eq!(std::fs::read(path).unwrap(), saved);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the disposable stock OpenSSH guest in tests/folder-sharing"]
async fn stock_openssh_host_certificate() -> Result<()> {
    let port = std::env::var("W11TERM_CERT_TEST_PORT")?.parse()?;
    let authority = std::env::var("W11TERM_CERT_TEST_CA")?;
    let identity =
        std::env::var_os("W11TERM_CERT_TEST_IDENTITY").expect("missing fixture identity");
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("known_hosts");
    let record = |host: &str, ca: &str| format!("@cert-authority [{host}]:{port} {ca}\n");
    let connect = |host: &str, authentication| {
        let mut target = options(port, authentication, None);
        target.host = host.into();
        target.user = "w11fixture".into();
        target.player.reconnect = false;
        let prompts = Prompts::with_known_hosts(path.clone());
        (start(target, prompts.clone()), prompts)
    };

    // Stock sshd offers both the leaf and its certificate. Only the signing CA is trusted.
    let valid = record("127.0.0.1", &authority);
    std::fs::write(&path, &valid)?;
    let (mut client, prompts) = connect(
        "127.0.0.1",
        Authentication::Identity {
            identity: identity.into(),
        },
    );
    let mut transcript = Vec::new();
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            tokio::select! {
                _ = prompts.changed() => anyhow::bail!("unexpected prompt during certificate login: {:?}", prompts.current()),
                event = client.output.recv() => match event {
                    Some(Event::Connected(1)) => {
                        // The echoed command does not contain the expected response as a literal.
                        client.input.send(TransportInput::Data(b"printf '\\nw11term-cert-%s\\n' \"$(id -un)\"; exit\r".to_vec()))
                            .map_err(|_| anyhow::anyhow!("SSH input closed before the login probe"))?;
                    }
                    Some(Event::Data(bytes)) => transcript.extend_from_slice(&bytes),
                    Some(_) => {}
                    None => break,
                }
            }
        }
        client.task.await?
    }).await??;
    let transcript = String::from_utf8_lossy(&transcript);
    assert!(
        transcript
            .lines()
            .any(|line| line == "w11term-cert-w11fixture"),
        "unexpected shell response: {transcript:?}"
    );
    assert_eq!(std::fs::read_to_string(&path)?, valid);

    // localhost reaches the same listener, but only 127.0.0.1 is a signed principal.
    let other_ca = key().public_key().to_openssh()?;
    for (host, ca, expected) in [
        ("localhost", authority.as_str(), "no matching principal"),
        (
            "127.0.0.1",
            other_ca.as_str(),
            "untrusted SSH host certificate",
        ),
    ] {
        let saved = record(host, ca);
        std::fs::write(&path, &saved)?;
        // A password prompt here would prove we started authentication before validating the certificate.
        let (mut client, prompts) = connect(host, Authentication::Password);
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::select! {
                result = &mut client.task => result,
                _ = prompts.changed() => panic!("unexpected prompt for {host}: {:?}", prompts.current()),
            }
        }).await??;
        let error = result.expect_err("untrusted connection accepted");
        assert!(format!("{error:#}").contains(expected), "{host}: {error:#}");
        assert_eq!(std::fs::read_to_string(&path)?, saved);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoked_key_fails_before_prompt_or_auth_even_with_an_explicit_pin() {
    let server_key = key();
    let pin = server_key
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();
    let (port, mut seen, server) = fixture(vec![server_key.clone()]).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");
    let record = format!(
        "@revoked [127.0.0.1]:{port} {}\n",
        server_key.public_key().to_openssh().unwrap()
    );
    std::fs::write(&path, &record).unwrap();
    for pin in [None, Some(pin)] {
        let prompts = Prompts::with_known_hosts(path.clone());
        let client = start(
            options(port, Authentication::Password, pin),
            prompts.clone(),
        );
        let error = tokio::time::timeout(Duration::from_secs(3), client.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(format!("{error:#}").contains("revoked"));
        assert!(prompts.current().is_none());
    }
    assert!(seen.try_recv().is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), record);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn password_transport_failure_reuses_the_candidate_without_a_second_prompt() {
    let key = key();
    let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let (port, mut seen, server) = fixture_with_password_drop(
        vec![key],
        Arc::new(std::sync::atomic::AtomicBool::new(true)),
    )
    .await;
    let prompts = Prompts::default();
    let mut client = start(
        options(port, Authentication::Password, Some(pin)),
        prompts.clone(),
    );
    let question = prompt(&prompts).await;
    prompts
        .answer(
            question.id,
            Answer::Authentication(vec![zeroize::Zeroizing::new("secret".into())]),
        )
        .unwrap();
    connected(&mut client, 1).await;
    assert!(prompts.current().is_none());
    let mut attempts = 0;
    while let Ok(event) = seen.try_recv() {
        if event == Seen::Password {
            attempts += 1;
        }
    }
    assert_eq!(attempts, 2);
    client.task.abort();
    server.abort();
}
