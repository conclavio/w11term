use crate::{
    events::Events,
    options::{Authentication, Source},
    prompts::Prompts,
    ssh,
    terminal::TransportInput,
};
use tokio::{sync::mpsc, task::JoinSet};
use wezterm_term::TerminalSize;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Running,
    Restarting { reset: bool },
    PublishingExit,
    Exited,
}

pub(super) struct Launcher {
    source: Source,
    sharing: ssh::Sharing,
    prompts: Prompts,
    events: Events,
}

impl Launcher {
    pub(super) fn new(
        source: Source,
        sharing: ssh::Sharing,
        prompts: Prompts,
        events: Events,
    ) -> Self {
        Self {
            source,
            sharing,
            prompts,
            events,
        }
    }

    pub(super) fn set_authentication(&mut self, auth: Authentication) {
        if let Source::Ssh(options) = &mut self.source {
            options.set_authentication(auth);
        }
    }

    pub(super) fn start(
        &mut self,
        tasks: &mut JoinSet<()>,
        size: TerminalSize,
        input: mpsc::UnboundedReceiver<TransportInput>,
        output: mpsc::Sender<ssh::Event>,
        reconnect_gate: Option<ssh::LoginGate>,
    ) -> String {
        match &self.source {
            Source::Local(shell) => {
                let status = format!("Starting {}…", shell.executable.display());
                let shell = shell.clone();
                let generation = self.sharing.next_generation();
                tasks.spawn(async move {
                    if let Err(error) = crate::conpty::run_generation(
                        shell,
                        size,
                        input,
                        output.clone(),
                        generation,
                    )
                    .await
                    {
                        let _ = output
                            .send(ssh::Event::Status(format!("Local shell: {error:#}")))
                            .await;
                    }
                    let _ = output.send(ssh::Event::Disconnected).await;
                });
                status
            }
            Source::Ssh(options) => {
                let status = format!(
                    "Connecting to {}@{}:{}…",
                    options.user, options.host, options.port
                );
                let options = options.clone();
                let mut sharing = self.sharing.for_launch();
                if reconnect_gate.is_some() {
                    sharing.launch_gate = reconnect_gate;
                }
                let prompts = self.prompts.clone();
                let events = self.events.clone();
                tasks.spawn_on(
                    async move {
                        let result = ssh::run_with_sharing(
                            options,
                            size,
                            input,
                            output.clone(),
                            Some(prompts),
                            events,
                            sharing,
                        )
                        .await;
                        let event = match result {
                            Ok(()) => ssh::Event::Status("Disconnected".into()),
                            Err(error) if error.is::<ssh::AttemptStopped>() => {
                                ssh::Event::ConnectionStopped
                            }
                            Err(error) => ssh::Event::ConnectionFailed(format!("{error:#}")),
                        };
                        let _ = output.send(event).await;
                    },
                    ssh::NETWORK.handle(),
                );
                status
            }
            Source::Replay(_)
            | Source::ViewRecording(_)
            | Source::TimedReplay(_)
            | Source::PausedReplay(_)
            | Source::Demo => {
                unreachable!("only live transports are launched")
            }
        }
    }
}
