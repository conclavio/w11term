use super::*;
use russh::keys::{
    Algorithm, Certificate, HashAlg, PrivateKey,
    signature::Signer,
    ssh_encoding::{Decode, Encode},
    ssh_key::{Signature, certificate},
};
use std::sync::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
};

async fn serve_agent(
    mut pipe: NamedPipeServer,
    keys: Arc<Vec<FixtureIdentity>>,
    signed: Arc<Mutex<Vec<Vec<u8>>>>,
) -> anyhow::Result<()> {
    loop {
        let length = match pipe.read_u32().await {
            Ok(length) => length,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(length <= 4096, "unexpected agent request length");
        let mut request = vec![0; length as usize];
        pipe.read_exact(&mut request).await?;
        let mut request = request.as_slice();
        let response = match u8::decode(&mut request)? {
            11 => {
                let mut response = vec![12];
                (keys.len() as u32).encode(&mut response)?;
                for identity in keys.iter() {
                    identity.blob.encode(&mut response)?;
                    identity.comment.encode(&mut response)?;
                }
                response
            }
            13 => {
                let blob = Vec::<u8>::decode(&mut request)?;
                let data = Vec::<u8>::decode(&mut request)?;
                let identity = keys.iter().find(|identity| identity.blob == blob).unwrap();
                let signature: Signature = identity.key.try_sign(&data)?;
                signed.lock().unwrap().push(blob);
                let mut encoded = Vec::new();
                signature.encode(&mut encoded)?;
                let mut response = vec![14];
                encoded.encode(&mut response)?;
                response
            }
            other => anyhow::bail!("unexpected agent request {other}"),
        };
        pipe.write_u32(response.len() as u32).await?;
        pipe.write_all(&response).await?;
    }
}

struct FixtureIdentity {
    key: PrivateKey,
    blob: Vec<u8>,
    comment: &'static str,
}

impl FixtureIdentity {
    fn plain(key: PrivateKey, comment: &'static str) -> anyhow::Result<Self> {
        Ok(Self {
            blob: key.public_key().to_bytes()?,
            key,
            comment,
        })
    }
}

struct AgentFixture {
    pipe: String,
    signed: Arc<Mutex<Vec<Vec<u8>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl AgentFixture {
    fn start(keys: Vec<FixtureIdentity>) -> anyhow::Result<Self> {
        let keys = Arc::new(keys);
        let signed = Arc::new(Mutex::new(Vec::new()));
        let signatures = signed.clone();
        let pipe = format!(
            r"\\.\pipe\w11term-test-agent-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        );
        let mut listener = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe)?;
        let agent_pipe = pipe.clone();
        let task = tokio::spawn(async move {
            loop {
                listener.connect().await.unwrap();
                let connection = listener;
                listener = ServerOptions::new().create(&agent_pipe).unwrap();
                tokio::spawn(serve_agent(connection, keys.clone(), signatures.clone()));
            }
        });
        Ok(Self { pipe, signed, task })
    }
}

impl Drop for AgentFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn fixture_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

fn fingerprint(key: &PrivateKey) -> String {
    key.public_key().fingerprint(HashAlg::Sha256).to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a Windows desktop and serves fixture keys over an isolated agent pipe"]
async fn native_agent_key_picker_offers_only_the_saved_fingerprint() -> anyhow::Result<()> {
    let (Source::Ssh(mut ssh), remote, opens, server) = fixture_connections(false, 4).await else {
        unreachable!()
    };
    let state = tempfile::tempdir()?;
    let decoy = fixture_key();
    let selected = fixture_key();
    let (wanted, rejected, missing) = (
        fingerprint(&selected),
        fingerprint(&decoy),
        fingerprint(&fixture_key()),
    );
    fs::write(
        remote.path().join("authorized-key"),
        selected.public_key().to_openssh()?,
    )?;
    let trace = remote.path().join("offered-key-trace");
    fs::write(&trace, "")?;
    let agent = AgentFixture::start(vec![
        FixtureIdentity::plain(decoy, "Decoy agent key")?,
        FixtureIdentity::plain(selected, "Selected agent key")?,
    ])?;
    let inherited_key = fixture_key();
    let inherited_fingerprint = fingerprint(&inherited_key);
    let inherited = AgentFixture::start(vec![FixtureIdentity::plain(
        inherited_key,
        "Inherited agent key",
    )?])?;

    ssh.authentication = Authentication::Agent;
    ssh.player.reconnect = false;
    let with_key = |fingerprint: &str| {
        let mut profile = ssh.clone();
        profile.authentication = Authentication::AgentKey {
            agent_fingerprint: fingerprint.parse().unwrap(),
        };
        profile.agent_pipe = Some(agent.pipe.parse().unwrap());
        profile
    };
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{
            "Agent profile": ssh,
            "Rejected agent key": with_key(&rejected),
            "Missing agent key": with_key(&missing),
        }}))?,
    )?;
    fs::write(
        state.path().join("agent-fixture.json"),
        serde_json::to_vec(&json!({
            "pipe": inherited.pipe, "selected_pipe": agent.pipe,
            "wanted": wanted, "inherited_fingerprint": inherited_fingerprint,
        }))?,
    )?;
    drive_gui("ssh-agent-key-ui.ps1", state.path());
    assert_eq!(
        fs::read_to_string(&trace)?.lines().collect::<Vec<_>>(),
        [&wanted, &rejected],
    );
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    assert!(inherited.signed.lock().unwrap().is_empty());
    server.abort();
    Ok(())
}

fn user_certificate(key: &PrivateKey, user: &str) -> anyhow::Result<Certificate> {
    let mut builder = certificate::Builder::new_with_random_nonce(
        &mut rand::rng(),
        key.public_key().key_data().clone(),
        0,
        u64::MAX,
    )?;
    builder.cert_type(certificate::CertType::User)?;
    builder.valid_principal(user)?;
    Ok(builder.sign(&fixture_key())?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a desktop and tests agent-signed certificates on an SSH jump route"]
async fn native_agent_user_certificates_use_the_correct_signer_at_each_endpoint()
-> anyhow::Result<()> {
    use w11term::options::JumpHost;

    let (Source::Ssh(mut destination), destination_root, destination_opens, destination_server) =
        fixture_connections(false, 5).await
    else {
        unreachable!()
    };
    let (Source::Ssh(gateway), gateway_root, gateway_opens, gateway_server) =
        fixture_server(false).await
    else {
        unreachable!()
    };
    fs::write(
        gateway_root.path().join("forward-port"),
        destination.port.to_string(),
    )?;
    let state = tempfile::tempdir()?;
    let gateway_key = fixture_key();
    let destination_key = fixture_key();
    let gateway_certificate = user_certificate(&gateway_key, &gateway.user)?;
    let mut destination_certificate = user_certificate(&destination_key, &destination.user)?;
    let missing_certificate = user_certificate(&fixture_key(), &destination.user)?;
    for (root, cert) in [
        (gateway_root.path(), &gateway_certificate),
        (destination_root.path(), &destination_certificate),
    ] {
        fs::write(root.join("authorized-certificate"), cert.to_openssh()?)?;
        fs::write(root.join("offered-key-trace"), "")?;
    }
    let gateway_pin = fingerprint(&gateway_key);
    let destination_pin = fingerprint(&destination_key);
    let gateway_blob = gateway_key.public_key().to_bytes()?;
    let destination_blob = destination_certificate.to_bytes()?;
    let gateway_path = "gateway-cert.pub";
    let destination_path = "destination-cert.pub";
    let missing_path = "missing-agent-cert.pub";
    destination_certificate.set_comment("Harmless local comment, absent from the agent's blob");
    for (name, cert) in [
        (gateway_path, &gateway_certificate),
        (destination_path, &destination_certificate),
        (missing_path, &missing_certificate),
    ] {
        fs::write(state.path().join(name), cert.to_openssh()?)?;
    }
    let gateway_agent = AgentFixture::start(vec![FixtureIdentity::plain(
        gateway_key,
        "Gateway signing key",
    )?])?;
    let destination_agent = AgentFixture::start(vec![
        FixtureIdentity::plain(destination_key.clone(), "Destination plain key")?,
        FixtureIdentity {
            key: destination_key,
            blob: destination_blob.clone(),
            comment: "Destination registered certificate",
        },
    ])?;

    destination.player.reconnect = false;
    destination.agent_pipe = Some(destination_agent.pipe.parse()?);
    let with_certificate = |path: &str, pin| {
        let mut profile = destination.clone();
        profile.authentication = Authentication::agent_certificate(path, pin);
        profile
    };
    let pinned = Some(gateway_pin.parse()?);
    let mut route = destination.clone();
    route.authentication = Authentication::Agent;
    route.proxy_jump.push(JumpHost {
        host: gateway.host,
        user: gateway.user,
        port: gateway.port,
        connect_timeout_seconds: gateway.connect_timeout_seconds,
        keepalive: gateway.keepalive,
        authentication: Authentication::agent_certificate(gateway_path, pinned.clone()),
        agent_pipe: Some(gateway_agent.pipe.parse()?),
        host_key: gateway.host_key,
        host_key_alias: gateway.host_key_alias,
    });
    let mut rejected = with_certificate(gateway_path, None);
    rejected.agent_pipe = Some(gateway_agent.pipe.parse()?);
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles": {
            "Agent certificate route": route,
            "Mismatched certificate pin": with_certificate(destination_path, pinned),
            "Missing certificate key": with_certificate(missing_path, None),
            "Rejected certificate": rejected,
        }}))?,
    )?;
    fs::write(
        state.path().join("agent-fixture.json"),
        serde_json::to_vec(&json!({
            "pipe": format!(r"\\.\pipe\w11term-no-agent-{}", rand::random::<u64>()),
            "certificate": destination_path,
            "gateway_pin": gateway_pin,
            "gateway_pipe": gateway_agent.pipe,
            "destination_pipe": destination_agent.pipe,
        }))?,
    )?;
    drive_gui("ssh-agent-certificate-ui.ps1", state.path());
    assert_eq!(gateway_opens.load(Ordering::Relaxed), 0);
    assert_eq!(destination_opens.load(Ordering::Relaxed), 1);
    assert_eq!(
        *gateway_agent.signed.lock().unwrap(),
        [gateway_blob.clone(), gateway_blob]
    );
    assert_eq!(
        *destination_agent.signed.lock().unwrap(),
        [destination_blob]
    );
    assert_eq!(
        fs::read_to_string(gateway_root.path().join("offered-key-trace"))?
            .lines()
            .collect::<Vec<_>>(),
        [&gateway_pin]
    );
    assert_eq!(
        fs::read_to_string(destination_root.path().join("offered-key-trace"))?
            .lines()
            .collect::<Vec<_>>(),
        [&destination_pin, &gateway_pin]
    );
    destination_server.abort();
    gateway_server.abort();
    Ok(())
}
