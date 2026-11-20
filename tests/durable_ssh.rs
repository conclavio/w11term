//! The loopback peer implements the zmx CLI response contract. Each attach
//! and control command still runs on a real authenticated SSH channel.
use anyhow::{Result, bail};
use russh::{
    Channel, ChannelId,
    keys::{Algorithm, HashAlg, PrivateKey},
    server,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Notify, oneshot},
};
use w11term::{
    durable::DurableReference,
    options::{Source, SshOptions},
    session::{Command, Session},
    ssh::{DurableAction, DurablePhase},
};

#[cfg(feature = "ui-smoke")]
#[path = "support/native_gui.rs"]
mod gui;

#[cfg(feature = "ui-smoke")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens an isolated native WinUI window"]
async fn native_durable_workspace() {
    let mut fixture = Fixture::new().await;
    fixture.options.durable = true;
    let dir = tempfile::tempdir().unwrap();
    gui::run_gui(
        &Source::Ssh(fixture.options.clone()),
        dir.path(),
        &[("W11TERM_DURABLE_SMOKE", "1")],
        "Durable workspace smoke passed",
    )
    .await;
    let backend = fixture.peer.backend.lock().unwrap();
    assert!(backend.shell.is_none());
    let count = |prefix: &str| {
        backend
            .commands
            .iter()
            .filter(|cmd| cmd.starts_with(prefix))
            .count()
    };
    assert_eq!(count("zmx attach --labels"), 1);
    assert_eq!(
        backend
            .commands
            .iter()
            .filter(|cmd| cmd.ends_with(" /bin/false"))
            .count(),
        1
    );
    assert_eq!(count("zmx kill"), 1);
}

#[derive(Default)]
struct Backend {
    shell: Option<(String, String)>,
    commands: Vec<String>,
    input: Vec<Vec<u8>>,
    grids: Vec<(u32, u32)>,
    missing_program: bool,
    hold_once: bool,
    replace_on_resume: bool,
}
#[derive(Clone, Default)]
struct Peer {
    backend: Arc<Mutex<Backend>>,
    checking: Arc<Notify>,
    release: Arc<Notify>,
}
impl server::Handler for Peer {
    type Error = anyhow::Error;
    async fn auth_none(&mut self, user: &str) -> Result<server::Auth> {
        Ok(if user == "pilot" {
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
        reply.accept().await;
        Ok(())
    }
    async fn pty_request(
        &mut self,
        c: ChannelId,
        _: &str,
        columns: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        s: &mut server::Session,
    ) -> Result<()> {
        self.backend.lock().unwrap().grids.push((columns, rows));
        s.channel_success(c)?;
        Ok(())
    }
    async fn shell_request(&mut self, c: ChannelId, s: &mut server::Session) -> Result<()> {
        self.backend
            .lock()
            .unwrap()
            .commands
            .push("<direct shell>".into());
        s.channel_success(c)?;
        Ok(())
    }
    async fn data(&mut self, c: ChannelId, data: &[u8], s: &mut server::Session) -> Result<()> {
        if data == b"lose transport" {
            bail!("injected SSH loss")
        }
        self.backend.lock().unwrap().input.push(data.to_vec());
        s.data(c, data.to_vec())?;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        c: ChannelId,
        data: &[u8],
        s: &mut server::Session,
    ) -> Result<()> {
        let command = std::str::from_utf8(data)?;
        let args: Vec<_> = command.split_whitespace().collect();
        s.channel_success(c)?;
        let (status, text, hold) = {
            let mut b = self.backend.lock().unwrap();
            b.commands.push(command.into());
            match args.as_slice() {
                ["zmx", "version"] if b.missing_program => (127, String::new(), false),
                ["zmx", "version"] => (0, "zmx\t\t1.0.1\nghostty_vt\tfixture\n".into(), false),
                ["zmx", "get", name, "w11term_generation"] => {
                    let label = b
                        .shell
                        .as_ref()
                        .filter(|(n, _)| n == name)
                        .map(|(_, g)| g.clone());
                    let hold = std::mem::take(&mut b.hold_once);
                    (
                        if label.is_some() { 0 } else { 1 },
                        label.unwrap_or_default(),
                        hold,
                    )
                }
                ["zmx", "attach", "--labels", label, name] => {
                    b.shell = Some((
                        name.to_string(),
                        label.strip_prefix("w11term_generation=").unwrap().into(),
                    ));
                    s.data(c, b"initial-screen".as_slice())?;
                    return Ok(());
                }
                ["zmx", "attach", name, "/bin/false"] => {
                    if b.replace_on_resume {
                        b.shell = Some((
                            name.to_string(),
                            "00000000000000000000000000000000".to_owned(),
                        ));
                    }
                    s.data(c, b"restore-screen".as_slice())?;
                    return Ok(());
                }
                ["zmx", "kill", name] => {
                    assert!(b.shell.as_ref().is_some_and(|(n, _)| n == name));
                    b.shell = None;
                    (0, "killed".into(), false)
                }
                _ => bail!("unexpected SSH command {command}"),
            }
        };
        if hold {
            self.checking.notify_one();
            self.release.notified().await;
        }
        if !text.is_empty() {
            s.data(c, text.into_bytes())?;
        }
        s.exit_status_request(c, status)?;
        s.close(c)?;
        Ok(())
    }
}
struct Fixture {
    peer: Peer,
    options: SshOptions,
    key: Arc<Mutex<PrivateKey>>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        let key = Arc::new(Mutex::new(key));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let options=serde_json::from_value(serde_json::json!({"host":"127.0.0.1","user":"pilot","port":port,"auth":"none","host_key":pin})).unwrap();
        let peer = Peer::default();
        let handler = peer.clone();
        let keys = key.clone();
        let server = tokio::spawn(async move {
            let mut peers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    socket=listener.accept()=>{
                        let config=Arc::new(server::Config {keys:vec![keys.lock().unwrap().clone()],auth_rejection_time:Duration::ZERO,..Default::default()});
                        let peer=handler.clone();
                        peers.spawn(async move {if let Ok(s)=server::run_stream(config,socket.unwrap().0,peer).await {let _=s.await;}});
                    }
                    _=peers.join_next(),if !peers.is_empty()=>{}
                }
            }
        });
        Self {
            peer,
            options,
            key,
            server,
        }
    }
    fn open(&self, saved: Option<DurableReference>) -> Session {
        Session::start_durable(
            Source::Ssh(self.options.clone()),
            Default::default(),
            false,
            None,
            saved,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn phase(session: &Session, want: DurablePhase) {
    let timeout = tokio::time::timeout(Duration::from_secs(7), async {
        while session.durable_state().unwrap().phase != want {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        timeout.is_ok(),
        "expected {want:?}, got {:?}",
        session.durable_state().unwrap()
    );
}
async fn raw(session: &Session, text: &str) -> Result<(), String> {
    let (reply, result) = oneshot::channel();
    session
        .input
        .send(Command::Raw(text.as_bytes().to_vec(), reply))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), result)
        .await
        .unwrap()
        .unwrap()
}
fn screen(session: &Session, text: &str) -> bool {
    session
        .frames
        .load()
        .rows
        .iter()
        .any(|row| row.as_str().contains(text))
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_input_waits_for_proof_and_survives_detach_link_loss_and_restart() {
    use DurableAction::*;
    use DurablePhase::*;
    let fixture = Fixture::new().await;
    fixture.peer.backend.lock().unwrap().hold_once = true;
    let pane = fixture.open(None);
    tokio::time::timeout(Duration::from_secs(5), fixture.peer.checking.notified())
        .await
        .unwrap();
    assert!(raw(&pane, "early").await.is_err());
    assert!(!screen(&pane, "initial-screen"));
    fixture.peer.release.notify_one();
    phase(&pane, Attached).await;
    let reference = pane.durable_state().unwrap().reference.unwrap();
    raw(&pane, "first").await.unwrap();
    pane.durable_action(Detach).unwrap();
    phase(&pane, Detached).await;
    pane.input
        .send(Command::Resize(wezterm_term::TerminalSize {
            rows: 48,
            cols: 172,
            ..Default::default()
        }))
        .unwrap();
    assert!(raw(&pane, "offline").await.is_err());
    pane.durable_action(Reattach).unwrap();
    phase(&pane, Attached).await;
    raw(&pane, "lose transport").await.unwrap();
    phase(&pane, Reconnecting).await;
    phase(&pane, Attached).await;
    assert_eq!(
        pane.durable_state().unwrap().reference.as_ref(),
        Some(&reference)
    );
    drop(pane);
    let reopened = fixture.open(Some(reference));
    phase(&reopened, Attached).await;
    raw(&reopened, "after restart").await.unwrap();
    reopened.durable_action(End).unwrap();
    phase(&reopened, Ended).await;
    let b = fixture.peer.backend.lock().unwrap();
    assert!(b.shell.is_none());
    assert!(b.grids.contains(&(172, 48)));
    assert_eq!(b.input, [b"first".to_vec(), b"after restart".to_vec()]);
    assert_eq!(
        b.commands
            .iter()
            .filter(|c| c.starts_with("zmx attach --labels"))
            .count(),
        1
    );
    assert_eq!(
        b.commands
            .iter()
            .filter(|c| c.ends_with(" /bin/false"))
            .count(),
        3
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_or_replaced_generation_never_enters_a_substitute_shell() {
    use DurableAction::*;
    use DurablePhase::*;
    let fixture = Fixture::new().await;
    let pane = fixture.open(None);
    phase(&pane, Attached).await;
    pane.durable_action(Detach).unwrap();
    phase(&pane, Detached).await;
    fixture.peer.backend.lock().unwrap().replace_on_resume = true;
    pane.durable_action(Reattach).unwrap();
    phase(&pane, Missing).await;
    assert!(!screen(&pane, "restore-screen"));
    assert!(raw(&pane, "wrong shell").await.is_err());
    assert!(pane.durable_action(End).is_err());
    let before = fixture.peer.backend.lock().unwrap().commands.len();
    pane.durable_action(Reattach).unwrap();
    phase(&pane, Missing).await;
    assert!(
        !fixture.peer.backend.lock().unwrap().commands[before..]
            .iter()
            .any(|c| c.starts_with("zmx attach"))
    );
    pane.durable_action(StartNew).unwrap();
    phase(&pane, Attached).await;
    assert_eq!(
        fixture
            .peer
            .backend
            .lock()
            .unwrap()
            .commands
            .iter()
            .filter(|c| c.starts_with("zmx attach --labels"))
            .count(),
        2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unavailable_backend_requires_explicit_direct_fallback() {
    use DurablePhase::*;
    let fixture = Fixture::new().await;
    fixture.peer.backend.lock().unwrap().missing_program = true;
    let pane = fixture.open(None);
    phase(&pane, Unavailable).await;
    assert!(raw(&pane, "fallback").await.is_err());
    assert_eq!(
        fixture.peer.backend.lock().unwrap().commands,
        ["zmx version"]
    );
    pane.durable_action(DurableAction::UseDirectShell).unwrap();
    phase(&pane, Direct).await;
    raw(&pane, "direct input").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn changed_authenticated_host_or_saved_account_cannot_end_or_reattach() {
    use DurableAction::*;
    use DurablePhase::*;
    let fixture = Fixture::new().await;
    let pane = fixture.open(None);
    phase(&pane, Attached).await;
    let mut wrong = pane.durable_state().unwrap().reference.unwrap();
    wrong.user = "other".into();
    let count = fixture.peer.backend.lock().unwrap().commands.len();
    let alien = fixture.open(Some(wrong));
    phase(&alien, Failed).await;
    assert_eq!(fixture.peer.backend.lock().unwrap().commands.len(), count);
    pane.durable_action(Detach).unwrap();
    phase(&pane, Detached).await;
    *fixture.key.lock().unwrap() =
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    // With an unpinned profile the saved fingerprint is an additional check.
    // A changed host must not reach the trust prompt, authentication or zmx.
    let mut unpinned = fixture.options.clone();
    unpinned.host_key = None;
    let wrong_peer = Session::start_durable(
        Source::Ssh(unpinned),
        Default::default(),
        false,
        None,
        pane.durable_state().unwrap().reference,
    );
    phase(&wrong_peer, Failed).await;
    assert!(wrong_peer.prompts.current().is_none());
    pane.durable_action(End).unwrap();
    phase(&pane, Failed).await;
    pane.durable_action(Reattach).unwrap();
    phase(&pane, Failed).await;
    assert_eq!(fixture.peer.backend.lock().unwrap().commands.len(), count);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_reference_checks_the_destination_key_through_a_jump() {
    use w11term::options::{Authentication, JumpHost};
    struct Jump {
        destination: u16,
    }
    impl server::Handler for Jump {
        type Error = anyhow::Error;
        async fn auth_none(&mut self, user: &str) -> Result<server::Auth> {
            Ok(if user == "gate" {
                server::Auth::Accept
            } else {
                server::Auth::reject()
            })
        }
        async fn channel_open_direct_tcpip(
            &mut self,
            channel: Channel<server::Msg>,
            host: &str,
            port: u32,
            _: &str,
            _: u32,
            reply: server::ChannelOpenHandle,
            _: &mut server::Session,
        ) -> Result<()> {
            if host != "behind-gate.invalid" || port != u32::from(self.destination) {
                reply
                    .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                    .await;
                return Ok(());
            }
            let mut socket =
                tokio::net::TcpStream::connect(("127.0.0.1", self.destination)).await?;
            reply.accept().await;
            tokio::spawn(async move {
                let _ =
                    tokio::io::copy_bidirectional(&mut channel.into_stream(), &mut socket).await;
            });
            Ok(())
        }
    }
    let mut fixture = Fixture::new().await;
    let destination = fixture.options.port;
    let jump_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let fingerprint = jump_key
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let gateway = tokio::spawn(async move {
        let config = Arc::new(server::Config {
            keys: vec![jump_key],
            ..Default::default()
        });
        let mut peers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                client=listener.accept()=>{
                    let config=config.clone();
                    peers.spawn(async move {
                        if let Ok(peer)=server::run_stream(config,client.unwrap().0,Jump{destination}).await {let _=peer.await;}
                    });
                }
                _=peers.join_next(),if !peers.is_empty()=>{}
            }
        }
    });
    fixture.options.host = "behind-gate.invalid".into();
    fixture.options.proxy_jump.push(JumpHost {
        host: "127.0.0.1".into(),
        user: "gate".into(),
        port,
        host_key: Some(fingerprint),
        authentication: Authentication::None,
    });
    let pane = fixture.open(None);
    phase(&pane, DurablePhase::Attached).await;
    let reference = pane.durable_state().unwrap().reference.unwrap();
    assert_eq!(Some(&reference.host_key), fixture.options.host_key.as_ref());
    drop(pane);
    let reopened = fixture.open(Some(reference));
    phase(&reopened, DurablePhase::Attached).await;
    reopened.durable_action(DurableAction::End).unwrap();
    phase(&reopened, DurablePhase::Ended).await;
    gateway.abort();
}
