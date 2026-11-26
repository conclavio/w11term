use super::*;
use russh::keys::{
    Algorithm, HashAlg, PrivateKey,
    signature::Signer,
    ssh_encoding::{Decode, Encode},
    ssh_key::Signature,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
};

async fn serve_agent(
    mut pipe: NamedPipeServer,
    keys: Arc<Vec<(PrivateKey, Vec<u8>)>>,
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
                // SSH_AGENT_IDENTITIES_ANSWER lists the decoy before the authorized key.
                let mut response = vec![12];
                (keys.len() as u32).encode(&mut response)?;
                for (index, (_, blob)) in keys.iter().enumerate() {
                    blob.encode(&mut response)?;
                    ["Decoy agent key", "Selected agent key"][index].encode(&mut response)?;
                }
                response
            }
            13 => {
                let blob = Vec::<u8>::decode(&mut request)?;
                let data = Vec::<u8>::decode(&mut request)?;
                let (key, _) = keys.iter().find(|(_, id)| *id == blob).unwrap();
                let signature: Signature = key.try_sign(&data)?;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a Windows desktop and serves fixture keys over an isolated agent pipe"]
async fn native_agent_key_picker_offers_only_the_saved_fingerprint() -> anyhow::Result<()> {
    let (Source::Ssh(mut ssh), remote, opens, server) = fixture_connections(false, 4).await else {
        unreachable!()
    };
    let state = tempfile::tempdir()?;
    let fresh_key = || PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let decoy = fresh_key();
    let selected = fresh_key();
    let fingerprint = |key: &PrivateKey| key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let (wanted, rejected, missing) = (
        fingerprint(&selected),
        fingerprint(&decoy),
        fingerprint(&fresh_key()),
    );
    fs::write(
        remote.path().join("authorized-key"),
        selected.public_key().to_openssh()?,
    )?;
    let trace = remote.path().join("offered-key-trace");
    fs::write(&trace, "")?;
    let keys = [decoy, selected]
        .into_iter()
        .map(|key| Ok((key.clone(), key.public_key().to_bytes()?)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let keys = Arc::new(keys);
    let pipe = format!(
        r"\\.\pipe\w11term-test-agent-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    );
    let mut listener = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe)?;
    let agent_pipe = pipe.clone();
    let agent = tokio::spawn(async move {
        loop {
            listener.connect().await.unwrap();
            let connection = listener;
            listener = ServerOptions::new().create(&agent_pipe).unwrap();
            let keys = keys.clone();
            tokio::spawn(serve_agent(connection, keys));
        }
    });

    ssh.authentication = Authentication::Agent;
    ssh.player.reconnect = false;
    let with_key = |fingerprint: &str| {
        let mut profile = ssh.clone();
        profile.authentication = Authentication::AgentKey {
            agent_fingerprint: fingerprint.parse().unwrap(),
        };
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
        serde_json::to_vec(&json!({"pipe":pipe, "wanted":wanted}))?,
    )?;
    drive_gui("ssh-agent-key-ui.ps1", state.path());
    assert_eq!(
        fs::read_to_string(&trace)?.lines().collect::<Vec<_>>(),
        [&wanted, &rejected],
    );
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    agent.abort();
    server.abort();
    Ok(())
}
