use super::*;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

pub(super) const START: &str = "Start timed recording…";
pub(super) const STOP: &str = "Stop recording";
pub(super) const OPEN: &str = "Open recording…";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Start(u64),
    Stop { pane: u64, id: u64 },
    Open,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Choices {
    start: Option<u64>,
    stop: Option<(u64, u64)>,
    open: bool,
}

impl Choices {
    pub(super) fn target(self, label: &str) -> Option<Choice> {
        match label {
            START => self.start.map(Choice::Start),
            STOP => self.stop.map(|(pane, id)| Choice::Stop { pane, id }),
            OPEN if self.open => Some(Choice::Open),
            _ => None,
        }
    }

    pub(super) fn menu(self) -> MenuItem {
        MenuItem::submenu(
            "recordings",
            "Recordings",
            [
                command_menu_item("start-recording", START, self.start.is_some()),
                command_menu_item("stop-recording", STOP, self.stop.is_some()),
                command_menu_item("open-recording", OPEN, self.open),
            ],
        )
    }
}

pub(super) fn invoke(choice: Option<Choice>) -> Message {
    choice.map_or(Message::ControlIdle, |op| {
        Message::Recording(Action::Invoke(op))
    })
}

#[derive(Default)]
pub(super) struct Recordings {
    pending: Option<Choice>,
    pub(super) status: Option<String>,
}

pub enum Outcome {
    Started,
    Stopped { pane: u64, path: PathBuf },
    Opened(Box<Source>),
}

pub enum Action {
    Invoke(Choice),
    Finished(Choice, Result<Option<Outcome>, String>),
    Dismiss,
}

fn worker_reply(
    worker: &mpsc::UnboundedSender<Command>,
    request: impl FnOnce(oneshot::Sender<Result<serde_json::Value, String>>) -> Command,
) -> anyhow::Result<serde_json::Value> {
    let (reply, result) = oneshot::channel();
    worker
        .send(request(reply))
        .map_err(|_| anyhow::anyhow!("The pane has closed"))?;
    result
        .blocking_recv()
        .context("The pane closed during the recording operation")?
        .map_err(anyhow::Error::msg)
}

impl Choice {
    fn run(
        self,
        worker: Option<&mpsc::UnboundedSender<Command>>,
        pick: impl FnOnce(file_picker::Purpose) -> anyhow::Result<Option<PathBuf>>,
    ) -> anyhow::Result<Option<Outcome>> {
        let outcome = match self {
            Self::Open => {
                let Some(path) = pick(file_picker::Purpose::OpenRecording)? else {
                    return Ok(None);
                };
                Outcome::Opened(Box::new(Source::open_recording(path)?))
            }
            Self::Start(_) => {
                let worker = worker.context("The pane has closed")?;
                let Some(path) = pick(file_picker::Purpose::StartRecording)? else {
                    return Ok(None);
                };
                worker_reply(worker, |reply| Command::StartTimedRecordingIfLive {
                    path,
                    reply,
                })?;
                Outcome::Started
            }
            Self::Stop { pane, id } => {
                let worker = worker.context("The pane has closed")?;
                let result = worker_reply(worker, |reply| Command::StopRecordingIfCurrent {
                    id,
                    reply,
                })?;
                let path = result["path"]
                    .as_str()
                    .context("The worker omitted the recording path")?;
                Outcome::Stopped {
                    pane,
                    path: path.into(),
                }
            }
        };
        Ok(Some(outcome))
    }
}

impl TerminalApp {
    fn recording_pane(&self, pane: u64) -> Option<&LiveTab> {
        let Page::Terminal(live) = &self.tabs.get(self.tab_for_pane(pane)?)?.page else {
            return None;
        };
        Some(live)
    }

    pub(super) fn recording_choices(&self) -> Choices {
        if self.recordings.pending.is_some() {
            return Choices::default();
        }
        let mut result = Choices {
            open: true,
            ..Choices::default()
        };
        if let Some(AppTab {
            page: Page::Terminal(live),
            ..
        }) = self.tabs.active()
        {
            let pane = live.session.pane_id;
            if let Some(id) = live.session.events.recording() {
                result.stop = Some((pane, id));
            } else if matches!(live.source, Source::Ssh(_) | Source::Local(_))
                && live.session.events.restart_token().is_none()
                && live.session.durable_state().is_none_or(|state| {
                    matches!(
                        state.phase,
                        w11term::ssh::DurablePhase::Attached | w11term::ssh::DurablePhase::Direct
                    )
                })
            {
                result.start = Some(pane);
            }
        }
        result
    }

    pub(super) fn recording_indicator(&self) -> Option<String> {
        let Page::Terminal(live) = &self.tabs.active()?.page else {
            return None;
        };
        let pane = live.session.pane_id;
        let path = live.session.events.recording_path()?;
        Some(format!("Recording pane {pane} to {}", path.display()))
    }

    pub(super) fn recording_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Invoke(choice) if self.recordings.pending.is_none() => {
                let pane = match choice {
                    Choice::Start(pane) | Choice::Stop { pane, .. } => Some(pane),
                    Choice::Open => None,
                };
                let worker = pane
                    .and_then(|p| self.recording_pane(p))
                    .map(|live| live.session.input.clone());
                let owner = self.keyboard.hwnd().map(|hwnd| hwnd.0 as usize);
                self.recordings.pending = Some(choice);
                ctx.spawn_background_with_rejection(
                    move |cancel| {
                        let result = if cancel.is_cancelled() {
                            Ok(None)
                        } else {
                            choice.run(worker.as_ref(), |purpose| {
                                let owner = owner.context("The window is unavailable")?;
                                Ok(file_picker::pick(owner, purpose)?
                                    .map(|file| file.path)
                                    .filter(|_| !cancel.is_cancelled()))
                            })
                        };
                        Message::Recording(Action::Finished(
                            choice,
                            result.map_err(|e| format!("{e:#}")),
                        ))
                    },
                    Message::Recording(Action::Finished(
                        choice,
                        Err("Recording controls are unavailable".into()),
                    )),
                );
            }
            Action::Finished(choice, result) if self.recordings.pending == Some(choice) => {
                self.recordings.pending = None;
                match result {
                    Ok(Some(Outcome::Started)) => {
                        self.recordings.status = None;
                    }
                    Ok(Some(Outcome::Stopped { pane, path })) => {
                        self.recordings.status = Some(format!(
                            "Saved recording for pane {pane} to {}",
                            path.display()
                        ));
                    }
                    Ok(Some(Outcome::Opened(source))) => {
                        self.recordings.status = None;
                        self.open(source_label(&source), *source, None, ctx);
                    }
                    Err(reason) => self.recordings.status = Some(reason),
                    Ok(None) => {}
                }
            }
            Action::Dismiss => self.recordings.status = None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_file_selection_leaves_the_pane_alone() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let result = Choice::Start(7)
            .run(Some(&sender), |purpose| {
                assert!(matches!(purpose, file_picker::Purpose::StartRecording));
                Ok(None)
            })
            .unwrap();
        assert!(result.is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(Choice::Open.run(None, |_| Ok(None)).unwrap().is_none());
    }

    #[test]
    fn start_rechecks_the_live_source_after_a_save_dialog() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let worker = std::thread::spawn(move || {
            let Some(Command::StartTimedRecordingIfLive { path, reply }) = receiver.blocking_recv()
            else {
                panic!("expected live pane validation");
            };
            assert_eq!(path, PathBuf::from("cockpit.vt"));
            reply.send(Err("Pane has exited".into())).unwrap();
        });
        let result = Choice::Start(7).run(Some(&sender), |_| Ok(Some("cockpit.vt".into())));
        assert!(result.is_err_and(|e| e.to_string() == "Pane has exited"));
        worker.join().unwrap();
    }

    #[test]
    fn a_queued_stop_uses_the_original_pane_and_recording_id() {
        let previous = Choices {
            stop: Some((7, 24)),
            open: true,
            ..Default::default()
        };
        let replacement = Choices {
            stop: Some((7, 26)),
            ..previous
        };
        let action = previous.target(STOP).unwrap();
        assert_ne!(replacement.target(STOP), Some(action));
        assert!(previous.target(START).is_none());
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let worker = std::thread::spawn(move || {
            let Some(Command::StopRecordingIfCurrent { id, reply }) = receiver.blocking_recv()
            else {
                panic!("expected a guarded stop");
            };
            assert_eq!(id, 24);
            reply
                .send(Ok(serde_json::json!({"path":"session.vt"})))
                .unwrap();
        });
        let result = action
            .run(Some(&sender), |_| {
                panic!("stopping a recording never opens a picker")
            })
            .unwrap();
        assert!(
            matches!(result, Some(Outcome::Stopped { pane: 7, path }) if path == PathBuf::from("session.vt"))
        );
        worker.join().unwrap();
    }
}
