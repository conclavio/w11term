//! Bounded per-pane event journal with monotonic cursors and loss detection.
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;
mod inbox;
pub use inbox::Inbox;
pub(crate) use inbox::display_field;

const CAPACITY: usize = 4096;
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub kind: String,
    pub data: Value,
}
#[derive(Clone, Debug, Serialize)]
pub struct Batch {
    pub events: Vec<Event>,
    pub next: u64,
    pub oldest: u64,
    pub lost: bool,
}
#[derive(Debug, Default)]
struct State {
    next: u64,
    events: VecDeque<Event>,
    progress: wezterm_term::Progress,
    recording: Option<(u64, Arc<Path>)>,
    replay: Option<crate::session::ReplayState>,
    inbox: Option<(u64, Arc<str>, Inbox)>,
    command_watch: Option<CommandWatch>,
    restart_token: Option<u64>,
    restart_closed: bool,
}

#[derive(Debug)]
struct CommandWatch {
    generation: u64,
    armed: bool,
    running: Option<RunningCommand>,
}

impl CommandWatch {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            armed: false,
            running: None,
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.generation);
    }
}

#[derive(Debug)]
struct RunningCommand {
    /// Keep a fixed-size identity even if the shell supplies a large OSC 133 aid.
    aid: Option<[u8; 32]>,
}

fn application_id(aid: Option<&str>) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    aid.map(|id| Sha256::digest(id.as_bytes()).into())
}

impl State {
    fn watch_state(&self) -> Option<bool> {
        self.command_watch.as_ref().map(|watch| watch.armed)
    }

    fn refresh_watch(&self, previous: Option<bool>) {
        if previous != self.watch_state()
            && let Some((_, _, inbox)) = &self.inbox
        {
            inbox.refresh();
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Events {
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
}
impl Events {
    pub fn replay_state(&self) -> Option<crate::session::ReplayState> {
        self.state.lock().unwrap().replay
    }

    pub(crate) fn set_replay_state(&self, replay: crate::session::ReplayState) {
        let mut state = self.state.lock().unwrap();
        if state.replay.replace(replay) != Some(replay)
            && let Some((_, _, inbox)) = &state.inbox
        {
            inbox.refresh();
        }
    }

    /// Sequence of the current recording's start, retained after journal eviction.
    pub fn recording(&self) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .recording
            .as_ref()
            .map(|(id, _)| *id)
    }

    pub fn recording_path(&self) -> Option<Arc<Path>> {
        self.state
            .lock()
            .unwrap()
            .recording
            .as_ref()
            .map(|(_, path)| Arc::clone(path))
    }

    pub(crate) fn recording_started(&self, path: &Path) -> u64 {
        self.update_recording(
            Some(path),
            "recording-started",
            serde_json::json!({"path": path}),
        )
    }

    pub(crate) fn recording_stopped(&self, bytes: u64) {
        self.update_recording(
            None,
            "recording-stopped",
            serde_json::json!({"bytes": bytes}),
        );
    }

    pub(crate) fn recording_error(&self, error: String) {
        self.update_recording(None, "recording-error", serde_json::json!({"error": error}));
    }

    fn update_recording(&self, path: Option<&Path>, kind: &str, data: Value) -> u64 {
        let data = Self::bounded_data(kind, data);
        let mut state = self.state.lock().unwrap();
        let sequence = state.next + 1;
        let recording = path.map(|path| (sequence, Arc::from(path)));
        if state.recording != recording {
            state.recording = recording;
            if let Some((_, _, inbox)) = &state.inbox {
                inbox.refresh();
            }
        }
        self.append(state, kind, data);
        sequence
    }

    pub fn restart_token(&self) -> Option<u64> {
        self.state.lock().unwrap().restart_token
    }

    pub(crate) fn restart_exited(
        &self,
        input: &tokio::sync::mpsc::UnboundedSender<crate::session::Command>,
        token: u64,
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        anyhow::ensure!(
            state.restart_token == Some(token),
            "Pane is running, unavailable, or the exit token has expired"
        );
        state.restart_token = None;
        if let Some((_, _, inbox)) = &state.inbox {
            inbox.refresh();
        }
        input
            .send(crate::session::Command::RestartExited(
                crate::session::ClaimedRestart(token),
            ))
            .map_err(|_| anyhow::anyhow!("The terminal pane has closed"))
    }

    pub(crate) fn pane_exited(&self, token: u64) {
        let mut state = self.state.lock().unwrap();
        if state.restart_closed {
            return;
        }
        state.restart_token = Some(token);
        if let Some((_, _, inbox)) = &state.inbox {
            inbox.refresh();
        }
        self.append(
            state,
            "pane-exited",
            serde_json::json!({"exit_token":token}),
        );
    }

    pub(crate) fn close_restart(&self) {
        let mut state = self.state.lock().unwrap();
        state.restart_closed = true;
        if state.restart_token.take().is_some()
            && let Some((_, _, inbox)) = &state.inbox
        {
            inbox.refresh();
        }
    }

    /// None while disconnected or on streams that may repaint historical bytes.
    pub fn command_watch(&self) -> Option<bool> {
        self.state.lock().unwrap().watch_state()
    }

    /// Current live shell stream, including the interval before its first frame.
    pub fn shell_generation(&self) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .command_watch
            .as_ref()
            .map(|watch| watch.generation)
    }

    pub fn idle_shell(&self, generation: u64) -> bool {
        self.state
            .lock()
            .unwrap()
            .command_watch
            .as_ref()
            .is_some_and(|watch| watch.generation == generation && watch.running.is_none())
    }

    pub fn set_command_watch(&self, armed: bool) -> bool {
        let mut state = self.state.lock().unwrap();
        let previous = state.watch_state();
        let Some(watch) = &mut state.command_watch else {
            return false;
        };
        watch.armed = armed;
        state.refresh_watch(previous);
        true
    }

    pub(crate) fn command_stream(&self, generation: Option<u64>) {
        let mut state = self.state.lock().unwrap();
        let previous = state.watch_state();
        state.command_watch = generation.map(CommandWatch::new);
        state.refresh_watch(previous);
    }

    pub(crate) fn reset_command_watch(&self) {
        let mut state = self.state.lock().unwrap();
        let previous = state.watch_state();
        if let Some(watch) = &mut state.command_watch {
            watch.reset();
        }
        state.refresh_watch(previous);
    }

    pub(crate) fn semantic_prompt(&self, marker: &wezterm_term::FinalTermSemanticPrompt) {
        use wezterm_term::FinalTermSemanticPrompt::*;
        let mut state = self.state.lock().unwrap();
        let previous = state.watch_state();
        let Some(watch) = &mut state.command_watch else {
            return;
        };
        let (aid, exit_code) = match marker {
            MarkEndOfInputAndStartOfOutput { aid, .. } => {
                // A repeated start or a nested shell cannot steal the original watch.
                watch.running.get_or_insert_with(|| RunningCommand {
                    aid: application_id(aid.as_deref()),
                });
                return;
            }
            CommandStatus { status, aid } => (aid, *status),
            FreshLineAndStartPrompt { aid, .. } | MarkEndOfCommandWithFreshLine { aid, .. } => {
                (aid, None)
            }
            // Without an end marker or a fresh prompt, a new input phase
            // abandons the previous unqualified command. It cannot prove success.
            MarkEndOfPromptAndStartOfInputUntilNextMarker
            | MarkEndOfPromptAndStartOfInputUntilEndOfLine => {
                if watch
                    .running
                    .as_ref()
                    .is_some_and(|command| command.aid.is_none())
                {
                    watch.reset();
                    state.refresh_watch(previous);
                }
                return;
            }
            StartPrompt(_) | FreshLine => return,
        };
        if !watch
            .running
            .as_ref()
            .is_some_and(|command| command.aid == application_id(aid.as_deref()))
        {
            return;
        }
        watch.running = None;
        if std::mem::take(&mut watch.armed) {
            self.append(
                state,
                "command-finished",
                serde_json::json!({"exit_code":exit_code}),
            );
        }
    }

    /// Progress is current pane state. Frequent updates must not evict journal entries.
    pub fn progress(&self) -> wezterm_term::Progress {
        self.state.lock().unwrap().progress
    }

    pub(crate) fn set_progress(&self, progress: wezterm_term::Progress) {
        let mut state = self.state.lock().unwrap();
        if state.progress != progress {
            state.progress = progress;
            if let Some((_, _, inbox)) = &state.inbox {
                inbox.refresh();
            }
        }
    }

    /// Attach a pane to its owning window, preserving acknowledgements when it moves.
    pub fn connect_inbox(&self, pane: u64, label: &str, inbox: &Inbox) {
        let mut state = self.state.lock().unwrap();
        if let Some((_, _, old)) = &state.inbox {
            if old.same_window(inbox) {
                return;
            }
            old.move_pane_to(pane, inbox);
            state.inbox = Some((pane, Arc::from(label), inbox.clone()));
        } else {
            let label = Arc::from(label);
            for event in &state.events {
                inbox.receive(pane, &label, event, false);
            }
            state.inbox = Some((pane, label, inbox.clone()));
        }
    }

    pub fn push(&self, kind: &str, data: Value) {
        let data = Self::bounded_data(kind, data);
        self.append(self.state.lock().unwrap(), kind, data);
    }

    fn bounded_data(kind: &str, data: Value) -> Value {
        let serialized = serde_json::to_string(&data).unwrap_or_default();
        if serialized.len() > 4096 {
            let mut preview = serde_json::json!({"truncated":true,"preview":serialized.chars().take(1024).collect::<String>()});
            if kind == "notification" {
                preview["summary"] = inbox::notification_text(&data).into();
            }
            if kind == "recording-error"
                && let Some(reason) = data["error"].as_str()
            {
                preview["error"] = reason.chars().take(200).collect::<String>().into();
            }
            preview
        } else {
            data
        }
    }

    fn append(&self, mut state: std::sync::MutexGuard<'_, State>, kind: &str, data: Value) {
        state.next += 1;
        let sequence = state.next;
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let event = Event {
            sequence,
            timestamp_ms,
            kind: kind.into(),
            data,
        };
        if let Some((pane, label, inbox)) = &state.inbox {
            inbox.receive(*pane, label, &event, true);
        }
        state.events.push_back(event);
        if state.events.len() > CAPACITY {
            state.events.pop_front();
        }
        drop(state);
        self.changed.notify_waiters();
    }
    pub fn since(&self, cursor: u64) -> Batch {
        let state = self.state.lock().unwrap();
        let oldest = state.events.front().map_or(state.next + 1, |e| e.sequence);
        Batch {
            events: state
                .events
                .iter()
                .filter(|e| e.sequence > cursor)
                .cloned()
                .collect(),
            next: state.next,
            oldest,
            lost: cursor < oldest.saturating_sub(1),
        }
    }
    pub async fn wait(&self, cursor: u64, timeout: std::time::Duration) -> Batch {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let batch = self.since(cursor);
            if !batch.events.is_empty() || batch.lost {
                return batch;
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                return self.since(cursor);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_recording_survives_journal_eviction_and_wakes_only_its_own_window() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::default());
        let counter = calls.clone();
        let inbox = Inbox::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        let pane = Events::default();
        pane.connect_inbox(11, "Loopback", &inbox);
        let path = std::path::Path::new("session.vt");
        pane.recording_started(path);
        let first = pane.recording().unwrap();
        assert_eq!(first, pane.since(0).events[0].sequence);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        inbox.acknowledge_wake();
        for _ in 0..CAPACITY {
            pane.push("title", Value::Null);
        }
        assert!(pane.since(0).lost);
        assert_eq!(pane.recording(), Some(first));
        assert_eq!(pane.recording_path().as_deref(), Some(path));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        // An ordinary event producer cannot change recorder ownership.
        pane.push("recording-error", Value::Null);
        assert_eq!(pane.recording(), Some(first));
        pane.recording_stopped(42);
        assert_eq!(pane.recording(), None);
        assert!(pane.recording_path().is_none());
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        pane.recording_started(path);
        assert!(pane.recording().unwrap() > first);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        inbox.acknowledge_wake();

        let new_calls = Arc::new(AtomicUsize::default());
        let counter = new_calls.clone();
        let moved_to = Inbox::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        pane.connect_inbox(11, "Loopback", &moved_to);
        let reason = "disk\nfull \u{202e} on D:";
        pane.recording_error(reason.to_string());
        assert_eq!(pane.recording(), None);
        assert!(pane.recording_path().is_none());
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(new_calls.load(Ordering::Relaxed), 1);
        assert_eq!(moved_to.unread_count(), 1);
        assert_eq!(inbox.unread_count(), 0);
        let notice = moved_to.rows().pop().unwrap();
        assert_eq!(notice.message, "Recording failed · disk full on D:");
        assert!(!moved_to.acknowledge_wake());
        let last = pane.since(0).events.pop().unwrap();
        assert_eq!(last.kind, "recording-error");
        assert_eq!(last.data["error"], reason);
        assert_eq!(last.sequence, notice.sequence);
    }

    #[test]
    fn oversized_recording_errors_keep_a_bounded_reason_when_attaching_the_inbox() {
        let pane = Events::default();
        pane.recording_error(format!("Disk full\n{}", "雪".repeat(5000)));
        let inbox = Inbox::new(|| {});
        pane.connect_inbox(3, "Loopback", &inbox);
        let row = inbox.rows().pop().unwrap();
        assert!(row.message.starts_with("Recording failed · Disk full 雪"));
        assert_eq!(row.message.chars().count(), 200);
        assert_eq!(pane.since(0).events[0].data["truncated"], true);
    }

    #[test]
    fn live_progress_coalesces_wakes_without_creating_notifications_or_journal_rows() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use wezterm_term::Progress;
        let calls = Arc::new(AtomicUsize::default());
        let counter = calls.clone();
        let inbox = Inbox::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        let pane = Events::default();
        pane.connect_inbox(11, "Loopback", &inbox);
        for value in 1..=100 {
            pane.set_progress(Progress::Percentage(value));
        }
        assert_eq!(pane.progress(), Progress::Percentage(100));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(inbox.unread_count(), 0);
        assert!(pane.since(0).events.is_empty());
        inbox.acknowledge_wake();
        pane.set_progress(Progress::Percentage(100));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        pane.set_progress(Progress::None);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn cursors_detect_loss_and_wait_without_polling() {
        let e = Events::default();
        let waiter = tokio::spawn({
            let e = e.clone();
            async move { e.wait(0, std::time::Duration::from_secs(1)).await }
        });
        e.push("bell", serde_json::json!({}));
        assert_eq!(waiter.await.unwrap().events.len(), 1);
        for _ in 0..CAPACITY {
            e.push("title", Value::Null);
        }
        let b = e.since(0);
        assert!(b.lost);
        assert_eq!(b.events.len(), CAPACITY);
        assert_eq!(b.oldest, 2);
        assert!(e.since(b.next).events.is_empty());
    }
}
