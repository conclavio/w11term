use super::durability::{LifecycleError, Program};
use super::game_resume;
use super::sharing::{ConnectionShare, ShareSlot, Sharing, ShellChannel};
use super::{ConnectionProgress, ConnectionStep, SESSION_SETUP_TIMEOUT};
use crate::{
    events::{AttemptGuard, Events, RetryWait, SshAttemptOutcome},
    options::{Authentication, SshOptions},
    prompts::{Answer, Challenge, Prompts, Question},
    terminal::{ScopedInput, TransportInput},
};
use anyhow::{Context, Result, ensure};
use russh::{
    client,
    keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;
use wezterm_term::TerminalSize;
use zeroize::Zeroizing;

mod handshake_deadline;

const WORKSPACE_COMMAND_INTERRUPTED: &str =
    "Workspace command interrupted; reopen the workspace to run it again";

#[derive(Debug)]
pub enum Event {
    /// Arrives before this viewer's first paint or any reconstructed terminal data.
    HerdrViewer,
    Data(Vec<u8>),
    Status(String),
    SessionListUpdated(String),
    Connected(u64),
    ConnectionProgress(Arc<ConnectionProgress>),
    /// A retry has been scheduled and supersedes the previous transport.
    Reconnecting(u32),
    /// Final SSH failure from the transport launcher.
    ConnectionFailed(String),
    ConnectionStopped,
    /// Ordered after the last bytes from this generation and before any retry.
    Disconnected,
}
#[derive(Debug)]
struct Permanent(String);
impl std::fmt::Display for Permanent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Permanent {}
fn stop(message: impl Into<String>) -> anyhow::Error {
    Permanent(message.into()).into()
}

async fn wait_for_retry_deadline(deadline: Option<Instant>, ready: Option<&OnceLock<Instant>>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
        if ready.is_none_or(|ready| ready.get().is_none()) {
            return;
        }
    }
    std::future::pending().await
}

fn ceil_seconds(duration: Duration) -> u64 {
    duration.as_secs() + u64::from(duration.subsec_nanos() != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_expired_reconnect_deadline_leaves_a_ready_shell_running() {
        let now = Instant::now();
        let ready = OnceLock::new();
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_retry_deadline(Some(now), Some(&ready)),
        )
        .await
        .unwrap();
        ready.set(now).unwrap();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                wait_for_retry_deadline(Some(now), Some(&ready))
            )
            .await
            .is_err()
        );
    }
}

#[derive(Debug)]
pub(crate) struct AttemptStopped;

impl std::fmt::Display for AttemptStopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SSH connection attempt stopped")
    }
}

impl std::error::Error for AttemptStopped {}

#[derive(Default)]
struct PendingAttempt {
    guard: Option<AttemptGuard>,
    cancelled: Option<oneshot::Receiver<()>>,
}

impl PendingAttempt {
    fn begin(&mut self, events: &Events) {
        let (guard, cancelled) = events.begin_ssh_attempt();
        self.guard = Some(guard);
        self.cancelled = Some(cancelled);
    }

    fn finish(guard: &mut Option<AttemptGuard>, outcome: SshAttemptOutcome) -> Result<()> {
        if let Some(guard) = guard.take()
            && !guard.finish(outcome)
        {
            return Err(AttemptStopped.into());
        }
        Ok(())
    }

    async fn wait_for_stop(cancelled: &mut Option<oneshot::Receiver<()>>) {
        if let Some(receiver) = cancelled {
            let result = receiver.await;
            *cancelled = None;
            if result.is_ok() {
                return;
            }
        }
        // The sender also closes when the attempt becomes an established shell.
        std::future::pending().await
    }
}
pub(crate) struct Handler {
    options: SshOptions,
    prompts: Option<Prompts>,
    known_hosts: std::path::PathBuf,
    fingerprint: Arc<std::sync::OnceLock<String>>,
    durable_peer: Option<String>,
    handshake_clock: watch::Sender<Option<Instant>>,
    pause_trust_deadline: bool,
}

// OpenSSH uses a host-key alias verbatim, even for a connection on another
// port. Port 22 tells the known_hosts reader and writer to use the bare name.
fn host_key_target(options: &SshOptions) -> (&str, u16) {
    match options.host_key_alias.as_deref() {
        Some(alias) => (alias, 22),
        None => (&options.host, options.port),
    }
}

impl Handler {
    fn known(&self, key: &PublicKeyOrCertificate) -> Result<bool> {
        let (name, port) = host_key_target(&self.options);
        super::known_hosts::verify(
            &self.known_hosts,
            name,
            port,
            key,
            self.options.host_key.as_deref(),
        )
    }

    fn learn(&self, key: &russh::keys::PublicKey) -> Result<()> {
        let (name, port) = host_key_target(&self.options);
        crate::options::validate_host_key_alias(name)?;
        if let Some(parent) = self
            .known_hosts
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        russh::keys::known_hosts::learn_known_hosts_path(
            &name.to_ascii_lowercase(),
            port,
            key,
            &self.known_hosts,
        )?;
        Ok(())
    }
}
impl client::Handler for Handler {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, presented: &PublicKeyOrCertificate) -> Result<bool> {
        let key = presented.public_key();
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        if self
            .durable_peer
            .as_ref()
            .is_some_and(|saved| saved != &fingerprint)
        {
            return Err(stop("SSH host key differs from the saved durable session"));
        }
        ensure!(
            self.fingerprint.get_or_init(|| fingerprint.clone()) == &fingerprint,
            Permanent("SSH peer changed its host key during this connection".into())
        );
        match self.known(presented) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(e) => {
                return Err(stop(format!(
                    "host verification failed; known keys will not be replaced: {e}"
                )));
            }
        }
        let prompts = self.prompts.as_ref().ok_or_else(|| {
            let alias = self.options.host_key_alias.as_deref()
                .map(|name| format!(" (host key alias {name})"))
                .unwrap_or_default();
            stop(format!(
                "unknown host key for {}:{}{alias}: {fingerprint}. Verify it, then supply --host-key {fingerprint} or connect in the GUI",
                self.options.host, self.options.port
            ))
        })?;
        let question = Question::HostKey {
            host: self.options.host.clone(),
            port: self.options.port,
            host_key_alias: self.options.host_key_alias.clone(),
            algorithm: key.algorithm().to_string(),
            fingerprint,
        };
        let remaining = if self.pause_trust_deadline {
            self.handshake_clock
                .send_replace(None)
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
        } else {
            None
        };
        ensure!(
            !remaining.is_some_and(|duration| duration.is_zero()),
            "SSH handshake timed out"
        );
        let answer = tokio::select! {
            answer = prompts.ask(question) => answer.map_err(|e| stop(e.to_string())),
            () = self.handshake_clock.closed() => Err(stop("SSH handshake ended")),
        }?;
        if let Some(remaining) = remaining {
            self.handshake_clock
                .send_replace(Some(Instant::now() + remaining));
        }
        ensure!(
            matches!(answer, Answer::Trust(true)),
            Permanent("host key was not accepted".into())
        );
        match self.known(presented) {
            Ok(true) => {}
            Ok(false) => self
                .learn(&key)
                .map_err(|e| stop(format!("saving accepted host key: {e}")))?,
            Err(e) => {
                return Err(stop(format!(
                    "host key changed while awaiting confirmation: {e}"
                )));
            }
        }
        Ok(true)
    }
}
pub(super) async fn authenticate(
    session: &mut client::Handle<Handler>,
    options: &SshOptions,
    prompts: Option<&Prompts>,
    password: &mut Option<Zeroizing<String>>,
) -> Result<()> {
    match &options.authentication {
        Authentication::None => ensure!(
            session.authenticate_none(&options.user).await?.success(),
            Permanent("server rejected SSH none authentication".into())
        ),
        Authentication::Identity { identity } => {
            let hash = session.best_supported_rsa_hash().await?.flatten();
            let key = crate::keys::load(identity)
                .map_err(|e| stop(format!("loading identity {}: {e:#}", identity.display())))?;
            ensure!(
                session
                    .authenticate_publickey(
                        &options.user,
                        PrivateKeyWithHashAlg::new(Arc::new(key), hash)
                    )
                    .await?
                    .success(),
                Permanent("public-key authentication rejected".into())
            );
        }
        Authentication::Agent => {
            let hash = session.best_supported_rsa_hash().await?.flatten();
            let pipe = std::env::var_os("SSH_AUTH_SOCK")
                .unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".into());
            let mut agent = russh::keys::agent::client::AgentClient::connect_named_pipe(pipe)
                .await
                .map_err(|e| {
                    stop(format!(
                        "cannot connect to Windows SSH agent: {e}; choose a device key or password"
                    ))
                })?;
            let mut accepted = false;
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
                    accepted = true;
                    break;
                }
            }
            ensure!(
                accepted,
                Permanent("SSH agent supplied no accepted identity".into())
            );
        }
        Authentication::Password => {
            let prompts =
                prompts.ok_or_else(|| stop("password authentication requires the GUI"))?;
            let mut accepted = false;
            for attempt in 0..3 {
                let candidate = if let Some(value) = password.take() {
                    value
                } else {
                    let answer=prompts.ask(Question::Authentication{name:format!("Sign in as {}@{}:{}",options.user,options.host,options.port),instructions:if attempt==0{"Password is kept in memory for reconnection, never saved to the profile."}else{"Password rejected. Try again."}.into(),challenges:vec![Challenge{label:"Password".into(),echo:false}]}).await.map_err(|e|stop(e.to_string()))?;
                    let Answer::Authentication(mut values) = answer else {
                        return Err(stop("authentication cancelled"));
                    };
                    ensure!(
                        values.len() == 1,
                        Permanent("invalid password answer".into())
                    );
                    values.remove(0)
                };
                match session
                    .authenticate_password(&options.user, candidate.as_str())
                    .await
                {
                    Ok(result) if result.success() => {
                        *password = Some(candidate);
                        accepted = true;
                        break;
                    }
                    Ok(_) if session.is_closed() => {
                        *password = Some(candidate);
                        anyhow::bail!("SSH transport closed during password authentication");
                    }
                    Ok(_) => {}
                    Err(error) => {
                        *password = Some(candidate);
                        return Err(error.into());
                    }
                }
            }
            ensure!(
                accepted,
                Permanent("password authentication rejected".into())
            );
        }
        Authentication::KeyboardInteractive => {
            use client::KeyboardInteractiveAuthResponse as Response;
            let prompts = prompts
                .ok_or_else(|| stop("keyboard-interactive authentication requires the GUI"))?;
            let mut result = session
                .authenticate_keyboard_interactive_start(&options.user, None)
                .await?;
            let mut rounds = 0;
            loop {
                match result {
                    Response::Success => break,
                    Response::Failure { .. } => {
                        return Err(stop("keyboard-interactive authentication rejected"));
                    }
                    Response::InfoRequest {
                        name,
                        instructions,
                        prompts: questions,
                    } => {
                        rounds += 1;
                        ensure!(
                            rounds <= 16 && questions.len() <= 16,
                            Permanent("too many keyboard-interactive challenges".into())
                        );
                        let count = questions.len();
                        let answer = if count == 0 {
                            Answer::Authentication(Vec::new())
                        } else {
                            ensure!(
                                name.len() <= 1024
                                    && instructions.len() <= 8192
                                    && questions.iter().all(|p| p.prompt.len() <= 4096),
                                Permanent("authentication challenge exceeds display limits".into())
                            );
                            let remote_name = crate::prompts::sanitize_single_line(&name, 256);
                            let remote_instructions =
                                crate::prompts::sanitize_multiline(&instructions, 2048, 24);
                            let attributed_instructions =
                                match (remote_name.is_empty(), remote_instructions.is_empty()) {
                                    (false, false) => format!(
                                        "Remote server ({remote_name}):\n{remote_instructions}"
                                    ),
                                    (false, true) => format!("Remote server: {remote_name}"),
                                    (true, false) => {
                                        format!("Remote server message:\n{remote_instructions}")
                                    }
                                    (true, true) => {
                                        "Remote server requested authentication.".into()
                                    }
                                };
                            prompts
                                .ask(Question::Authentication {
                                    // Keep the dialog title strictly local so remote server
                                    // text cannot spoof or reorder the verified endpoint.
                                    name: format!(
                                        "Sign in as {}@{}:{}",
                                        options.user, options.host, options.port
                                    ),
                                    instructions: attributed_instructions,
                                    challenges: questions
                                        .into_iter()
                                        .map(|p| Challenge {
                                            label: crate::prompts::sanitize_single_line(
                                                &p.prompt, 256,
                                            ),
                                            echo: p.echo,
                                        })
                                        .collect(),
                                })
                                .await
                                .map_err(|e| stop(e.to_string()))?
                        };
                        let Answer::Authentication(values) = answer else {
                            return Err(stop("authentication cancelled"));
                        };
                        ensure!(
                            values.len() == count,
                            Permanent("incorrect challenge answer count".into())
                        );
                        result = session
                            .authenticate_keyboard_interactive_respond(
                                values.iter().map(|v| v.to_string()).collect(),
                            )
                            .await?;
                    }
                }
            }
        }
    }
    Ok(())
}
async fn connect(
    options: &SshOptions,
    prompts: Option<&Prompts>,
    passwords: &mut [Option<Zeroizing<String>>],
    durable_peer: Option<&str>,
    output: Option<&mpsc::Sender<Event>>,
) -> Result<ConnectionShare> {
    let route = options
        .proxy_jump
        .iter()
        .cloned()
        .map(SshOptions::from)
        .chain(std::iter::once(options.clone()));
    let mut upstream: Option<ConnectionShare> = None;
    for (index, (endpoint, password)) in route.zip(passwords).enumerate() {
        let expected_peer = if index == options.proxy_jump.len() {
            durable_peer
        } else {
            None
        };
        let address = format!("{}@{}:{}", endpoint.user, endpoint.host, endpoint.port);
        let jump = (index < options.proxy_jump.len()).then_some(index + 1);
        let report = |step| report_connection_step(output, &endpoint, jump, step);
        let now = Instant::now();
        let deadline = endpoint
            .connect_timeout_seconds
            .map(|seconds| now + Duration::from_secs(seconds.get().into()));
        let open_deadline = deadline.unwrap_or_else(|| now + Duration::from_secs(15));
        let (mut handle, fingerprint) = if let Some(hop) = &upstream {
            report(ConnectionStep::OpeningJumpChannel).await?;
            let handle = hop.handle();
            let channel = tokio::time::timeout_at(
                open_deadline,
                handle.channel_open_direct_tcpip(
                    &endpoint.host,
                    u32::from(endpoint.port),
                    "127.0.0.1",
                    0,
                ),
            )
            .await
            .with_context(|| format!("forward to {address} timed out"))?
            .map_err(|error| {
                let message = format!("jump host refused forward to {address}: {error}");
                match error {
                    russh::Error::ChannelOpenFailure(
                        russh::ChannelOpenFailure::AdministrativelyProhibited
                        | russh::ChannelOpenFailure::UnknownChannelType,
                    ) => stop(message),
                    _ => anyhow::Error::from(error).context(message),
                }
            })?;
            report(ConnectionStep::KeyExchange).await?;
            key_exchange(
                &endpoint,
                prompts,
                expected_peer,
                channel.into_stream(),
                deadline,
            )
            .await
        } else {
            report(ConnectionStep::OpeningTcp).await?;
            let stream = tokio::time::timeout_at(
                open_deadline,
                tokio::net::TcpStream::connect((endpoint.host.as_str(), endpoint.port)),
            )
            .await
            .with_context(|| format!("SSH TCP connection to {address} timed out"))?
            .with_context(|| format!("connecting to {address}"))?;
            stream.set_nodelay(true)?;
            report(ConnectionStep::KeyExchange).await?;
            key_exchange(&endpoint, prompts, expected_peer, stream, deadline).await
        }
        .with_context(|| format!("SSH handshake for {address}"))?;
        report(ConnectionStep::Authenticating).await?;
        authenticate(&mut handle, &endpoint, prompts, password)
            .await
            .with_context(|| format!("SSH authentication for {address}"))?;
        upstream = Some(ConnectionShare::new(
            handle,
            endpoint,
            prompts,
            fingerprint,
            upstream,
        ));
    }
    upstream.context("missing SSH destination")
}

async fn report_connection_step(
    output: Option<&mpsc::Sender<Event>>,
    endpoint: &SshOptions,
    jump: Option<usize>,
    step: ConnectionStep,
) -> Result<()> {
    if let Some(output) = output {
        output
            .send(Event::ConnectionProgress(Arc::new(ConnectionProgress {
                step,
                host: endpoint.host.clone(),
                port: endpoint.port,
                user: endpoint.user.clone(),
                jump,
            })))
            .await?;
    }
    Ok(())
}

pub(super) async fn key_exchange(
    options: &SshOptions,
    prompts: Option<&Prompts>,
    durable_peer: Option<&str>,
    stream: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    deadline: Option<Instant>,
) -> Result<(client::Handle<Handler>, String)> {
    let known_hosts = match prompts.and_then(Prompts::known_hosts) {
        Some(path) => std::path::PathBuf::from(path),
        None => {
            let elevated = crate::control::security::is_elevated()
                .map_err(|error| stop(format!("checking process integrity: {error:#}")))?;
            if elevated {
                // Do not trust Medium-integrity %USERPROFILE%\.ssh\known_hosts when elevated.
                // `keys::data_dir()` routes to the verified elevated directory in production
                // while still respecting explicit `W11TERM_DATA_DIR` isolation in tests.
                crate::keys::data_dir()
                    .map_err(|error| {
                        stop(format!("cannot locate elevated known_hosts: {error:#}"))
                    })?
                    .join("known_hosts")
            } else {
                std::env::home_dir()
                    .map(|home| home.join(".ssh/known_hosts"))
                    .ok_or_else(|| stop("cannot locate known_hosts"))?
            }
        }
    };
    let (name, port) = host_key_target(options);
    let trust_host_ca = super::known_hosts::has_authority(&known_hosts, name, port)
        .map_err(|error| stop(format!("reading host certificate authorities: {error:#}")))?;
    let config = Arc::new(client::Config {
        preferred: super::negotiation::preferred(trust_host_ca),
        nodelay: true,
        keepalive_interval: options.keepalive.interval(),
        keepalive_max: options.keepalive.max_unanswered.get() as usize,
        ..Default::default()
    });
    let fingerprint = Arc::new(std::sync::OnceLock::new());
    let (handshake_clock, mut clock) = watch::channel(Some(
        deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(300)),
    ));
    let (stream, complete) = handshake_deadline::HandshakeStream::new(stream);
    let connecting = client::connect_stream(
        config,
        stream,
        Handler {
            options: options.clone(),
            prompts: prompts.cloned(),
            known_hosts,
            fingerprint: fingerprint.clone(),
            durable_peer: durable_peer.map(str::to_owned),
            handshake_clock,
            pause_trust_deadline: deadline.is_some(),
        },
    );
    let session = tokio::select! {
        biased;
        result = connecting => result?,
        () = handshake_deadline::expired(&mut clock) => anyhow::bail!("SSH handshake timed out"),
    };
    let _ = complete.send(());
    Ok((
        session,
        fingerprint
            .get()
            .context("SSH peer did not supply an authenticated host key")?
            .clone(),
    ))
}
fn offline(input: TransportInput, size: &mut TerminalSize, options: &mut SshOptions) {
    match input {
        TransportInput::Resize(new) => *size = new,
        TransportInput::Authenticate(auth) => options.authentication = auth,
        TransportInput::Data(_) | TransportInput::Automatic(_) => {}
        message @ TransportInput::DisconnectShell { .. } => {
            message.reject_input("The SSH shell changed before disconnect.");
        }
        message @ (TransportInput::Raw(..) | TransportInput::Scoped { .. }) => {
            message.reject_input("SSH is reconnecting; input was not queued or sent");
        }
        TransportInput::Sftp(job) => {
            let _ = job.reply.send(Err(
                "SSH is reconnecting; SFTP operation was not started".into()
            ));
        }
    }
}
pub async fn run(
    mut options: SshOptions,
    size: TerminalSize,
    input: mpsc::UnboundedReceiver<TransportInput>,
    output: mpsc::Sender<Event>,
) -> Result<()> {
    options.player.reconnect = false;
    run_configured(options, size, input, output, None, Events::default()).await
}
pub async fn run_configured(
    options: SshOptions,
    size: TerminalSize,
    input: mpsc::UnboundedReceiver<TransportInput>,
    output: mpsc::Sender<Event>,
    prompts: Option<Prompts>,
    events: Events,
) -> Result<()> {
    run_with_sharing(
        options,
        size,
        input,
        output,
        prompts,
        events,
        Sharing::default(),
    )
    .await
}

pub(super) struct OpenShell {
    pub(super) link: Arc<ConnectionShare>,
    pub(super) channel: ShellChannel,
    published: ShareSlot,
}

pub(super) async fn request_terminal(
    channel: &russh::Channel<client::Msg>,
    options: &SshOptions,
    size: TerminalSize,
) -> Result<()> {
    channel
        .request_pty(
            true,
            options.terminal_type.as_str(),
            size.cols as u32,
            size.rows as u32,
            size.pixel_width as u32,
            size.pixel_height as u32,
            &[],
        )
        .await?;
    channel.set_env(false, "COLORTERM", "truecolor").await?;
    Ok(())
}

async fn connect_shell(
    options: SshOptions,
    prompts: Option<Prompts>,
    passwords: &mut [Option<Zeroizing<String>>],
    source: Option<ConnectionShare>,
    published: ShareSlot,
    durable_peer: Option<String>,
    output: &mpsc::Sender<Event>,
) -> Result<OpenShell> {
    let opening_session = || {
        report_connection_step(
            Some(output),
            &options,
            None,
            ConnectionStep::OpeningSessionChannel,
        )
    };
    if let Some(link) = source.filter(|link| link.matches(&options, prompts.as_ref())) {
        opening_session().await?;
        match link.open_shell().await {
            Ok(channel) => {
                return Ok(OpenShell {
                    link: Arc::new(link),
                    channel,
                    published,
                });
            }
            // The new pane can still get its own login on single-channel
            // servers. No PTY, shell or application input was requested.
            Err(error) => log::debug!(
                "split channel unavailable, opening independent SSH connection: {error}"
            ),
        }
    }
    let link = Arc::new(
        connect(
            &options,
            prompts.as_ref(),
            passwords,
            durable_peer.as_deref(),
            Some(output),
        )
        .await?,
    );
    opening_session().await?;
    let channel = link.open_shell().await?;
    Ok(OpenShell {
        link,
        channel,
        published,
    })
}

pub(crate) async fn run_with_sharing(
    options: SshOptions,
    size: TerminalSize,
    input: mpsc::UnboundedReceiver<TransportInput>,
    output: mpsc::Sender<Event>,
    prompts: Option<Prompts>,
    events: Events,
    mut sharing: Sharing,
) -> Result<()> {
    options.validate_route()?;
    let passwords = vec![None; options.proxy_jump.len() + 1];
    if let Some(startup) = &sharing.startup {
        startup.check_target(&options)?;
    }
    let lifecycle = sharing.durable.take();
    let game = (options.player.rustmush && !options.durable).then(game_resume::Pane::new);
    let mut driver = Driver {
        options,
        size,
        input,
        output,
        prompts,
        events,
        sharing,
        passwords,
        generation: 0,
        command_dispatched: false,
        game,
    };
    if let Some(worker) = lifecycle {
        worker.serve(&mut driver).await
    } else {
        driver.run(&Program::Shell(None)).await
    }
}

pub(super) struct Driver {
    pub(super) options: SshOptions,
    size: TerminalSize,
    pub(super) input: mpsc::UnboundedReceiver<TransportInput>,
    pub(super) output: mpsc::Sender<Event>,
    prompts: Option<Prompts>,
    events: Events,
    sharing: Sharing,
    passwords: Vec<Option<Zeroizing<String>>>,
    generation: u64,
    command_dispatched: bool,
    game: Option<game_resume::Pane>,
}

impl Driver {
    /// Authenticate without opening a PTY or starting a shell. Keep this
    /// connection available for a subsequent attach from the same chooser.
    pub(super) async fn discover_sessions(
        &mut self,
        herdr: bool,
    ) -> Result<(Vec<crate::durable::DiscoveredSession>, String)> {
        let options = self.options.clone();
        let offered = self
            .sharing
            .offered
            .take()
            .filter(|link| link.matches(&options, self.prompts.as_ref()));
        let scan = async {
            let link = match offered {
                Some(link) => link,
                None => {
                    connect(
                        &options,
                        self.prompts.as_ref(),
                        &mut self.passwords,
                        None,
                        None,
                    )
                    .await?
                }
            };
            let entries = if herdr {
                tokio::time::timeout(
                    Duration::from_secs(25),
                    super::herdr::discover(&link, &options),
                )
                .await
                .context("Herdr browser timed out")??
            } else {
                super::durability::discover(&link, &options).await
            };
            Ok::<_, anyhow::Error>((link, entries))
        };
        tokio::pin!(scan);
        loop {
            tokio::select! {
                message = self.input.recv() => match message {
                    Some(message) => offline(message, &mut self.size, &mut self.options),
                    None => anyhow::bail!("Session discovery closed"),
                },
                result = &mut scan => {
                    let (link, entries) = result?;
                    self.sharing.offered = Some(link);
                    return Ok(entries);
                }
            }
        }
    }

    pub(super) fn discard_input(&mut self, message: TransportInput) {
        match message {
            message @ (TransportInput::Raw(..) | TransportInput::Scoped { .. }) => {
                message.reject_input("No SSH shell is attached; input was not queued or sent");
            }
            TransportInput::Sftp(job) => {
                let _ = job.reply.send(Err(
                    "No SSH shell is attached; SFTP operation was not started".into(),
                ));
            }
            other => offline(other, &mut self.size, &mut self.options),
        }
    }

    pub(super) async fn run(&mut self, program: &Program) -> Result<()> {
        program.validate_target(&self.options)?;
        let can_stop = matches!(program, Program::Shell(None))
            && !self.options.durable
            && !self.options.player.rustmush
            && self
                .sharing
                .startup
                .as_ref()
                .is_none_or(|launch| launch.command.is_empty());
        let mut attempt = PendingAttempt::default();
        if can_stop {
            attempt.begin(&self.events);
        }
        let result = self.run_pending(program, can_stop, &mut attempt).await;
        let outcome = match &result {
            Err(error) => SshAttemptOutcome::Failed(
                crate::events::display_field(&format!("{error:#}")).into(),
            ),
            Ok(()) => SshAttemptOutcome::Closed,
        };
        PendingAttempt::finish(&mut attempt.guard, outcome)?;
        result
    }

    async fn run_pending(
        &mut self,
        program: &Program,
        can_stop: bool,
        attempt: &mut PendingAttempt,
    ) -> Result<()> {
        let mut failures = 0u32;
        let mut deadline = None;
        let window = Duration::from_secs(self.options.player.reconnect_window_seconds.get().into());
        let expired = || {
            format!(
                "automatic reconnection stopped after {} seconds",
                window.as_secs()
            )
        };
        loop {
            let connected = {
                let launch_gate = self.sharing.launch_gate.take();
                let events = &self.events;
                let connect = connect_shell(
                    self.options.clone(),
                    self.prompts.clone(),
                    &mut self.passwords,
                    self.sharing.offered.take(),
                    self.sharing.published.clone(),
                    program.peer_fingerprint(),
                    &self.output,
                );
                let future = async {
                    // The slot covers login and opening the SSH channel. Active
                    // shells and automatic retries do not occupy a slot.
                    let _permit = if let Some(gate) = launch_gate {
                        let permit = gate
                            .permits
                            .acquire_owned()
                            .await
                            .context("The SSH group login queue closed")?;
                        events.set_ssh_connection_phase(gate.admitted_phase);
                        Some(permit)
                    } else {
                        None
                    };
                    connect.await
                };
                tokio::pin!(future);
                loop {
                    tokio::select! {
                        biased;
                        _ = PendingAttempt::wait_for_stop(&mut attempt.cancelled) => return Err(AttemptStopped.into()),
                        _ = wait_for_retry_deadline(deadline, None) => anyhow::bail!("{}", expired()),
                        result = &mut future => break result,
                        message = self.input.recv() => match message {
                            Some(message) => offline(message, &mut self.size, &mut self.options),
                            None => return Ok(()),
                        }
                    }
                }
            };
            let result = match connected {
                Ok(session) => {
                    self.generation = self.sharing.next_generation();
                    let ready = OnceLock::new();
                    let completed = tokio::select! {
                        biased;
                        _ = PendingAttempt::wait_for_stop(&mut attempt.cancelled) => {
                            return Err(AttemptStopped.into());
                        }
                        _ = wait_for_retry_deadline(deadline, Some(&ready)) => None,
                        result = run_channel(session, self, program, &mut attempt.guard, &ready) => Some(result),
                    };
                    let Some(result) = completed else {
                        let error = anyhow::anyhow!("{}", expired());
                        return Err(if self.command_dispatched {
                            error.context(WORKSPACE_COMMAND_INTERRUPTED)
                        } else {
                            error
                        });
                    };
                    self.output.send(Event::Disconnected).await?;
                    if let Some(game) = &mut self.game {
                        game.disconnected();
                    }
                    if ready
                        .get()
                        .is_some_and(|since| since.elapsed() > Duration::from_secs(30))
                    {
                        failures = 0;
                        deadline = None;
                    }
                    result
                }
                Err(e) => Err(e),
            };
            let error = match result {
                Ok(()) => return Ok(()),
                Err(e) => e,
            };
            if error.is::<AttemptStopped>() {
                return Err(error);
            }
            if self.command_dispatched && error.downcast_ref::<Permanent>().is_none() {
                return Err(error.context(WORKSPACE_COMMAND_INTERRUPTED));
            }
            if program.herdr_reference().is_some()
                || matches!(program, Program::End(_))
                || error.downcast_ref::<Permanent>().is_some()
                || error.downcast_ref::<LifecycleError>().is_some()
                || !self.options.player.reconnect
            {
                return Err(error);
            }
            let now = Instant::now();
            let expires_at = *deadline.get_or_insert(now + window);
            if expires_at <= now {
                return Err(error.context(expired()));
            }
            failures += 1;
            if can_stop && attempt.guard.is_none() {
                attempt.begin(&self.events);
            }
            program.reconnecting(&format!("Link lost: {error:#}"));
            let next_attempt =
                now + Duration::from_secs((1u64 << failures.saturating_sub(1).min(4)).min(10));
            let will_retry = next_attempt < expires_at;
            let wake_at = next_attempt.min(expires_at);
            let delay_seconds = ceil_seconds(wake_at - now);
            self.events.push(
                "reconnecting",
                serde_json::json!({
                    "attempt": failures,
                    "delay_seconds": delay_seconds,
                    "retry_remaining_seconds": ceil_seconds(expires_at - now),
                    "will_retry": will_retry
                }),
            );
            self.output.send(Event::Reconnecting(failures)).await?;
            let action = if will_retry {
                format!("reconnecting in {delay_seconds} s (attempt {failures})")
            } else {
                format!("retry window expires in {delay_seconds} s")
            };
            self.output
                .send(Event::Status(format!("Link lost · {action} · {error:#}")))
                .await?;
            let wait = tokio::time::sleep_until(wake_at);
            tokio::pin!(wait);
            let mut retry = attempt
                .guard
                .as_ref()
                .and_then(|attempt| attempt.offer_retry(wake_at));
            {
                let request = RetryWait::requested(retry.as_mut());
                tokio::pin!(request);
                loop {
                    tokio::select! {
                        biased;
                        _ = PendingAttempt::wait_for_stop(&mut attempt.cancelled) => return Err(AttemptStopped.into()),
                        _ = &mut request => break,
                        _ = &mut wait => break,
                        message = self.input.recv() => match message {
                            Some(message) => self.discard_input(message),
                            None => return Ok(()),
                        }
                    }
                }
            }
            drop(retry);
            if Instant::now() >= expires_at {
                return Err(error.context(expired()));
            }
            self.output
                .send(Event::Status(format!("Reconnecting · attempt {failures}")))
                .await?;
        }
    }
}
async fn run_channel(
    mut shell: OpenShell,
    driver: &mut Driver,
    program: &Program,
    attempt: &mut Option<AttemptGuard>,
    ready: &OnceLock<Instant>,
) -> Result<()> {
    let _published = shell.published.shell_lifetime(&shell.link);
    let managed = !matches!(program, Program::Shell(_));
    let step = if managed {
        ConnectionStep::PreparingRemoteProgram
    } else {
        ConnectionStep::RequestingTerminal
    };
    report_connection_step(Some(&driver.output), &driver.options, None, step).await?;
    if let Some(reference) = program.herdr_reference() {
        return run_herdr_channel(shell, driver, program, &reference).await;
    }
    let Driver {
        options,
        size,
        input,
        output,
        events,
        generation,
        game,
        sharing,
        command_dispatched,
        ..
    } = driver;
    let generation = *generation;
    let original_size = *size;
    let probe = game
        .as_mut()
        .filter(|_| !managed)
        .map(|g| g.probe(shell.link.fingerprint()));
    let restoring = probe.as_ref().is_some_and(game_resume::Probe::redeeming);
    let mut waiting_for_resume = restoring;
    let mut queued_before_unlock = 0usize;
    let mut process_exited = false;
    let mut output_eof = false;
    let mut early_data = Vec::new();
    if managed {
        let startup = program.prepare(&mut shell, options.clone(), original_size);
        tokio::pin!(startup);
        early_data = loop {
            tokio::select! {
                biased;
                message = input.recv() => match message {Some(message) => offline(message,size,options),None => return Ok(())},
                result = &mut startup => break result?,
            }
        };
        if matches!(program, Program::End(_)) {
            return Ok(());
        }
    }
    if probe.is_some() {
        shell
            .channel
            .close_game_session_on_drop(shell.link.as_ref().clone());
    }
    let channel = shell.channel.channel();
    let (sftp_tx, sftp_rx) = mpsc::channel(8);
    let (cleanup_tx, cleanup_rx) = mpsc::unbounded_channel();
    let mut children = tokio::task::JoinSet::new();
    children.spawn(crate::sftp::run(
        shell.link.handle(),
        generation,
        sftp_rx,
        cleanup_rx,
    ));
    let setup_deadline = tokio::time::Instant::now() + SESSION_SETUP_TIMEOUT;
    const SETUP_TIMEOUT: &str = "SSH PTY or shell setup timed out";
    if !managed {
        tokio::time::timeout_at(setup_deadline, async {
            request_terminal(channel, options, *size).await?;
            if let Some(probe) = &probe {
                channel
                    .set_env(false, game_resume::ENVIRONMENT, probe.marker())
                    .await?;
            }
            if let Some(startup) = &sharing.startup {
                *command_dispatched = !startup.command.is_empty();
                channel.exec(true, startup.exec_request()).await?;
            } else {
                channel.request_shell(true).await?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context(SETUP_TIMEOUT)??;
    } else if size.cols != original_size.cols
        || size.rows != original_size.rows
        || size.pixel_width != original_size.pixel_width
        || size.pixel_height != original_size.pixel_height
    {
        channel
            .window_change(
                size.cols as u32,
                size.rows as u32,
                size.pixel_width as u32,
                size.pixel_height as u32,
            )
            .await?;
    }
    let mut pending = if managed { 0 } else { 2u8 };
    let setup_timeout = tokio::time::sleep_until(setup_deadline);
    tokio::pin!(setup_timeout);
    let (auth, mut auth_rx) = tokio::sync::oneshot::channel();
    let mut probing = probe.is_some();
    let mut auth_pending = probing;
    let link = &shell.link;
    let exchange = |probe: Option<game_resume::Probe>, auth, previous| async move {
        let probe = probe.expect("enabled game sideband");
        let (outcome, follow_up) = probe.exchange(link, auth, previous).await;
        (probe.redeeming(), outcome, follow_up)
    };
    let sideband = exchange(probe, auth, None);
    tokio::pin!(sideband);
    if managed {
        shell.published.publish(&shell.link, generation);
        let _ = ready.set(Instant::now());
        program.ready();
        events.push(
            "connected",
            serde_json::json!({"generation":generation,"durable":true}),
        );
        output.send(Event::Connected(generation)).await?;
        output
            .send(Event::Status("Attached to verified durable shell".into()))
            .await?;
        for chunk in early_data.chunks(32 * 1024) {
            output.send(Event::Data(chunk.to_vec())).await?;
        }
    }
    loop {
        tokio::select! {
            _ = &mut setup_timeout, if pending > 0 => anyhow::bail!(SETUP_TIMEOUT),
            (redeeming, outcome, follow_up) = &mut sideband, if probing => {
                if waiting_for_resume {
                    queued_before_unlock = input.len();
                    waiting_for_resume = false;
                }
                // A final refusal allows one new issue on this primary channel.
                // A missing reply leaves the previous proof for the next reconnect.
                let pane = game.as_mut().expect("enabled game sideband");
                let status = pane.finish(link.fingerprint(), outcome);
                events.push("game-resume", serde_json::json!({"status": status}));
                if redeeming {
                    let message = if status == "redeemed" {
                        "RustMUSH session restored".into()
                    } else {
                        format!("RustMUSH resume {status} · using the current login")
                    };
                    output.send(Event::Status(message)).await?;
                } else if restoring && status == "issued" {
                    output.send(Event::Status("RustMUSH reconnect ticket renewed".into())).await?;
                }
                probing = follow_up.is_some();
                auth_pending = probing;
                if let Some(previous) = follow_up {
                    let (auth, receiver) = tokio::sync::oneshot::channel();
                    auth_rx = receiver;
                    sideband.set(exchange(Some(pane.probe(link.fingerprint())), auth, Some(previous)));
                }
            }
            result = &mut auth_rx, if auth_pending => {
                auth_pending = false;
                if result.is_ok() {
                    if waiting_for_resume {
                        queued_before_unlock = input.len();
                        waiting_for_resume = false;
                        output.send(Event::Status("RustMUSH resume requires game account login".into())).await?;
                    }
                    events.push("game-resume", serde_json::json!({"status": "auth_required"}));
                }
            }
            message = input.recv() => {
                if process_exited {
                    match message {
                        Some(message @ TransportInput::DisconnectShell { .. }) => {
                            message.reject_input("The SSH shell already exited.");
                        }
                        Some(message @ (TransportInput::Raw(..) | TransportInput::Scoped { .. })) => {
                            message.reject_input("Remote process exited; input was not sent");
                        }
                        Some(TransportInput::Sftp(job)) => { let _ = job.reply.send(Err("Remote process exited; SFTP operation was not started".into())); }
                        Some(_) => {}
                        None => return Ok(()),
                    }
                    continue;
                }
                // Commands already queued when the welcome/restore gate opened
                // belong to the previous input epoch, even when processed later.
                let discard_input = waiting_for_resume || queued_before_unlock > 0;
                queued_before_unlock = queued_before_unlock.saturating_sub(1);
                match message {
                    Some(TransportInput::DisconnectShell { connection, reply }) => {
                        if managed || options.player.rustmush ||
                            sharing.startup.as_ref().is_some_and(|startup| !startup.command.is_empty())
                        {
                            let _ = reply.send(Err("Disconnect requires a direct SSH shell.".into()));
                        } else if connection != generation || pending != 0 {
                            let _ = reply.send(Err("The SSH shell changed before disconnect.".into()));
                        } else {
                            // The channel guard closes this shell. A sibling may still own the link.
                            events.push("ssh-disconnect-requested", serde_json::json!({
                                "connection_generation": generation
                            }));
                            let _ = reply.send(Ok(()));
                            return Ok(());
                        }
                    }
                    Some(TransportInput::Authenticate(auth)) => options.authentication = auth,
                    Some(TransportInput::Sftp(job)) => {
                        forward_sftp(&sftp_tx,&cleanup_tx,job);
                    }
                    Some(TransportInput::Data(_) | TransportInput::Automatic(_)) if discard_input => {}
                    Some(message @ (TransportInput::Raw(..) | TransportInput::Scoped { .. })) if discard_input => {
                        message.reject_input("RustMUSH resume is pending; input was not queued or sent");
                    }
                    Some(TransportInput::Data(bytes) | TransportInput::Automatic(bytes)) => channel.data(bytes.as_slice()).await?,
                    Some(TransportInput::Scoped { connection, reply, kind, .. }) if connection != generation || pending != 0 => {
                        kind.reject(reply, kind.stale_error());
                    }
                    Some(TransportInput::Scoped { kind: ScopedInput::GroupInterrupt(link), reply, .. })
                        if !events.group_command_running(link) => {
                        let _ = reply.send(Err(crate::terminal::GROUP_COMMAND_ENDED.into()));
                    }
                    Some(TransportInput::Scoped { kind: ScopedInput::GroupCommand(receipt), bytes, reply, .. }) => {
                        // A failed write can follow a partial send. Dropping the
                        // reply leaves group delivery unconfirmed.
                        channel.data(bytes.as_slice()).await?;
                        receipt.acknowledge();
                        let _ = reply.send(Ok(()));
                    }
                    Some(TransportInput::Raw(bytes, reply) | TransportInput::Scoped { bytes, reply, .. }) => {
                        let result = channel.data(bytes.as_slice()).await;
                        let _ = reply.send(result.as_ref().map(|_| ()).map_err(|e| e.to_string()));
                        result?;
                    }
                    Some(TransportInput::Resize(new)) => {
                        *size = new;
                        channel.window_change(size.cols as u32, size.rows as u32,
                            size.pixel_width as u32, size.pixel_height as u32).await?;
                    }
                    None => return Ok(()),
                }
            }
            message=channel.wait()=>match message {
                Some(russh::ChannelMsg::Data{data}|russh::ChannelMsg::ExtendedData{data,..})=>for chunk in data.chunks(32*1024){output.send(Event::Data(chunk.to_vec())).await?;},
                Some(russh::ChannelMsg::Success) if pending > 0 => {
                    pending -= 1;
                    if pending == 0 {
                        PendingAttempt::finish(attempt, SshAttemptOutcome::Connected)?;
                        shell.published.publish(&shell.link, generation);
                        let _ = ready.set(Instant::now());
                        program.ready();
                        events.push("connected", serde_json::json!({"generation":generation}));
                        output.send(Event::Connected(generation)).await?;
                        output.send(Event::Status(format!("{}@{}:{}", options.user, options.host, options.port))).await?;
                    }
                },
                Some(russh::ChannelMsg::Failure)=>return Err(stop(format!("remote server rejected a PTY or {} request ({pending} outstanding)", if sharing.startup.is_some() {"workspace exec"} else {"shell"}))),
                // Exit status is advisory until EOF/close; the peer can still
                // deliver final terminal bytes after reporting it.
                Some(russh::ChannelMsg::ExitStatus{exit_status})=>{process_exited=true;output.send(Event::Status(format!("Remote process exited ({exit_status})"))).await?;if output_eof{return Ok(());}},
                Some(russh::ChannelMsg::Close)=>return Ok(()),
                Some(russh::ChannelMsg::Eof)=>{if process_exited{return Ok(())} output_eof=true;},
                None if process_exited => return Ok(()),
                None=>return Err(anyhow::anyhow!("SSH transport closed unexpectedly")),_=>{}
            }
        }
    }
}

async fn run_herdr_channel(
    shell: OpenShell,
    driver: &mut Driver,
    program: &Program,
    reference: &crate::durable::DurableReference,
) -> Result<()> {
    let original = driver.size;
    let cols = u16::try_from(original.cols)?;
    let rows = u16::try_from(original.rows)?;
    let link = shell.link;
    let connecting = tokio::time::timeout(
        Duration::from_secs(20),
        super::herdr::attach(shell.channel, &link, reference, cols, rows),
    );
    tokio::pin!(connecting);
    let mut viewer = loop {
        tokio::select! {
            message=driver.input.recv()=>match message {Some(message)=>driver.discard_input(message),None=>return Ok(())},
            result=&mut connecting=>break result.context("Timed out attaching Herdr terminal")??,
        }
    };
    // Commands queued up to this verification belong to the old input epoch.
    for _ in 0..driver.input.len() {
        if let Ok(message) = driver.input.try_recv() {
            driver.discard_input(message);
        }
    }
    if driver.size.cols != original.cols || driver.size.rows != original.rows {
        viewer
            .resize(
                u16::try_from(driver.size.cols)?,
                u16::try_from(driver.size.rows)?,
            )
            .await?;
    }
    let generation = driver.generation;
    shell.published.publish(&link, generation);
    driver.output.send(Event::HerdrViewer).await?;
    driver.output.send(Event::Connected(generation)).await?;
    driver.events.push(
        "connected",
        serde_json::json!({"generation":generation,"durable":true,"backend":"herdr"}),
    );
    program.ready();
    let (sftp_tx, sftp_rx) = mpsc::channel(8);
    let (cleanup_tx, cleanup_rx) = mpsc::unbounded_channel();
    let mut jobs = tokio::task::JoinSet::new();
    jobs.spawn(crate::sftp::run(
        link.handle(),
        generation,
        sftp_rx,
        cleanup_rx,
    ));
    loop {
        tokio::select! {
            message=driver.input.recv()=>match message {
                Some(TransportInput::Data(data))=>viewer.input(&data).await?,
                Some(TransportInput::Automatic(data))=>viewer.automatic_input(&data).await?,
                Some(TransportInput::Scoped {connection,reply,kind,..}) if connection != generation => {
                    kind.reject(reply, kind.stale_error());
                }
                Some(TransportInput::Scoped {kind:ScopedInput::GroupInterrupt(_),reply,..}) => {
                    let _=reply.send(Err("Group interrupts require a direct SSH shell.".into()));
                }
                Some(TransportInput::Raw(data,reply) | TransportInput::Scoped {bytes:data,reply,..})=>{
                    let result=viewer.input(&data).await;
                    let _=reply.send(result.as_ref().map(|_|()).map_err(|e|e.to_string()));
                    result?;
                }
                Some(TransportInput::Resize(size))=>{driver.size=size; viewer.resize(u16::try_from(size.cols)?,u16::try_from(size.rows)?).await?;}
                Some(TransportInput::Authenticate(auth))=>driver.options.authentication=auth,
                Some(TransportInput::Sftp(job))=>forward_sftp(&sftp_tx,&cleanup_tx,job),
                Some(message @ TransportInput::DisconnectShell { .. }) => {
                    message.reject_input("Disconnect requires a direct SSH shell.");
                }
                None=>{viewer.close().await?;return Ok(());}
            },
            data=viewer.next_data()=>match data? {
                Some(bytes)=>{let size=bytes.len(); if size>0 { driver.output.send(Event::Data(bytes)).await?; viewer.credit(size).await?; }}
                None=>return Ok(()),
            }
        }
    }
}

fn forward_sftp(
    sftp_tx: &mpsc::Sender<crate::sftp::Job>,
    cleanup_tx: &mpsc::UnboundedSender<crate::sftp::Job>,
    job: crate::sftp::Job,
) {
    if job.reply.is_closed() && matches!(job.operation, crate::sftp::Operation::Close { .. }) {
        let _ = cleanup_tx.send(job);
    } else if let Err(error) = sftp_tx.try_send(job) {
        let message = if matches!(&error, mpsc::error::TrySendError::Full(_)) {
            "SFTP queue is full"
        } else {
            "SFTP worker is closed"
        };
        let _ = error.into_inner().reply.send(Err(message.into()));
    }
}
