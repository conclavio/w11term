//! Three SSH servers with separate host keys, authorized keys and user names.
//! The two private host names can only be resolved by their preceding test jump.
use anyhow::Result;
use russh::{
    Channel, ChannelId, ChannelOpenFailure, Pty,
    keys::{
        Algorithm, HashAlg, PrivateKey, PublicKey,
        ssh_key::{LineEnding, certificate},
    },
    server,
};
use std::{
    fmt::Write,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinHandle,
};
use w11term::{
    events::Events,
    options::{Authentication, JumpHost, SshOptions},
    profiles::ProfileStore,
    prompts::Prompts,
    ssh::{self, Event},
    terminal::TransportInput,
};

const HOSTS: [&str; 3] = [
    "127.0.0.1",
    "inner.private.invalid",
    "shell.private.invalid",
];
const USERS: [&str; 3] = ["outer-user", "inner-user", "shell-user"];
const PROBE: &[u8] = b"through-both-jumps";

#[derive(Clone, Debug, Eq, PartialEq)]
enum Seen {
    Auth(usize),
    Forward(usize),
    Shell,
}

struct Peer {
    index: usize,
    identity: PublicKey,
    next: Option<SocketAddr>,
    seen: Arc<Mutex<Vec<Seen>>>,
}
impl server::Handler for Peer {
    type Error = anyhow::Error;

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<server::Auth> {
        if user == USERS[self.index] && key == &self.identity {
            self.seen.lock().unwrap().push(Seen::Auth(self.index));
            Ok(server::Auth::Accept)
        } else {
            Ok(server::Auth::reject())
        }
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
        let Some(next) = self
            .next
            .filter(|address| host == HOSTS[self.index + 1] && port == u32::from(address.port()))
        else {
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        };
        let mut stream = TcpStream::connect(next).await?;
        reply.accept().await;
        self.seen.lock().unwrap().push(Seen::Forward(self.index));
        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut channel.into_stream(), &mut stream).await;
        });
        Ok(())
    }
    async fn channel_open_session(
        &mut self,
        _: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> Result<()> {
        if self.next.is_none() {
            reply.accept().await;
        } else {
            reply
                .reject(ChannelOpenFailure::AdministrativelyProhibited)
                .await;
        }
        Ok(())
    }
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut server::Session,
    ) -> Result<()> {
        session.channel_success(channel)?;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut server::Session,
    ) -> Result<()> {
        self.seen.lock().unwrap().push(Seen::Shell);
        session.channel_success(channel)?;
        Ok(())
    }
    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut server::Session,
    ) -> Result<()> {
        if self.next.is_none() {
            session.data(channel, data.to_vec())?;
        }
        Ok(())
    }
}

struct Fixture {
    options: SshOptions,
    seen: Arc<Mutex<Vec<Seen>>>,
    servers: Vec<JoinHandle<()>>,
    keys: tempfile::TempDir,
}
impl Fixture {
    async fn start() -> Result<Self> {
        let keys = tempfile::tempdir()?;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut endpoints = Vec::new();
        let mut servers = Vec::new();
        let mut known_hosts = String::new();
        let ca = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let mut next = None;
        for index in (0..HOSTS.len()).rev() {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
            let address = listener.local_addr()?;
            let host = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
            // All three peers offer both forms. Only the two private names trust
            // the CA. The public jump has a conventional known_hosts entry.
            let mut cert = certificate::Builder::new_with_random_nonce(
                &mut rand::rng(),
                host.public_key().key_data().clone(),
                0,
                u64::MAX,
            )?;
            cert.cert_type(certificate::CertType::Host)?;
            cert.valid_principal(HOSTS[index])?;
            let (marker, trusted_key) = if index == 0 {
                ("", host.public_key())
            } else {
                ("@cert-authority ", ca.public_key())
            };
            writeln!(
                known_hosts,
                "{marker}[{}]:{} {}",
                HOSTS[index],
                address.port(),
                trusted_key.to_openssh()?
            )?;
            let identity = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
            let path = keys.path().join(format!("id-{index}"));
            std::fs::write(&path, identity.to_openssh(LineEnding::LF)?.as_bytes())?;
            endpoints.push(JumpHost {
                host: HOSTS[index].into(),
                port: address.port(),
                user: USERS[index].into(),
                authentication: Authentication::Identity { identity: path },
                host_key: Some(host.public_key().fingerprint(HashAlg::Sha256).to_string()),
            });
            let config = Arc::new(server::Config {
                certificates: vec![cert.sign(&ca)?],
                keys: vec![host],
                auth_rejection_time: Duration::ZERO,
                ..Default::default()
            });
            let peer = Peer {
                index,
                identity: identity.public_key().clone(),
                next,
                seen: seen.clone(),
            };
            servers.push(tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                if let Ok(running) = server::run_stream(config, stream, peer).await {
                    let _ = running.await;
                }
            }));
            next = Some(address);
        }
        endpoints.reverse();
        std::fs::write(keys.path().join("known_hosts"), known_hosts)?;
        let mut options = SshOptions::from(endpoints.pop().unwrap());
        options.proxy_jump = endpoints;
        options.player.reconnect = false;
        Ok(Self {
            options,
            seen,
            servers,
            keys,
        })
    }
    /// Use the same importer and save path as Settings, then open the saved route.
    /// Each alias, identity and independently trusted server lives in the fixture.
    fn import_saved_route(&mut self) -> Result<()> {
        let dir = self.keys.path().join("SSH fragments");
        std::fs::create_dir(&dir)?;
        let ports = [
            self.options.proxy_jump[0].port,
            self.options.proxy_jump[1].port,
            self.options.port,
        ];
        let aliases = ["outer", "inner", "destination"];
        for (i, alias) in aliases.iter().enumerate() {
            let identity = self.keys.path().join(format!("id-{i}"));
            let mut section = format!(
                "Host {alias}\n HostName {}\n User {}\n Port {}\n IdentityFile \"{}\"\n",
                HOSTS[i],
                USERS[i],
                ports[i],
                identity.display()
            );
            if i == 2 {
                section.push_str(" ProxyJump outer,inner\n");
            }
            std::fs::write(dir.join(format!("{i}-{alias}.conf")), section)?;
        }
        let config = self.keys.path().join("config");
        std::fs::write(
            &config,
            format!("Include \"{}/*.conf\"\nHost *\n Port 1\n", dir.display()),
        )?;
        let report = w11term::ssh_config::import_file(&config)?;
        assert_eq!(report.aliases.len(), 3, "{:?}", report.warnings);
        let entry = report
            .aliases
            .iter()
            .find(|a| a.name == "destination")
            .unwrap();
        let options = entry
            .options
            .as_ref()
            .unwrap_or_else(|| panic!("{:?}", entry.warnings));
        let store = ProfileStore::new(self.keys.path().join("profiles.json"))?;
        store.add(&entry.name, options)?;
        let mut saved = store.load()?;
        assert_eq!(saved.len(), 1);
        self.options = saved.remove("destination").unwrap();
        assert!(
            self.options.host_key.is_none()
                && self
                    .options
                    .proxy_jump
                    .iter()
                    .all(|hop| hop.host_key.is_none())
        );
        self.options.player.reconnect = false;
        Ok(())
    }
    fn connect(
        &self,
    ) -> (
        mpsc::UnboundedSender<TransportInput>,
        mpsc::Receiver<Event>,
        JoinHandle<Result<()>>,
    ) {
        let (input, rx) = mpsc::unbounded_channel();
        let (tx, output) = mpsc::channel(16);
        let prompts = Prompts::with_known_hosts(self.keys.path().join("known_hosts"));
        let task = tokio::spawn(ssh::run_configured(
            self.options.clone(),
            Default::default(),
            rx,
            tx,
            Some(prompts),
            Events::default(),
        ));
        (input, output, task)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for server in &self.servers {
            server.abort();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imported_config_fragments_connect_through_both_trusted_jumps_and_close_with_the_terminal()
{
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut fixture = Fixture::start().await?;
        fixture.import_saved_route()?;
        let (input, mut output, task) = fixture.connect();
        let mut echo = Vec::new();
        while let Some(event) = output.recv().await {
            match event {
                Event::Connected(1) => input.send(TransportInput::Data(PROBE.to_vec()))?,
                Event::Data(bytes) => {
                    echo.extend(bytes);
                    if echo.len() >= PROBE.len() {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert_eq!(echo, PROBE);
        assert_eq!(
            *fixture.seen.lock().unwrap(),
            [
                Seen::Auth(0),
                Seen::Forward(0),
                Seen::Auth(1),
                Seen::Forward(1),
                Seen::Auth(2),
                Seen::Shell
            ]
        );
        drop(input);
        task.await??;
        for server in &mut fixture.servers {
            server.await?;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .expect("proxy chain did not complete or release every hop")
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_pin_at_any_endpoint_stops_before_its_authentication() {
    for (rejected, host) in HOSTS.iter().enumerate() {
        let mut fixture = Fixture::start().await.unwrap();
        let wrong = fixture.options.proxy_jump[(rejected + 1) % 2]
            .host_key
            .clone();
        if let Some(hop) = fixture.options.proxy_jump.get_mut(rejected) {
            hop.host_key = wrong;
        } else {
            fixture.options.host_key = wrong;
        }
        fixture.options.player.reconnect = true;
        let (_input, _output, task) = fixture.connect();
        let result = tokio::time::timeout(Duration::from_secs(8), task)
            .await
            .expect("mismatched pin must fail permanently")
            .unwrap();
        let diagnostic = format!("{:#}", result.unwrap_err());
        assert!(
            diagnostic.contains("host key mismatch") && diagnostic.contains(*host),
            "{diagnostic}"
        );
        let allowed: Vec<_> = (0..rejected)
            .flat_map(|i| [Seen::Auth(i), Seen::Forward(i)])
            .collect();
        assert_eq!(*fixture.seen.lock().unwrap(), allowed);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_forward_stops_before_the_next_ssh_handshake() {
    let mut fixture = Fixture::start().await.unwrap();
    fixture.options.proxy_jump[1].host = "not-allowed.private.invalid".into();
    let (_input, _output, task) = fixture.connect();
    let result = tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .unwrap()
        .unwrap();
    let diagnostic = format!("{:#}", result.unwrap_err());
    assert!(
        diagnostic.contains("refused forward")
            && diagnostic.contains("not-allowed.private.invalid"),
        "{diagnostic}"
    );
    assert_eq!(*fixture.seen.lock().unwrap(), [Seen::Auth(0)]);
}
