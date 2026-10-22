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
    options::{Authentication, SshOptions},
    prompts::{Answer, Prompts, Question},
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
struct Peer {
    seen: mpsc::UnboundedSender<Seen>,
    drop_password: Arc<std::sync::atomic::AtomicBool>,
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
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<()> {
        reply.accept().await;
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
        self.seen.send(Seen::Data(data.into()))?;
        if data == b"drop" {
            anyhow::bail!("simulated link failure");
        }
        if data == b"quit" {
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
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, seen) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut count = 0;
        let mut peers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {stream=listener.accept()=>{let(stream,_)=stream.unwrap();let config=Arc::new(server::Config{keys:vec![keys[count.min(keys.len()-1)].clone()],auth_rejection_time:Duration::ZERO,..Default::default()});count+=1;let seen=tx.clone();let drop_password=drop_password.clone();peers.spawn(async move{if let Ok(running)=server::run_stream(config,stream,Peer{seen,drop_password}).await{let _=running.await;}});},_=peers.join_next(),if !peers.is_empty()=>{}}
        }
    });
    (port, seen, task)
}
fn options(port: u16, authentication: Authentication, pin: Option<String>) -> SshOptions {
    SshOptions {
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
        player: Default::default(),
    }
}
fn key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
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
    assert!(response.await.unwrap().unwrap_err().contains("not queued"));
    client
        .input
        .send(TransportInput::Resize(wezterm_term::TerminalSize {
            cols: 170,
            rows: 48,
            ..Default::default()
        }))
        .unwrap();
    connected(&mut client, 2).await;
    assert!(prompts.current().is_none());
    let mut observations = Vec::new();
    while let Ok(event) = seen.try_recv() {
        observations.push(event);
    }
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
    client
        .input
        .send(TransportInput::Data(b"quit".to_vec()))
        .unwrap();
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
