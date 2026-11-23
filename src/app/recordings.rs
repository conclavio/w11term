use super::*;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};
use w11term::session::{ReplayAction, ReplayState};

pub(super) const START: &str = "Start timed recording…";
pub(super) const STOP: &str = "Stop recording";
pub(super) const OPEN: &str = "Open recording…";
pub(super) const OPEN_PAUSED: &str = "Open recording paused…";
const PAUSE: &str = "Pause replay";
const RESUME: &str = "Resume replay";
const STEP: &str = "Next recorded update";

pub(super) fn is_command(label: &str) -> bool {
    matches!(
        label,
        START | STOP | OPEN | OPEN_PAUSED | PAUSE | RESUME | STEP
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Start(u64),
    Stop { pane: u64, id: u64 },
    Open,
    OpenPaused,
    Playback(u64, ReplayAction),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Choices {
    start: Option<u64>,
    stop: Option<(u64, u64)>,
    open: bool,
    playback: Option<(u64, ReplayState)>,
}

impl Choices {
    pub(super) fn target(self, label: &str) -> Option<Choice> {
        match label {
            START => self.start.map(Choice::Start),
            STOP => self.stop.map(|(pane, id)| Choice::Stop { pane, id }),
            OPEN if self.open => Some(Choice::Open),
            OPEN_PAUSED if self.open => Some(Choice::OpenPaused),
            PAUSE | RESUME | STEP => {
                let (pane, state) = self.playback?;
                let action = match (label, state) {
                    (PAUSE, ReplayState::Playing) => ReplayAction::Pause,
                    (RESUME, ReplayState::Paused) => ReplayAction::Resume,
                    (STEP, ReplayState::Playing | ReplayState::Paused) => ReplayAction::Step,
                    _ => return None,
                };
                Some(Choice::Playback(pane, action))
            }
            _ => None,
        }
    }

    pub(super) fn menu(self) -> MenuItem {
        let mut items = vec![
            command_menu_item("start-recording", START, self.start.is_some()),
            command_menu_item("stop-recording", STOP, self.stop.is_some()),
            command_menu_item("open-recording", OPEN, self.open),
            command_menu_item("open-recording-paused", OPEN_PAUSED, self.open),
        ];
        if self.playback.is_some() {
            for (id, label) in [
                ("pause-replay", PAUSE),
                ("resume-replay", RESUME),
                ("step-replay", STEP),
            ] {
                items.push(command_menu_item(id, label, self.target(label).is_some()));
            }
        }
        MenuItem::submenu("recordings", "Recordings", items)
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
    Playback,
    Stopped { pane: u64, path: PathBuf },
    Opened(Box<Source>),
}

pub enum Action {
    Invoke(Choice),
    Finished(Choice, Result<Option<Outcome>, String>),
    Dismiss,
}

fn worker_reply<T>(
    worker: &mpsc::UnboundedSender<Command>,
    request: impl FnOnce(oneshot::Sender<Result<T, String>>) -> Command,
) -> anyhow::Result<T> {
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
            Self::Open | Self::OpenPaused => {
                let Some(path) = pick(file_picker::Purpose::OpenRecording)? else {
                    return Ok(None);
                };
                let source = Source::open_recording(path)?;
                let source = if self == Self::OpenPaused {
                    source.start_paused()?
                } else {
                    source
                };
                Outcome::Opened(Box::new(source))
            }
            Self::Playback(_, action) => {
                let worker = worker.context("The replay pane has closed")?;
                worker_reply(worker, |reply| Command::Replay { action, reply })?;
                Outcome::Playback
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
            result.playback = live
                .session
                .events
                .replay_state()
                .map(|state| (pane, state));
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
                    Choice::Start(pane) | Choice::Stop { pane, .. } | Choice::Playback(pane, _) => {
                        Some(pane)
                    }
                    Choice::Open | Choice::OpenPaused => None,
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
                    Ok(Some(Outcome::Started | Outcome::Playback)) => {
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
        for choice in [Choice::Open, Choice::OpenPaused] {
            assert!(choice.run(None, |_| Ok(None)).unwrap().is_none());
        }
    }

    #[test]
    fn opening_paused_requires_a_timing_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.vt");
        std::fs::write(&path, b"shell prompt").unwrap();
        let pick = |_| Ok(Some(path.clone()));
        assert!(matches!(Choice::Open.run(None, pick).unwrap(),
            Some(Outcome::Opened(source)) if matches!(*source, Source::ViewRecording(_))));
        assert!(
            Choice::OpenPaused
                .run(None, pick)
                .is_err_and(|e| e.to_string().contains("requires a timed recording"))
        );
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
    fn playback_choices_match_the_active_pane_state_and_keep_the_original_target() {
        let playing = Choices {
            playback: Some((41, ReplayState::Playing)),
            open: true,
            ..Default::default()
        };
        let paused = Choices {
            playback: Some((42, ReplayState::Paused)),
            ..playing
        };
        assert_eq!(
            playing.target(PAUSE),
            Some(Choice::Playback(41, ReplayAction::Pause))
        );
        assert!(playing.target(RESUME).is_none());
        assert_eq!(
            paused.target(RESUME),
            Some(Choice::Playback(42, ReplayAction::Resume))
        );
        assert_eq!(
            paused.target(STEP),
            Some(Choice::Playback(42, ReplayAction::Step))
        );
        assert_eq!(paused.target(OPEN_PAUSED), Some(Choice::OpenPaused));
        for state in [
            ReplayState::Loading,
            ReplayState::Failed,
            ReplayState::Complete,
        ] {
            let idle = Choices {
                playback: Some((41, state)),
                ..playing
            };
            assert!(
                [STEP, PAUSE, RESUME]
                    .into_iter()
                    .all(|label| idle.target(label).is_none())
            );
        }
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
            matches!(result, Some(Outcome::Stopped { pane: 7, path }) if path == std::path::Path::new("session.vt"))
        );
        worker.join().unwrap();
    }
}
