//! Capture time drives protocol timers as well as data and window size. A
//! separate reader validates the files and keeps only one record of lookahead.
use crate::recorder::{ReplayEvent, TimedReplay};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};
use wezterm_term::TerminalSize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayAction {
    #[default]
    Status,
    Pause,
    Resume,
    Step,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayState {
    Loading,
    Playing,
    Paused,
    Complete,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayStatus {
    pub state: ReplayState,
    pub position_us: u64,
    pub duration_us: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
type Reply = oneshot::Sender<Result<ReplayStatus, String>>;

enum Record {
    Initial {
        size: TerminalSize,
        duration: Duration,
        first: Duration,
    },
    Event {
        at: Duration,
        event: ReplayEvent,
        next: Duration,
    },
    End,
}

fn read(path: &Path, tx: &mpsc::Sender<anyhow::Result<Record>>) -> anyhow::Result<()> {
    let mut input = TimedReplay::open(path)?;
    let duration = input.duration();
    let mut current = input.next_event()?;
    let at = |entry: &Option<(Duration, ReplayEvent)>| entry.as_ref().map_or(duration, |e| e.0);
    if tx
        .blocking_send(Ok(Record::Initial {
            size: input.initial_size(),
            duration,
            first: at(&current),
        }))
        .is_err()
    {
        return Ok(());
    }
    while let Some((time, event)) = current {
        current = input.next_event()?;
        if tx
            .blocking_send(Ok(Record::Event {
                at: time,
                event,
                next: at(&current),
            }))
            .is_err()
        {
            return Ok(());
        }
    }
    let _ = tx.blocking_send(Ok(Record::End));
    Ok(())
}

pub(super) enum Update {
    Clock(Instant),
    Event(Instant, ReplayEvent),
    State,
}

pub(super) struct Playback {
    path: PathBuf,
    records: mpsc::Receiver<anyhow::Result<Record>>,
    pub state: ReplayState,
    error: Option<String>,
    duration: Option<Duration>,
    /// Protocol timers use epoch + capture position, including during manual steps.
    epoch: Instant,
    position: Duration,
    wall_anchor: Instant,
    next_at: Duration,
    start_paused: bool,
    pub in_group: bool,
    pause_at: Option<Duration>,
    after_group: Option<Reply>,
    after_publish: Option<Reply>,
}

impl Playback {
    pub fn open(path: PathBuf, start_paused: bool) -> Self {
        let (tx, records) = mpsc::channel(1);
        let input = path.clone();
        let worker = std::thread::Builder::new()
            .name("vt-recording-reader".into())
            .spawn(move || {
                if let Err(error) = read(&input, &tx) {
                    let _ = tx.blocking_send(Err(error));
                }
            });
        let now = Instant::now();
        let mut playback = Self {
            path,
            records,
            state: ReplayState::Loading,
            error: None,
            duration: None,
            epoch: now,
            position: Duration::ZERO,
            wall_anchor: now,
            next_at: Duration::ZERO,
            start_paused,
            in_group: false,
            pause_at: None,
            after_group: None,
            after_publish: None,
        };
        if let Err(error) = worker {
            playback.fail(error.to_string());
        }
        playback
    }

    fn current_position(&self) -> Duration {
        if self.state == ReplayState::Playing {
            (self.position + self.wall_anchor.elapsed()).min(self.next_at)
        } else {
            self.position
        }
    }
    fn status(&self) -> ReplayStatus {
        ReplayStatus {
            state: self.state,
            position_us: self.current_position().as_micros() as u64,
            duration_us: self.duration.map(|v| v.as_micros() as u64),
            error: self.error.clone(),
        }
    }
    pub fn label(&self) -> String {
        let path = self.path.display();
        match self.state {
            ReplayState::Loading => format!("Loading timed replay · {path}"),
            ReplayState::Playing => format!("Timed replay · {path}"),
            ReplayState::Complete => format!("Timed replay complete · {path}"),
            ReplayState::Failed => format!(
                "Timed replay: {}",
                self.error.as_deref().unwrap_or_default()
            ),
            ReplayState::Paused => format!(
                "Timed replay paused · {} / {} ms · {path}",
                self.position.as_millis(),
                self.duration.unwrap_or_default().as_millis()
            ),
        }
    }

    pub fn label_key(&self) -> (ReplayState, Duration) {
        (
            self.state,
            if self.state == ReplayState::Paused {
                self.position
            } else {
                Duration::ZERO
            },
        )
    }

    fn fail(&mut self, reason: String) {
        self.position = self.current_position();
        self.state = ReplayState::Failed;
        self.in_group = false;
        self.error = Some(reason.clone());
        if let Some(reply) = self.after_group.take() {
            let _ = reply.send(Err(reason));
        }
    }

    /// Returns capture time for bringing display timers up to a pause boundary.
    pub fn control(&mut self, action: ReplayAction, reply: Reply) -> Option<Instant> {
        if action == ReplayAction::Status {
            let _ = reply.send(Ok(self.status()));
            return None;
        }
        if matches!(self.state, ReplayState::Complete | ReplayState::Failed) {
            let _ = reply.send(Err(self
                .error
                .clone()
                .unwrap_or_else(|| "Timed replay has completed".into())));
            return None;
        }
        if self.after_group.is_some() {
            let _ = reply.send(Err(
                "A replay operation is finishing the current timestamp".into()
            ));
            return None;
        }
        let position = self.current_position();
        // A pause at a due timestamp must acknowledge the same frame it reports.
        // Leave an explicitly paused startup at zero until its first step.
        let due_pause = action == ReplayAction::Pause
            && self.state == ReplayState::Playing
            && position == self.next_at;
        if due_pause {
            self.pause_at = Some(
                (self.position + self.wall_anchor.elapsed()).min(
                    self.duration
                        .expect("playing replay has a validated duration"),
                ),
            );
        }
        if self.state == ReplayState::Loading {
            self.start_paused = action != ReplayAction::Resume;
        } else if action != ReplayAction::Resume || self.state != ReplayState::Playing {
            self.position = position;
            self.wall_anchor = Instant::now();
            self.state = if action == ReplayAction::Resume {
                ReplayState::Playing
            } else {
                ReplayState::Paused
            };
        }
        if action == ReplayAction::Step || self.in_group || due_pause {
            self.after_group = Some(reply);
        } else {
            self.after_publish = Some(reply);
        }
        (self.state != ReplayState::Loading).then_some(self.epoch + position)
    }

    pub fn published(&mut self) {
        if !self.in_group
            && let Some(reply) = self.after_publish.take()
        {
            let _ = reply.send(Ok(self.status()));
        }
    }

    /// Only the receiver wait and the wall timer suspend. The caller can freely
    /// cancel either wait when it handles pane input or a playback command.
    pub async fn next(&mut self, deadline: Option<Instant>) -> Update {
        if matches!(self.state, ReplayState::Complete | ReplayState::Failed)
            || (self.state == ReplayState::Paused && self.after_group.is_none() && !self.in_group)
        {
            return std::future::pending().await;
        }
        if let Some(target) = self.pause_at
            && self.next_at > target
        {
            // The reader has delivered every event due at the requested pause.
            // Settle protocol timers through any remaining gap before replying.
            self.pause_at = None;
            self.position = target;
            self.after_publish = self.after_group.take();
            return Update::Clock(self.epoch + target);
        }
        if self.state == ReplayState::Playing && self.after_group.is_none() && !self.in_group {
            let next = deadline.map_or(self.next_at, |d| {
                d.saturating_duration_since(self.epoch).min(self.next_at)
            });
            let wake = self.wall_anchor + next.saturating_sub(self.position);
            if wake > Instant::now() {
                tokio::time::sleep_until(wake).await;
            }
            let position = self.current_position();
            if position < self.next_at {
                return Update::Clock(self.epoch + position);
            }
        }
        match self.records.recv().await {
            Some(Ok(Record::Initial {
                size,
                duration,
                first,
            })) => {
                self.duration = Some(duration);
                self.next_at = first;
                self.wall_anchor = Instant::now();
                self.state = if self.start_paused {
                    ReplayState::Paused
                } else {
                    ReplayState::Playing
                };
                Update::Event(self.epoch, ReplayEvent::Resize(size))
            }
            Some(Ok(Record::Event { at, event, next })) => {
                self.next_at = next;
                self.in_group = at == next;
                if self.state == ReplayState::Paused {
                    self.position = at;
                }
                if !self.in_group && self.pause_at.is_none() {
                    self.after_publish = self.after_group.take();
                }
                Update::Event(self.epoch + at, event)
            }
            Some(Ok(Record::End)) => {
                self.position = self
                    .duration
                    .expect("validated footer follows initial size");
                self.state = ReplayState::Complete;
                self.pause_at = None;
                self.in_group = false;
                self.after_publish = self.after_group.take();
                Update::Clock(self.epoch + self.position)
            }
            Some(Err(error)) => {
                self.fail(format!("{error:#}"));
                Update::State
            }
            None => {
                self.fail("timed recording reader stopped unexpectedly".into());
                Update::State
            }
        }
    }
}

#[cfg(test)]
mod tests;
