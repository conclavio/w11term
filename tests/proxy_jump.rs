//! Three SSH servers with separate host keys, authorized keys and user names.
//! The two private host names can only be resolved by their preceding test jump.
use anyhow::Result;
use russh::{
    Channel, ChannelId, ChannelOpenFailure, Preferred, Pty, cipher, kex,
    keys::{
        Algorithm, Certificate, EcdsaCurve, HashAlg, PrivateKey, PublicKey,
        ssh_key::{LineEnding, certificate},
    },
    server,
};
use std::{
    borrow::Cow,
    fmt::Write,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinHandle,
};
use w11term::{
    events::Events,
    options::{Authentication, JumpHost, Source, SshOptions},
    profiles::ProfileStore,
    prompts::Prompts,
    session::{Command, Session},
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
const INSPECTION_KEX: [kex::Name; 3] = [
    kex::CURVE25519,
    kex::CURVE25519_PRE_RFC_8731,
    kex::MLKEM768X25519_SHA256,
];
const INSPECTION_CIPHERS: [cipher::Name; 3] = [
    cipher::AES_128_CTR,
    cipher::AES_256_GCM,
    cipher::CHACHA20_POLY1305,
];

#[derive(Clone, Debug, Eq, PartialEq)]
enum Seen {
    Auth(usize),
    Forward(usize),
    Shell,
}

struct Peer {
    index: usize,
    identity: PublicKey,
    user_certificate: Option<Certificate>,
    next: Option<SocketAddr>,
    seen: Arc<Mutex<Vec<Seen>>>,
}
impl server::Handler for Peer {
    type Error = anyhow::Error;

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<server::Auth> {
        if self.user_certificate.is_none() && user == USERS[self.index] && key == &self.identity {
            self.seen.lock().unwrap().push(Seen::Auth(self.index));
            Ok(server::Auth::Accept)
        } else {
            Ok(server::Auth::reject())
        }
    }
    async fn auth_openssh_certificate(
        &mut self,
        user: &str,
        cert: &Certificate,
    ) -> Result<server::Auth> {
        if user == USERS[self.index] && self.user_certificate.as_ref() == Some(cert) {
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
        Self::start_with_aliases(None).await
    }
    async fn start_with_aliases(aliases: Option<[&str; 3]>) -> Result<Self> {
        Self::start_route(aliases, None, false, false).await
    }

    async fn start_route(
        aliases: Option<[&str; 3]>,
        stall: Option<usize>,
        inspect_algorithms: bool,
        user_certificates: bool,
    ) -> Result<Self> {
        let keys = tempfile::tempdir()?;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut endpoints = Vec::new();
        let mut servers = Vec::new();
        let mut known_hosts = String::new();
        let ca = PrivateKey::random(
            &mut rand::rng(),
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP384,
            },
        )?;
        let host_algorithms = [
            Algorithm::Ed25519,
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP521,
            },
        ];
        let mut next = None;
        for index in (0..HOSTS.len()).rev() {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
            let address = listener.local_addr()?;
            let host = PrivateKey::random(&mut rand::rng(), host_algorithms[index].clone())?;
            // All three peers offer both forms. Only the two private names trust
            // the CA. The public jump has a conventional known_hosts entry.
            let mut cert = certificate::Builder::new_with_random_nonce(
                &mut rand::rng(),
                host.public_key().key_data().clone(),
                0,
                u64::MAX,
            )?;
            cert.cert_type(certificate::CertType::Host)?;
            let alias = aliases.map(|names| names[index]);
            cert.valid_principal(alias.unwrap_or(HOSTS[index]))?;
            let (marker, trusted_key) = if index == 0 {
                ("", host.public_key())
            } else {
                ("@cert-authority ", ca.public_key())
            };
            let trust_name = alias
                .map(str::to_owned)
                .unwrap_or_else(|| format!("[{}]:{}", HOSTS[index], address.port()));
            writeln!(
                known_hosts,
                "{marker}{trust_name} {}",
                trusted_key.to_openssh()?
            )?;
            let identity = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
            let path = keys.path().join(format!("id-{index}"));
            std::fs::write(&path, identity.to_openssh(LineEnding::LF)?.as_bytes())?;
            let (certificate_path, user_certificate) = if user_certificates {
                let mut user_cert = certificate::Builder::new_with_random_nonce(
                    &mut rand::rng(),
                    identity.public_key().key_data().clone(),
                    0,
                    u64::MAX,
                )?;
                user_cert.cert_type(certificate::CertType::User)?;
                user_cert.valid_principal(USERS[index])?;
                let user_cert = user_cert.sign(&ca)?;
                let path = keys.path().join(format!("id-{index}-cert.pub"));
                std::fs::write(&path, user_cert.to_openssh()?)?;
                (Some(path), Some(user_cert))
            } else {
                (None, None)
            };
            endpoints.push(JumpHost {
                host: HOSTS[index].into(),
                port: address.port(),
                connect_timeout_seconds: None,
                keepalive: Default::default(),
                user: USERS[index].into(),
                authentication: Authentication::identity(path, certificate_path),
                agent_pipe: None,
                host_key: Some(host.public_key().fingerprint(HashAlg::Sha256).to_string()),
                host_key_alias: alias.map(str::to_owned),
            });
            let mut preferred = Preferred::default();
            if inspect_algorithms {
                let mut kex = vec![INSPECTION_KEX[index]];
                if index != 0 {
                    kex.push(kex::EXTENSION_OPENSSH_STRICT_KEX_AS_SERVER);
                }
                preferred.kex = Cow::Owned(kex);
                preferred.cipher = Cow::Owned(vec![INSPECTION_CIPHERS[index]]);
            }
            let config = Arc::new(server::Config {
                certificates: vec![cert.sign(&ca)?],
                keys: vec![host],
                auth_rejection_time: Duration::ZERO,
                preferred,
                ..Default::default()
            });
            let peer = Peer {
                index,
                identity: identity.public_key().clone(),
                user_certificate,
                next,
                seen: seen.clone(),
            };
            servers.push(tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                if stall == Some(index) {
                    stream.write_all(b"SSH-2.0-w11term-test\r\n").await.unwrap();
                    let mut received = Vec::new();
                    stream.read_to_end(&mut received).await.unwrap();
                    assert!(received.starts_with(b"SSH-2.0-"));
                } else if let Ok(running) = server::run_stream(config, stream, peer).await {
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
            let authentication = self
                .options
                .proxy_jump
                .get(i)
                .map_or(&self.options.authentication, |hop| &hop.authentication);
            if let Authentication::Identity {
                certificate: Some(path),
                ..
            } = authentication
            {
                writeln!(section, " CertificateFile \"{}\"", path.display())?;
            }
            let trust_alias = self
                .options
                .proxy_jump
                .get(i)
                .map(|hop| &hop.host_key_alias)
                .unwrap_or(&self.options.host_key_alias);
            if let Some(name) = trust_alias {
                writeln!(section, " HostKeyAlias {name}")?;
            }
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
    assert_imported_route(None, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_hop_trust_aliases_validate_certificates_without_changing_forwarding_addresses() {
    assert_imported_route(
        Some(["public-jump.keys", "inner-jump.keys", "shell.keys"]),
        false,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn imported_user_certificates_authenticate_each_jump_and_the_destination() {
    assert_imported_route(None, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_or_mismatched_user_certificates_do_not_fall_back_to_the_raw_key() -> Result<()> {
    for matching in [true, false] {
        // This fixture accepts the raw private key. A certificate failure must
        // stop login instead of authenticating with that raw key.
        let mut fixture = Fixture::start().await?;
        let identity = fixture.keys.path().join("id-0");
        let public = russh::keys::load_secret_key(
            fixture
                .keys
                .path()
                .join(if matching { "id-0" } else { "id-1" }),
            None,
        )?;
        let ca = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let mut cert = certificate::Builder::new_with_random_nonce(
            &mut rand::rng(),
            public.public_key().key_data().clone(),
            0,
            u64::MAX,
        )?;
        cert.cert_type(certificate::CertType::User)?;
        cert.valid_principal(USERS[0])?;
        let path = fixture.keys.path().join("test-cert.pub");
        std::fs::write(&path, cert.sign(&ca)?.to_openssh()?)?;
        fixture.options.proxy_jump[0].authentication =
            Authentication::identity(identity, Some(path));

        let (_input, _output, task) = fixture.connect();
        let result = tokio::time::timeout(Duration::from_secs(8), task).await??;
        let error = format!("{:#}", result.unwrap_err());
        let expected = if matching {
            "certificate authentication rejected"
        } else {
            "certificate does not match the private key"
        };
        assert!(error.contains(expected), "{error}");
        assert!(fixture.seen.lock().unwrap().is_empty());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inspection_reports_each_verified_peer_in_connection_order() {
    let fixture = Fixture::start_route(
        Some([
            "outer-inspect.invalid",
            "inner-inspect.invalid",
            "destination-inspect.invalid",
        ]),
        None,
        true,
        false,
    )
    .await
    .unwrap();
    let session = Session::start(Source::Ssh(fixture.options.clone()));
    let route = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (reply, receive) = tokio::sync::oneshot::channel();
            session.input.send(Command::ReadModes(reply)).unwrap();
            let inspection = receive.await.unwrap();
            if let Some(ssh) = inspection.ssh {
                assert_eq!(inspection.connection_generation, 1);
                assert_eq!(
                    inspection.ssh_connection,
                    Some(w11term::events::SshConnectionPhase::Connected)
                );
                assert!(inspection.ssh_progress.is_none());
                break ssh;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("verified SSH route was never published");
    let stages: Vec<_> = session
        .events
        .since(0)
        .events
        .into_iter()
        .filter(|event| event.kind == "ssh-step")
        .map(|event| event.data)
        .collect();
    let mut expected_stages = Vec::new();
    for (index, (host, user)) in HOSTS.into_iter().zip(USERS).enumerate() {
        let opening = if index == 0 {
            "opening-tcp"
        } else {
            "opening-jump-channel"
        };
        let (port, jump) = if let Some(hop) = fixture.options.proxy_jump.get(index) {
            (hop.port, Some(index + 1))
        } else {
            (fixture.options.port, None)
        };
        for step in [opening, "key-exchange", "authenticating"] {
            let mut stage = serde_json::json!({
                "step": step, "host": host, "user": user, "port": port
            });
            if let Some(jump) = jump {
                stage["jump"] = jump.into();
            }
            expected_stages.push(stage);
        }
    }
    for step in ["opening-session-channel", "requesting-terminal"] {
        expected_stages.push(serde_json::json!({
            "step": step, "host": HOSTS[2], "user": USERS[2], "port": fixture.options.port
        }));
    }
    assert_eq!(stages, expected_stages);

    let expected = fixture
        .options
        .proxy_jump
        .iter()
        .cloned()
        .map(SshOptions::from)
        .chain(std::iter::once(fixture.options.clone()));
    assert_eq!(route.jump_hosts.len(), 2);
    let expected_host_keys = ["ssh-ed25519", "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp521"];
    for (index, (peer, expected)) in route
        .jump_hosts
        .iter()
        .chain([&route.destination])
        .zip(expected)
        .enumerate()
    {
        assert_eq!(
            (&peer.host, &peer.user, peer.port),
            (&expected.host, &expected.user, expected.port)
        );
        assert_eq!(Some(&peer.key_fingerprint), expected.host_key.as_ref());
        assert_eq!(peer.host_key_alias, expected.host_key_alias);
        let algorithms = peer.initial_algorithms.as_ref().unwrap();
        assert_eq!(algorithms.key_exchange, INSPECTION_KEX[index].as_ref());
        assert_eq!(algorithms.host_key, expected_host_keys[index]);
        assert_eq!(algorithms.cipher, INSPECTION_CIPHERS[index].as_ref());
        assert_eq!(algorithms.strict_key_exchange, index != 0);
    }
}

async fn assert_imported_route(aliases: Option<[&str; 3]>, user_certificates: bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut fixture = Fixture::start_route(aliases, None, false, user_certificates).await?;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_key_exchange_expires_and_closes_a_direct_or_forwarded_stream() {
    for stalled in [0, 1, 2] {
        let mut fixture = Fixture::start_route(None, Some(stalled), false, false)
            .await
            .unwrap();
        let seconds = Some(1.try_into().unwrap());
        if let Some(hop) = fixture.options.proxy_jump.get_mut(stalled) {
            hop.connect_timeout_seconds = seconds;
        } else {
            fixture.options.connect_timeout_seconds = seconds;
        }
        let (_input, _output, task) = fixture.connect();
        let error = tokio::time::timeout(Duration::from_secs(8), task)
            .await
            .expect("the endpoint's timeout did not end the handshake")
            .unwrap()
            .unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(
            diagnostic.contains("SSH handshake timed out"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains(HOSTS[stalled]), "{diagnostic}");
        let seen: Vec<_> = (0..stalled)
            .flat_map(|index| [Seen::Auth(index), Seen::Forward(index)])
            .collect();
        assert_eq!(*fixture.seen.lock().unwrap(), seen);
        tokio::time::timeout(Duration::from_secs(5), &mut fixture.servers[2 - stalled])
            .await
            .expect("the timed-out handshake left its stream open")
            .unwrap();
    }
}
