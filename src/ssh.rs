use crate::options::{Authentication, SshOptions};
use crate::terminal::TransportInput;
use anyhow::{Context, Result, bail, ensure};
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use wezterm_term::TerminalSize;

#[derive(Debug)]
pub enum Event {
    Data(Vec<u8>),
    Status(String),
}

pub(crate) struct Handler(SshOptions);

impl client::Handler for Handler {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        let key = key.public_key();
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        if let Some(expected) = &self.0.host_key {
            ensure!(
                expected == &fingerprint,
                "host key mismatch: expected {expected}, received {fingerprint}"
            );
            return Ok(true);
        }
        if russh::keys::check_known_hosts(&self.0.host, self.0.port, &key)? {
            return Ok(true);
        }
        bail!(
            "unknown host key for {}:{}: {}. Verify it, then supply --host-key {} or add the host to OpenSSH known_hosts",
            self.0.host,
            self.0.port,
            fingerprint,
            fingerprint
        )
    }
}

pub async fn run(
    options: SshOptions,
    size: TerminalSize,
    mut input: mpsc::UnboundedReceiver<TransportInput>,
    output: mpsc::Sender<Event>,
) -> Result<()> {
    let config = Arc::new(client::Config {
        nodelay: true,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        ..Default::default()
    });
    let mut session = tokio::time::timeout(
        Duration::from_secs(20),
        client::connect(
            config,
            (options.host.as_str(), options.port),
            Handler(options.clone()),
        ),
    )
    .await
    .context("SSH connection timed out")??;
    match &options.authentication {
        Authentication::None => {
            let result = session.authenticate_none(&options.user).await?;
            ensure!(
                result.success(),
                "server rejected SSH none authentication: {result:?}"
            );
        }
        Authentication::Identity { identity: path } => {
            let hash = session.best_supported_rsa_hash().await?.flatten();
            let key = russh::keys::load_secret_key(path, None).with_context(|| {
                format!(
                    "loading {} (use ssh-agent for encrypted keys)",
                    path.display()
                )
            })?;
            ensure!(
                session
                    .authenticate_publickey(
                        &options.user,
                        PrivateKeyWithHashAlg::new(Arc::new(key), hash)
                    )
                    .await?
                    .success(),
                "public-key authentication rejected"
            );
        }
        Authentication::Agent => {
            let hash = session.best_supported_rsa_hash().await?.flatten();
            let pipe = std::env::var_os("SSH_AUTH_SOCK")
                .unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".into());
            let mut agent = russh::keys::agent::client::AgentClient::connect_named_pipe(pipe)
            .await
            .context(
                "cannot connect to Windows SSH agent; start the OpenSSH agent or supply --identity",
            )?;
            let mut authenticated = false;
            for identity in agent.request_identities().await? {
                if session
                    .authenticate_publickey_with(
                        &options.user,
                        identity.public_key().into_owned(),
                        hash,
                        &mut agent,
                    )
                    .await?
                    .success()
                {
                    authenticated = true;
                    break;
                }
            }
            ensure!(authenticated, "SSH agent supplied no accepted identity");
        }
    }
    let session = Arc::new(session);
    let mut channel = session.channel_open_session().await?;
    let (sftp_tx, sftp_rx) = mpsc::channel(8);
    // JoinSet aborts its children even when the pane aborts this SSH task.
    let mut children = tokio::task::JoinSet::new();
    children.spawn(crate::sftp::run(session.clone(), sftp_rx));
    channel
        .request_pty(
            true,
            "xterm-256color",
            size.cols as u32,
            size.rows as u32,
            size.pixel_width as u32,
            size.pixel_height as u32,
            &[],
        )
        .await?;
    channel.request_shell(true).await?;
    output
        .send(Event::Status(format!(
            "{}@{}:{}",
            options.user, options.host, options.port
        )))
        .await?;
    // Requests are sent in order. Failure is surfaced rather than treating a rejected PTY as ready.
    let mut requests_pending: u8 = 2;
    loop {
        tokio::select! {
            message = input.recv() => match message {
                Some(TransportInput::Sftp(job)) => {
                    if let Err(error) = sftp_tx.try_send(job) {
                        let message = if matches!(&error, mpsc::error::TrySendError::Full(_)) { "SFTP queue is full" } else { "SFTP worker is closed" };
                        let _ = error.into_inner().reply.send(Err(message.into()));
                    }
                }
                Some(TransportInput::Data(bytes)) => channel.data(bytes.as_slice()).await?,
                Some(TransportInput::Raw(bytes, reply)) => {
                    let result = channel.data(bytes.as_slice()).await;
                    let _ = reply.send(result.as_ref().map(|_| ()).map_err(|e| e.to_string()));
                    result?;
                }
                Some(TransportInput::Resize(size)) => channel.window_change(size.cols as u32, size.rows as u32, size.pixel_width as u32, size.pixel_height as u32).await?,
                None => break,
            },
            message = channel.wait() => match message {
                Some(russh::ChannelMsg::Data { data }) | Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                    // Bounded output delivery applies backpressure to the SSH channel.
                    for chunk in data.chunks(32 * 1024) { output.send(Event::Data(chunk.to_vec())).await?; }
                }
                Some(russh::ChannelMsg::Success) => requests_pending = requests_pending.saturating_sub(1),
                Some(russh::ChannelMsg::Failure) => bail!("remote server rejected a PTY or shell request ({requests_pending} outstanding)"),
                Some(russh::ChannelMsg::ExitStatus { exit_status }) => { output.send(Event::Status(format!("Remote process exited ({exit_status})"))).await?; }
                Some(russh::ChannelMsg::Close) | None => break,
                _ => {}
            }
        }
    }
    session
        .disconnect(russh::Disconnect::ByApplication, "Window closed", "en")
        .await?;
    Ok(())
}
