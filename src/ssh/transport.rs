use super::durability::{LifecycleError, Program};
use super::sharing::{ConnectionShare, ShareSlot, Sharing, ShellChannel};
use crate::{
    events::Events,
    options::{Authentication, SshOptions},
    prompts::{Answer, Challenge, Prompts, Question},
    terminal::TransportInput,
};
use anyhow::{Context, Result, ensure};
use russh::{
    client,
    keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate},
};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use wezterm_term::TerminalSize;
use zeroize::Zeroizing;

#[derive(Debug)]
pub enum Event {
    Data(Vec<u8>),
    Status(String),
    Connected(u64),
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
pub(crate) struct Handler {
    options: SshOptions,
    prompts: Option<Prompts>,
    fingerprint: Arc<std::sync::OnceLock<String>>,
    durable_peer: Option<String>,
}
impl Handler {
    fn known(&self, key: &russh::keys::PublicKey) -> Result<bool> {
        let path = self
            .prompts
            .as_ref()
            .and_then(Prompts::known_hosts)
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::home_dir().map(|home| home.join(".ssh/known_hosts")))
            .context("cannot locate known_hosts")?;
        super::known_hosts::verify(
            &path,
            &self.options.host,
            self.options.port,
            key,
            self.options.host_key.as_deref(),
        )
    }

    fn learn(&self, key: &russh::keys::PublicKey) -> Result<()> {
        use russh::keys::known_hosts;
        if let Some(path) = self.prompts.as_ref().and_then(Prompts::known_hosts) {
            known_hosts::learn_known_hosts_path(
                &self.options.host.to_ascii_lowercase(),
                self.options.port,
                key,
                path,
            )?;
        } else {
            known_hosts::learn_known_hosts(
                &self.options.host.to_ascii_lowercase(),
                self.options.port,
                key,
            )?;
        }
        Ok(())
    }
}
impl client::Handler for Handler {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        let key = key.public_key();
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
        match self.known(&key) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(e) => {
                return Err(stop(format!(
                    "host verification failed; known keys will not be replaced: {e}"
                )));
            }
        }
        let prompts=self.prompts.as_ref().ok_or_else(||stop(format!("unknown host key for {}:{}: {fingerprint}. Verify it, then supply --host-key {fingerprint} or connect in the GUI",self.options.host,self.options.port)))?;
        let answer = prompts
            .ask(Question::HostKey {
                host: self.options.host.clone(),
                port: self.options.port,
                algorithm: key.algorithm().to_string(),
                fingerprint,
            })
            .await
            .map_err(|e| stop(e.to_string()))?;
        ensure!(
            matches!(answer, Answer::Trust(true)),
            Permanent("host key was not accepted".into())
        );
        match self.known(&key) {
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
async fn authenticate(
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
                            prompts
                                .ask(Question::Authentication {
                                    name: format!(
                                        "{}@{}:{} · {name}",
                                        options.user, options.host, options.port
                                    ),
                                    instructions,
                                    challenges: questions
                                        .into_iter()
                                        .map(|p| Challenge {
                                            label: p.prompt,
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
        let (handle, fingerprint) = if let Some(hop) = &upstream {
            let handle = hop.handle();
            let channel = tokio::time::timeout(
                Duration::from_secs(15),
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
            handshake(
                &endpoint,
                prompts,
                password,
                expected_peer,
                channel.into_stream(),
            )
            .await
        } else {
            let stream = tokio::time::timeout(
                Duration::from_secs(15),
                tokio::net::TcpStream::connect((endpoint.host.as_str(), endpoint.port)),
            )
            .await
            .with_context(|| format!("SSH TCP connection to {address} timed out"))?
            .with_context(|| format!("connecting to {address}"))?;
            stream.set_nodelay(true)?;
            handshake(&endpoint, prompts, password, expected_peer, stream).await
        }
        .with_context(|| format!("SSH handshake or authentication for {address}"))?;
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

async fn handshake(
    options: &SshOptions,
    prompts: Option<&Prompts>,
    password: &mut Option<Zeroizing<String>>,
    durable_peer: Option<&str>,
    stream: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
) -> Result<(client::Handle<Handler>, String)> {
    let config = Arc::new(client::Config {
        nodelay: true,
        keepalive_interval: Some(Duration::from_secs(5)),
        keepalive_max: 2,
        ..Default::default()
    });
    let fingerprint = Arc::new(std::sync::OnceLock::new());
    let mut session = tokio::time::timeout(
        Duration::from_secs(300),
        client::connect_stream(
            config,
            stream,
            Handler {
                options: options.clone(),
                prompts: prompts.cloned(),
                fingerprint: fingerprint.clone(),
                durable_peer: durable_peer.map(str::to_owned),
            },
        ),
    )
    .await
    .context("SSH handshake timed out")??;
    authenticate(&mut session, options, prompts, password).await?;
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
        TransportInput::Data(_) => {}
        TransportInput::Raw(_, reply) => {
            let _ = reply.send(Err(
                "SSH is reconnecting; input was not queued or sent".into()
            ));
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

async fn connect_shell(
    options: SshOptions,
    prompts: Option<Prompts>,
    passwords: &mut [Option<Zeroizing<String>>],
    source: Option<ConnectionShare>,
    published: ShareSlot,
    durable_peer: Option<String>,
) -> Result<OpenShell> {
    if let Some(link) = source.filter(|link| link.matches(&options, prompts.as_ref())) {
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
        )
        .await?,
    );
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
    let lifecycle = sharing.durable.take();
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
}

impl Driver {
    pub(super) fn discard_input(&mut self, message: TransportInput) {
        match message {
            TransportInput::Raw(_, reply) => {
                let _ = reply.send(Err(
                    "No SSH shell is attached; input was not queued or sent".into(),
                ));
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
        let mut failures = 0u32;
        let mut dropped_at: Option<tokio::time::Instant> = None;
        loop {
            let deadline = dropped_at.map(|since| since + Duration::from_secs(300));
            let connected = {
                let future = connect_shell(
                    self.options.clone(),
                    self.prompts.clone(),
                    &mut self.passwords,
                    self.sharing.offered.take(),
                    self.sharing.published.clone(),
                    program.peer_fingerprint(),
                );
                tokio::pin!(future);
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep_until(deadline.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(3600))), if deadline.is_some() => anyhow::bail!("automatic reconnection stopped after 300 seconds"),
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
                    self.generation += 1;
                    let started = tokio::time::Instant::now();
                    let result = run_channel(session, self, program).await;
                    if started.elapsed() > Duration::from_secs(30) {
                        failures = 0;
                        dropped_at = None;
                    }
                    result
                }
                Err(e) => Err(e),
            };
            let error = match result {
                Ok(()) => return Ok(()),
                Err(e) => e,
            };
            if matches!(program, Program::End(_))
                || error.downcast_ref::<Permanent>().is_some()
                || error.downcast_ref::<LifecycleError>().is_some()
                || !self.options.player.reconnect
            {
                return Err(error);
            }
            let since = *dropped_at.get_or_insert_with(tokio::time::Instant::now);
            if since.elapsed() >= Duration::from_secs(300) {
                return Err(error.context("automatic reconnection stopped after 300 seconds"));
            }
            failures += 1;
            program.reconnecting(&format!("Link lost: {error:#}"));
            let delay = Duration::from_secs((1u64 << failures.saturating_sub(1).min(4)).min(10));
            self.events.push(
                "reconnecting",
                serde_json::json!({"attempt":failures,"delay_seconds":delay.as_secs()}),
            );
            self.output
                .send(Event::Status(format!(
                    "Link lost · reconnecting in {} s (attempt {failures}) · {error:#}",
                    delay.as_secs()
                )))
                .await?;
            let wait = tokio::time::sleep(
                delay.min(Duration::from_secs(300).saturating_sub(since.elapsed())),
            );
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    _ = &mut wait => break,
                    message = self.input.recv() => match message {
                        Some(message) => self.discard_input(message),
                        None => return Ok(()),
                    }
                }
            }
        }
    }
}
async fn run_channel(mut shell: OpenShell, driver: &mut Driver, program: &Program) -> Result<()> {
    let Driver {
        options,
        size,
        input,
        output,
        events,
        generation,
        ..
    } = driver;
    let generation = *generation;
    let original_size = *size;
    let managed = !matches!(program, Program::Shell(_));
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
    let channel = shell.channel.channel();
    let (sftp_tx, sftp_rx) = mpsc::channel(8);
    let (cleanup_tx, cleanup_rx) = mpsc::unbounded_channel();
    let mut children = tokio::task::JoinSet::new();
    children.spawn(crate::sftp::run(shell.link.handle(), sftp_rx, cleanup_rx));
    if !managed {
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
        channel.set_env(false, "COLORTERM", "truecolor").await?;
        channel.request_shell(true).await?;
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
    if managed {
        shell.published.publish(&shell.link);
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
            message=input.recv()=>match message {
                Some(TransportInput::Authenticate(auth))=>options.authentication=auth,
                Some(TransportInput::Sftp(job))=>if job.reply.is_closed() && matches!(job.operation,crate::sftp::Operation::Close{..}) { let _=cleanup_tx.send(job); } else if let Err(error)=sftp_tx.try_send(job){let message=if matches!(&error,mpsc::error::TrySendError::Full(_)){"SFTP queue is full"}else{"SFTP worker is closed"};let _=error.into_inner().reply.send(Err(message.into()));},
                Some(TransportInput::Data(bytes))=>channel.data(bytes.as_slice()).await?,Some(TransportInput::Raw(bytes,reply))=>{let result=channel.data(bytes.as_slice()).await;let _=reply.send(result.as_ref().map(|_|()).map_err(|e|e.to_string()));result?;},
                Some(TransportInput::Resize(new))=>{*size=new;channel.window_change(size.cols as u32,size.rows as u32,size.pixel_width as u32,size.pixel_height as u32).await?;},None=>return Ok(()),
            },
            message=channel.wait()=>match message {
                Some(russh::ChannelMsg::Data{data}|russh::ChannelMsg::ExtendedData{data,..})=>for chunk in data.chunks(32*1024){output.send(Event::Data(chunk.to_vec())).await?;},
                Some(russh::ChannelMsg::Success) if pending>0=>{pending=pending.saturating_sub(1);if pending==0{shell.published.publish(&shell.link);program.ready();events.push("connected",serde_json::json!({"generation":generation}));output.send(Event::Connected(generation)).await?;output.send(Event::Status(format!("{}@{}:{}",options.user,options.host,options.port))).await?;}},
                Some(russh::ChannelMsg::Failure)=>return Err(stop(format!("remote server rejected a PTY or shell request ({pending} outstanding)"))),
                Some(russh::ChannelMsg::ExitStatus{exit_status})=>{output.send(Event::Status(format!("Remote process exited ({exit_status})"))).await?;return Ok(());},
                Some(russh::ChannelMsg::Close)=>return Ok(()),None=>return Err(anyhow::anyhow!("SSH transport closed unexpectedly")),_=>{}
            }
        }
    }
}
