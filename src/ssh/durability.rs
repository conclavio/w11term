//! Opt-in remote PTY lifecycle. A shell channel stays private until its
//! backend generation is verified on the same authenticated SSH transport.
use super::{
    ConnectionShare,
    transport::{Driver, Event, OpenShell},
};
use crate::{
    durable::{Backend, DiscoveredSession, DurableReference, tmux},
    options::SshOptions,
};
use anyhow::{Context, Result, bail, ensure};
use russh::ChannelMsg;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc;
use wezterm_term::TerminalSize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurablePhase {
    Discovering,
    Choosing,
    Connecting,
    Attached,
    Detached,
    Reconnecting,
    Missing,
    Unavailable,
    Ended,
    Direct,
    Failed,
}
impl DurablePhase {
    pub fn is_browser(self) -> bool {
        matches!(self, Self::Discovering | Self::Choosing)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DurableAction {
    Refresh,
    Choose(DurableReference),
    Detach,
    Reattach,
    End,
    StartNew,
    UseDirectShell,
}

#[derive(Clone, Debug)]
pub struct DurableState {
    pub herdr_browser: bool,
    pub phase: DurablePhase,
    pub reference: Option<DurableReference>,
    pub detail: String,
    pub candidates: Arc<[DiscoveredSession]>,
}
impl DurableState {
    pub fn permits(&self, action: DurableAction) -> bool {
        use DurableAction::*;
        use DurablePhase::*;
        match action {
            Refresh => {
                self.phase == Choosing
                    || (matches!(self.phase, Detached | Missing | Unavailable | Failed)
                        && self
                            .reference
                            .as_ref()
                            .is_some_and(|r| r.backend != Backend::Zmx))
            }
            Choose(reference) => {
                self.phase == Choosing && self.candidates.iter().any(|s| s.reference == reference)
            }
            Detach => matches!(self.phase, Attached | Reconnecting) && self.reference.is_some(),
            Reattach => {
                matches!(self.phase, Detached | Missing | Failed | Unavailable)
                    && self.reference.is_some()
            }
            End => {
                matches!(self.phase, Attached | Detached)
                    && self
                        .reference
                        .as_ref()
                        .is_some_and(|r| r.backend == Backend::Zmx)
            }
            UseDirectShell | StartNew if self.herdr_browser => false,
            StartNew
                if self
                    .reference
                    .as_ref()
                    .is_some_and(|r| r.backend != Backend::Zmx) =>
            {
                false
            }
            StartNew | UseDirectShell => {
                matches!(self.phase, Missing | Unavailable | Ended | Failed)
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct Model(Arc<Mutex<DurableState>>);
impl Model {
    fn set(&self, phase: DurablePhase, message: impl Into<String>) {
        let mut state = self.0.lock().unwrap();
        state.phase = phase;
        state.detail = message.into();
    }
    fn reference(&self) -> Option<DurableReference> {
        self.0.lock().unwrap().reference.clone()
    }
}

pub(crate) struct DurableControl {
    model: Model,
    requests: mpsc::Sender<DurableAction>,
}
pub(crate) struct Worker {
    model: Model,
    requests: mpsc::Receiver<DurableAction>,
}
pub(crate) enum Start {
    Attach(Option<DurableReference>),
    Discover,
    DiscoverHerdr,
}
impl DurableControl {
    pub(crate) fn new(start: Start) -> (Self, Worker) {
        let herdr_browser = matches!(&start, Start::DiscoverHerdr)
            || matches!(&start, Start::Attach(Some(r)) if r.backend == Backend::Herdr);
        let (phase, reference, detail) = match start {
            Start::Discover | Start::DiscoverHerdr => (
                DurablePhase::Discovering,
                None,
                "Looking for remote sessions…",
            ),
            Start::Attach(reference) => (
                DurablePhase::Connecting,
                reference,
                "Connecting to durable shell",
            ),
        };
        let model = Model(Arc::new(Mutex::new(DurableState {
            herdr_browser,
            phase,
            reference,
            detail: detail.into(),
            candidates: Arc::new([]),
        })));
        let (requests, receiver) = mpsc::channel(1);
        (
            Self {
                model: model.clone(),
                requests,
            },
            Worker {
                model,
                requests: receiver,
            },
        )
    }
    pub(crate) fn state(&self) -> DurableState {
        self.model.0.lock().unwrap().clone()
    }
    pub(crate) fn action(&self, action: DurableAction) -> Result<()> {
        let mut state = self.model.0.lock().unwrap();
        ensure!(
            state.permits(action.clone()),
            "This action is unavailable for the current durable shell state"
        );
        let refreshing = action == DurableAction::Refresh;
        self.requests
            .try_send(action)
            .context("A durable shell action is already pending or the pane has closed")?;
        if refreshing {
            state.candidates = Arc::new([]);
            state.reference = None;
        }
        state.phase = if refreshing {
            DurablePhase::Discovering
        } else {
            DurablePhase::Connecting
        };
        state.detail = "Applying durable shell action".into();
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct LifecycleError(DurablePhase, String);
impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}
impl std::error::Error for LifecycleError {}
fn failure(phase: DurablePhase, message: impl Into<String>) -> anyhow::Error {
    LifecycleError(phase, message.into()).into()
}

pub(super) enum Program {
    Discover,
    Shell(Option<Model>),
    Attach(Model),
    End(Model),
}
impl Program {
    pub(super) fn herdr_reference(&self) -> Option<DurableReference> {
        match self {
            Self::Attach(model) => model.reference().filter(|r| r.backend == Backend::Herdr),
            _ => None,
        }
    }
    pub(super) fn peer_fingerprint(&self) -> Option<String> {
        match self {
            Self::Attach(model) | Self::End(model) => model.reference().map(|r| r.host_key),
            Self::Shell(_) | Self::Discover => None,
        }
    }
    pub(super) fn validate_target(&self, options: &SshOptions) -> Result<()> {
        if let Self::Attach(model) | Self::End(model) = self
            && let Some(reference) = model.reference()
        {
            reference
                .validate()
                .map_err(|e| failure(DurablePhase::Failed, e.to_string()))?;
            ensure!(
                reference.matches_endpoint(options),
                failure(
                    DurablePhase::Failed,
                    "Saved durable reference belongs to a different SSH host, key or account"
                )
            );
        }
        Ok(())
    }
    pub(super) fn reconnecting(&self, message: &str) {
        if let Self::Attach(model) = self {
            model.set(DurablePhase::Reconnecting, message);
        }
    }
    pub(super) fn ready(&self) {
        match self {
            Self::Attach(model) => model.set(
                DurablePhase::Attached,
                "Attached to the verified remote PTY",
            ),
            Self::Shell(Some(model)) => {
                model.set(DurablePhase::Direct, "Connected to a new direct SSH shell")
            }
            _ => {}
        }
    }
    pub(super) async fn prepare(
        &self,
        shell: &mut OpenShell,
        options: SshOptions,
        size: TerminalSize,
    ) -> Result<Vec<u8>> {
        use DurablePhase::*;
        let model = match self {
            Self::Attach(model) | Self::End(model) => model,
            Self::Shell(_) | Self::Discover => unreachable!(),
        };
        let saved = model.reference();
        if let Some(reference) = &saved {
            reference
                .validate()
                .map_err(|e| failure(Failed, e.to_string()))?;
            if reference.host_key != shell.link.fingerprint() {
                return Err(failure(
                    Failed,
                    "Saved durable reference belongs to a different SSH host, key or account",
                ));
            }
            if reference.backend == Backend::Tmux {
                ensure!(
                    matches!(self, Self::Attach(_)),
                    "Existing tmux sessions can only be attached"
                );
                let target = tmux::Target::parse(&reference.name, &reference.generation)
                    .context("Invalid tmux session reference")?;
                return attach_tmux(shell, &options, size, target).await;
            }
        }
        check_version(&shell.link).await?;
        let create = saved.is_none();
        let reference =
            saved.unwrap_or_else(|| DurableReference::new(&options, shell.link.fingerprint()));
        reference.validate()?;
        if !create {
            verify(&shell.link, &reference).await?;
        }
        if matches!(self, Self::End(_)) {
            ensure!(
                !create,
                failure(Failed, "There is no saved remote PTY to end")
            );
            let stopped = command(&shell.link, &format!("zmx kill {}", reference.name)).await?;
            ensure!(
                stopped.status == 0,
                failure(
                    Failed,
                    "zmx could not confirm the session ended; the saved reference was kept"
                )
            );
            return Ok(Vec::new());
        }
        let attach = if create {
            // Remember an uncertain create before dispatch. A retry checks
            // this identity and never creates a second shell automatically.
            model.0.lock().unwrap().reference = Some(reference.clone());
            format!(
                "zmx attach --labels w11term_generation={} {}",
                reference.generation, reference.name
            )
        } else {
            format!("zmx attach {} /bin/false", reference.name)
        };
        let early = exec_pty(shell, &options, size, attach).await?;
        // Exec success precedes starting the remote process. A new daemon may
        // briefly be absent, but a mismatched nonempty label is definitive.
        for attempt in 0..20 {
            let label = read_generation(&shell.link, &reference).await?;
            if label.status == 0 && label.stdout == reference.generation.as_bytes() {
                return Ok(early);
            }
            if !create || label.status == 0 || attempt == 19 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
        Err(failure(
            Missing,
            "Remote PTY generation could not be verified after attachment",
        ))
    }
}

async fn exec_pty(
    shell: &mut OpenShell,
    options: &SshOptions,
    size: TerminalSize,
    command: String,
) -> Result<Vec<u8>> {
    use DurablePhase::*;
    let channel = shell.channel.channel();
    super::transport::request_terminal(channel, options, size).await?;
    channel.exec(true, command).await?;
    let mut early = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut pending = 2;
        while pending > 0 {
            match channel.wait().await {
                Some(ChannelMsg::Success) => pending -= 1,
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    ensure!(
                        early.len() + data.len() <= 1024 * 1024,
                        failure(
                            Failed,
                            "Too much unverified terminal output before attachment"
                        )
                    );
                    early.extend_from_slice(&data);
                }
                Some(ChannelMsg::Failure | ChannelMsg::ExitStatus { .. } | ChannelMsg::Close) => {
                    return Err(failure(
                        Unavailable,
                        "The remote server refused the terminal or exec request",
                    ));
                }
                None => bail!("SSH channel closed during durable attachment"),
                _ => {}
            }
        }
        Ok(())
    })
    .await
    .context("SSH durable startup timed out")??;
    Ok(early)
}

async fn attach_tmux(
    shell: &mut OpenShell,
    options: &SshOptions,
    size: TerminalSize,
    target: tmux::Target,
) -> Result<Vec<u8>> {
    use DurablePhase::*;
    let mut early = exec_pty(shell, options, size, target.attach()).await?;
    let channel = shell.channel.channel();
    let link = &shell.link;
    tokio::time::timeout(Duration::from_secs(10), async {
        let client = loop {
            if let Some(end) = early.iter().position(|b| *b == b'\n') {
                let header = std::str::from_utf8(&early[..end])?.trim_end_matches('\r');
                let pid = header.parse::<u32>().ok().filter(|pid| *pid != 0 && pid.to_string() == header)
                    .context("Invalid tmux client PID on the SSH terminal")?;
                early.drain(..=end);
                break pid;
            }
            ensure!(early.len() <= 32, "Missing tmux client PID on the SSH terminal");
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => early.extend_from_slice(&data),
                Some(ChannelMsg::ExtendedData { .. } | ChannelMsg::ExitStatus { .. } | ChannelMsg::Close) | None =>
                    return Err(failure(Unavailable, "Unable to launch tmux on this SSH account")),
                _ => {}
            }
        };
        // An SSH exec acknowledgement only means the login shell accepted the
        // command. Withhold input and rendering until this particular tmux
        // client appears on the selected generation, even on a busy server.
        let check = async {
            loop {
                let clients = bounded_command(link, &target.clients(), 1024 * 1024).await?;
                if clients.status != 0 {
                    return Err(failure(Missing, "The selected tmux session is no longer available"));
                }
                if target.attached(client, &clients.stdout) { return Ok::<_, anyhow::Error>(()); }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        tokio::pin!(check);
        loop {
            tokio::select! {
                result = &mut check => { result?; return Ok(early); }
                message = channel.wait() => match message {
                    Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                        ensure!(early.len() + data.len() <= 1024 * 1024, "Too much output before tmux attachment");
                        early.extend_from_slice(&data);
                    }
                    Some(ChannelMsg::ExitStatus { .. } | ChannelMsg::Close) | None =>
                        return Err(failure(Missing, "tmux could not attach the selected session. Check that it still exists.")),
                    _ => {}
                }
            }
        }
    }).await.map_err(|_| failure(Unavailable, "Timed out verifying the tmux attachment"))?
}

pub(super) struct CommandResult {
    pub(super) status: u32,
    pub(super) stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn compatible_version(output: &[u8]) -> bool {
    let Some(line) = output
        .split(|b| *b == b'\n')
        .next()
        .and_then(|v| std::str::from_utf8(v).ok())
    else {
        return false;
    };
    let mut fields = line.split_ascii_whitespace();
    if fields.next() != Some("zmx") {
        return false;
    }
    let Some(version) = fields.next() else {
        return false;
    };
    if fields.next().is_some() {
        return false;
    }
    // zmx's first version line is "zmx<TAB><TAB>MAJOR.MINOR.PATCH".
    // Packaging metadata does not change compatibility. A prerelease of the
    // minimum version predates that release; later 1.x builds still undergo
    // the same live generation checks as the version used in the Linux trial.
    semver::Version::parse(version).is_ok_and(|version| {
        version.major == 1
            && version
                .cmp_precedence(&semver::Version::new(1, 0, 1))
                .is_ge()
    })
}

async fn command(link: &ConnectionShare, text: &str) -> Result<CommandResult> {
    bounded_command(link, text, 8192).await
}
pub(super) async fn bounded_command(
    link: &ConnectionShare,
    text: &str,
    limit: usize,
) -> Result<CommandResult> {
    tokio::time::timeout(Duration::from_secs(6), async {
        let mut guard = link.open_shell().await.map_err(|e| {
            if link.handle().is_closed() {
                e
            } else {
                failure(
                    DurablePhase::Unavailable,
                    format!("Durable sessions require a separate SSH command channel: {e}"),
                )
            }
        })?;
        let channel = guard.channel();
        channel.exec(true, text).await?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut size = 0usize;
        let mut status = None;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    size += data.len();
                    if size <= limit {
                        stdout.extend_from_slice(&data);
                    }
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    size += data.len();
                    if size <= limit {
                        stderr.extend_from_slice(&data);
                    }
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => status = Some(exit_status),
                Some(ChannelMsg::Close) | None => break,
                Some(ChannelMsg::Failure) => {
                    return Err(failure(
                        DurablePhase::Unavailable,
                        "The SSH server refused the session command",
                    ));
                }
                _ => {}
            }
            ensure!(
                size <= limit,
                failure(
                    DurablePhase::Unavailable,
                    "Unexpected output from remote session command"
                )
            );
        }
        let status =
            status.context("SSH command channel closed before reporting an exit status")?;
        Ok(CommandResult {
            status,
            stdout,
            stderr,
        })
    })
    .await
    .context("Timed out waiting for the remote session command")?
}
async fn check_version(link: &ConnectionShare) -> Result<()> {
    let version = command(link, "zmx version").await?;
    ensure!(
        version.status == 0,
        failure(
            DurablePhase::Unavailable,
            "Unable to run zmx on this SSH account. Start a direct shell or install zmx on the remote host."
        )
    );
    ensure!(
        compatible_version(&version.stdout),
        failure(
            DurablePhase::Unavailable,
            "Unsupported zmx version. Durable shells require zmx 1.x, starting with 1.0.1."
        )
    );
    Ok(())
}
async fn discover_zmx(
    link: &ConnectionShare,
    options: &SshOptions,
) -> Result<Vec<DiscoveredSession>> {
    check_version(link).await?;
    let listing = bounded_command(link, "zmx list", 1024 * 1024).await?;
    ensure!(
        listing.status == 0,
        "zmx could not list sessions on this account"
    );
    crate::durable::parse_listing(&listing.stdout, options, link.fingerprint())
}

async fn discover_tmux(
    link: &ConnectionShare,
    options: &SshOptions,
) -> Result<Vec<DiscoveredSession>> {
    let listing = bounded_command(link, tmux::LIST, 1024 * 1024).await?;
    if listing.status != 0 {
        let stderr = String::from_utf8_lossy(&listing.stderr);
        if listing.status == 1
            && (stderr.starts_with("no server running on ")
                || stderr.trim() == "no sessions"
                || (stderr.starts_with("error connecting to ")
                    && stderr.trim_end().ends_with("(No such file or directory)")))
        {
            return Ok(Vec::new());
        }
        ensure!(listing.status != 127, "tmux is unavailable on this account");
        bail!(
            "tmux could not list sessions on the default socket (exit {})",
            listing.status
        );
    }
    Ok(tmux::parse_listing(&listing.stdout)
        .map_err(anyhow::Error::msg)?
        .into_iter()
        .map(|entry| DiscoveredSession {
            reference: DurableReference {
                backend: Backend::Tmux,
                host: options.host.clone(),
                port: options.port,
                user: options.user.clone(),
                host_key: link.fingerprint().into(),
                name: entry.target.name(),
                generation: entry.target.generation(),
            },
            display_name: entry.name,
            windows: Some(entry.windows),
            clients: entry.clients,
            directory: String::new(),
            command: String::new(),
        })
        .collect())
}

pub(super) async fn discover(
    link: &ConnectionShare,
    options: &SshOptions,
) -> (Vec<DiscoveredSession>, String) {
    let mut entries = Vec::new();
    let mut notices = Vec::new();
    // Probe sequentially so an SSH account that allows only one command
    // channel at a time can still browse either installed backend.
    for result in [
        discover_zmx(link, options).await,
        discover_tmux(link, options).await,
    ] {
        match result {
            Ok(sessions) => entries.extend(sessions),
            Err(err) => notices.push(format!("{err:#}")),
        }
    }
    let detail = match entries.len() {
        0 if !notices.is_empty() => format!(
            "No compatible remote sessions found. {}",
            notices.join(". ")
        ),
        0 => "No compatible remote sessions found".into(),
        1 => "1 remote session".into(),
        count => format!("{count} remote sessions"),
    };
    (entries, detail)
}
async fn read_generation(
    link: &ConnectionShare,
    reference: &DurableReference,
) -> Result<CommandResult> {
    command(
        link,
        &format!("zmx get {} w11term_generation", reference.name),
    )
    .await
}
async fn verify(link: &ConnectionShare, reference: &DurableReference) -> Result<()> {
    let label = read_generation(link, reference).await?;
    ensure!(
        label.status == 0 && label.stdout == reference.generation.as_bytes(),
        failure(
            DurablePhase::Missing,
            "The saved remote PTY is missing or has a different generation"
        )
    );
    Ok(())
}

impl Worker {
    pub(super) async fn serve(mut self, driver: &mut Driver) -> Result<()> {
        use DurablePhase::*;
        let mut next = Some(if self.model.0.lock().unwrap().phase == Discovering {
            Program::Discover
        } else {
            Program::Attach(self.model.clone())
        });
        loop {
            if let Some(program) = next.take() {
                let execute = async {
                    if matches!(program, Program::Discover) {
                        let herdr = self.model.0.lock().unwrap().herdr_browser;
                        let (entries, detail) = driver.discover_sessions(herdr).await?;
                        let mut state = self.model.0.lock().unwrap();
                        state.detail = detail;
                        state.candidates = entries.into();
                        state.phase = Choosing;
                        Ok(())
                    } else {
                        driver.run(&program).await
                    }
                };
                let result = tokio::select! {
                    biased;
                    action=self.requests.recv()=>{
                        let Some(action)=action else {return Ok(())};
                        next=self.transition(action);
                        continue;
                    },
                    result=execute=>result
                };
                match result {
                    Ok(()) if matches!(program, Program::Discover) => {}
                    Err(error) if matches!(program, Program::Discover) => {
                        self.model.set(Choosing, format!("{error:#}"))
                    }
                    Ok(()) if matches!(program, Program::End(_)) => {
                        self.model.0.lock().unwrap().reference = None;
                        self.model.set(Ended, "Remote durable shell ended");
                    }
                    Ok(()) if matches!(program, Program::Shell(_)) => {
                        self.model.set(Ended, "Direct SSH shell exited")
                    }
                    Ok(()) => self.model.set(
                        Detached,
                        "Remote attachment closed; reattach to check the saved shell",
                    ),
                    Err(error) => {
                        let phase = error
                            .downcast_ref::<LifecycleError>()
                            .map_or(Failed, |e| e.0);
                        self.model.set(phase, format!("{error:#}"));
                    }
                }
            }
            let update = {
                let state = self.model.0.lock().unwrap();
                if state.phase == Choosing {
                    Event::SessionListUpdated(state.detail.clone())
                } else {
                    Event::Status(state.detail.clone())
                }
            };
            driver.output.send(update).await?;
            loop {
                tokio::select! {
                    biased;
                    action=self.requests.recv()=>{
                        let Some(action)=action else {return Ok(())};
                        next=self.transition(action);
                        break;
                    },
                    input=driver.input.recv()=>match input {Some(input)=>driver.discard_input(input), None=>return Ok(())},
                }
            }
        }
    }
    fn transition(&self, action: DurableAction) -> Option<Program> {
        use DurableAction::*;
        match action {
            Refresh => {
                self.model
                    .set(DurablePhase::Discovering, "Looking for remote sessions…");
                Some(Program::Discover)
            }
            Choose(reference) => {
                let mut state = self.model.0.lock().unwrap();
                state.reference = Some(reference);
                state.candidates = Arc::new([]);
                state.phase = DurablePhase::Connecting;
                state.detail = "Reattaching to the selected remote PTY".into();
                Some(Program::Attach(self.model.clone()))
            }
            Detach => {
                self.model.set(
                    DurablePhase::Detached,
                    "Detached. The remote PTY continues running.",
                );
                None
            }
            Reattach => {
                self.model.set(
                    DurablePhase::Connecting,
                    "Reattaching to the saved remote PTY",
                );
                Some(Program::Attach(self.model.clone()))
            }
            End => {
                self.model.set(
                    DurablePhase::Connecting,
                    "Verifying the remote PTY before ending it",
                );
                Some(Program::End(self.model.clone()))
            }
            StartNew | UseDirectShell => {
                self.model.0.lock().unwrap().reference = None;
                self.model
                    .set(DurablePhase::Connecting, "Starting a new shell");
                Some(if action == StartNew {
                    Program::Attach(self.model.clone())
                } else {
                    Program::Shell(Some(self.model.clone()))
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::compatible_version;
    #[test]
    fn accepts_compatible_zmx_versions_after_the_first_supported_release() {
        for version in ["1.0.1", "1.0.1+distro.1", "1.0.4", "1.2.0", "1.1.0-rc.1"] {
            assert!(
                compatible_version(format!("zmx\t\t{version}\nghostty_vt\tv\n").as_bytes()),
                "{version}"
            );
        }
        for version in [
            "0.8.2",
            "1.0.0",
            "1.0.1-rc.1",
            "2.0.0",
            "1.2",
            "1.02.0",
            "1.2.0.1",
            "1.2.0+",
            "1.2.fail",
            "1.2.1 extra",
        ] {
            assert!(
                !compatible_version(format!("zmx\t\t{version}\n").as_bytes()),
                "{version}"
            );
        }
        assert!(!compatible_version(b"other 1.2.3\nzmx 1.0.1"));
    }
}
