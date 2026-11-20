//! Opt-in remote PTY lifecycle. A shell channel stays private until its
//! backend generation is verified on the same authenticated SSH transport.
use super::{
    ConnectionShare,
    transport::{Driver, Event, OpenShell},
};
use crate::{durable::DurableReference, options::SshOptions};
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurableAction {
    Detach,
    Reattach,
    End,
    StartNew,
    UseDirectShell,
}

#[derive(Clone, Debug)]
pub struct DurableState {
    pub phase: DurablePhase,
    pub reference: Option<DurableReference>,
    pub detail: String,
}
impl DurableState {
    pub fn permits(&self, action: DurableAction) -> bool {
        use DurableAction::*;
        use DurablePhase::*;
        match action {
            Detach => matches!(self.phase, Attached | Reconnecting) && self.reference.is_some(),
            Reattach => {
                matches!(self.phase, Detached | Missing | Failed | Unavailable)
                    && self.reference.is_some()
            }
            End => matches!(self.phase, Attached | Detached) && self.reference.is_some(),
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
impl DurableControl {
    pub(crate) fn new(reference: Option<DurableReference>) -> (Self, Worker) {
        let model = Model(Arc::new(Mutex::new(DurableState {
            phase: DurablePhase::Connecting,
            reference,
            detail: "Connecting to durable shell".into(),
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
            state.permits(action),
            "This action is unavailable for the current durable shell state"
        );
        self.requests
            .try_send(action)
            .context("A durable shell action is already pending or the pane has closed")?;
        state.phase = DurablePhase::Connecting;
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
    Shell(Option<Model>),
    Attach(Model),
    End(Model),
}
impl Program {
    pub(super) fn peer_fingerprint(&self) -> Option<String> {
        match self {
            Self::Attach(model) | Self::End(model) => model.reference().map(|r| r.host_key),
            Self::Shell(_) => None,
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
            Self::Shell(_) => unreachable!(),
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
        }
        let version = command(&shell.link, "zmx version").await?;
        ensure!(
            version.status == 0,
            failure(
                Unavailable,
                "Unable to run zmx on this SSH account. Start a direct shell or install zmx on the remote host."
            )
        );
        ensure!(
            compatible_version(&version.stdout),
            failure(
                Unavailable,
                "Unsupported zmx version. Durable shells require zmx 1.x, starting with 1.0.1."
            )
        );
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
        let channel = shell.channel.channel();
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
        channel.exec(true, attach).await?;
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
                    Some(
                        ChannelMsg::Failure | ChannelMsg::ExitStatus { .. } | ChannelMsg::Close,
                    ) => {
                        return Err(failure(
                            Unavailable,
                            "The remote server refused the zmx terminal or exec request",
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

struct CommandResult {
    status: u32,
    stdout: Vec<u8>,
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
        let mut size = 0usize;
        let mut status = None;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    size += data.len();
                    if size <= 8192 {
                        stdout.extend_from_slice(&data);
                    }
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => size += data.len(),
                Some(ChannelMsg::ExitStatus { exit_status }) => status = Some(exit_status),
                Some(ChannelMsg::Close) | None => break,
                Some(ChannelMsg::Failure) => {
                    return Err(failure(
                        DurablePhase::Unavailable,
                        "The SSH server refused a zmx command",
                    ));
                }
                _ => {}
            }
            ensure!(
                size <= 8192,
                failure(
                    DurablePhase::Unavailable,
                    "Unexpected output from remote zmx command"
                )
            );
        }
        let status =
            status.context("SSH command channel closed before reporting an exit status")?;
        Ok(CommandResult { status, stdout })
    })
    .await
    .context("Timed out waiting for the remote zmx command")?
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
        let mut next = Some(Program::Attach(self.model.clone()));
        loop {
            if let Some(program) = next.take() {
                let result = tokio::select! {
                    biased;
                    action=self.requests.recv()=>{
                        let Some(action)=action else {return Ok(())};
                        next=self.transition(action);
                        continue;
                    },
                    result=driver.run(&program)=>result
                };
                match result {
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
            let detail = self.model.0.lock().unwrap().detail.clone();
            driver.output.send(Event::Status(detail)).await?;
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
