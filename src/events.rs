//! Bounded per-pane event journal with monotonic cursors and loss detection.
use crate::options::GroupPromptPattern;
use crate::sftp::{TransferId, TransferOperation, TransferOutcome};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;
mod dotfile_sync;
mod group_command;
mod group_transcript;
mod inbox;
mod kitty_notification;
mod running_output;
mod ssh_attempt;
mod tool_bundle;
pub use dotfile_sync::{DotfileSyncReport, DotfileSyncStatus};
use group_command::GroupRun;
pub use group_command::{GroupCommandResult, GroupExecution, GroupInterruptTarget, GroupOutput};
pub use group_transcript::{
    GroupCompletionState, GroupOutputAlert, GroupOutputAlertRule, GroupOutputAlertTrigger,
    GroupOutputExpectation, GroupOutputExpectationState, GroupQueueActivity, GroupQueueEvent,
    GroupQueueEventKind, GroupRecordingEnd, GroupRecordingMode, GroupRecordingOutcome,
    GroupRecordingSummary, GroupTranscript, GroupTranscriptHistory, GroupTranscriptHost,
    GroupTranscriptLine, GroupTranscriptOutput, GroupTranscriptRecording, GroupTranscriptSnapshot,
    GroupTranscriptStream, GroupWriteReceipt,
};
pub(crate) use inbox::display_field;
pub use inbox::{Inbox, InboxWake, NoticeTarget, SshLoginResult};
pub use kitty_notification::{NotificationChoice, NotificationResponse};
pub use running_output::RunningOutputResult;
pub(crate) use ssh_attempt::{AttemptGuard, RetryWait, SshConnectionSnapshot};
pub use ssh_attempt::{
    PendingSshAttempt, PendingSshRetry, SshAttemptCompletion, SshAttemptOutcome,
};
pub use tool_bundle::{ToolBundleSyncReport, ToolBundleSyncStatus, ToolPathSyncStatus};

const CAPACITY: usize = 4096;
const LONG_COMMAND_THRESHOLD: Duration = Duration::from_secs(10);
pub const RECENT_SFTP_TRANSFER_LIMIT: usize = 20;
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub kind: String,
    pub data: Value,
    #[serde(skip)]
    pub(crate) command: Option<CommandLink>,
    #[serde(skip)]
    saved_download: Option<SavedDownload>,
    #[serde(skip)]
    eviction: Option<Arc<Notify>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SavedDownload {
    File(Arc<Path>),
    Folder(Arc<Path>),
    BatchFolder(Arc<Path>),
}

#[derive(Clone, Debug)]
pub struct RecentSftpTransfer {
    pub id: TransferId,
    pub finished_at: Instant,
    pub finished_unix_ms: u64,
    pub outcome: TransferOutcome,
    pub message: Arc<str>,
    pub background: bool,
    pub saved_download: Option<SavedDownload>,
    pub details: Option<Arc<SftpTransferDetails>>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SftpTransferDetails {
    pub completed: Vec<SftpTransferItem>,
    pub stopped: Option<SftpTransferItem>,
    pub omitted_completed: usize,
    pub not_started: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SftpTransferItem {
    pub operation: TransferOperation,
    pub remote: String,
    pub local: PathBuf,
    pub outcome: TransferOutcome,
    pub bytes_transferred: Option<u64>,
}

#[derive(Default)]
struct EventLinks {
    command: Option<CommandLink>,
    saved_download: Option<SavedDownload>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandLink {
    pub generation: u64,
    pub id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunningCommandTarget {
    pub generation: u64,
    id: u64,
    pub start_token: u64,
}

impl RunningCommandTarget {
    pub fn new(generation: u64, command: &wezterm_term::RunningCommand) -> Self {
        Self {
            generation,
            id: command.id,
            start_token: command.start_token,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommandInterruptPermit {
    events: Events,
    nonce: Arc<()>,
}

impl CommandInterruptPermit {
    /// Called by the transport immediately before the write. Each permit is single-use.
    pub(crate) fn claim(&self) -> bool {
        let mut state = self.events.state.lock().unwrap();
        let current = state
            .command_interrupt
            .as_ref()
            .is_some_and(|(_, pending)| {
                pending
                    .upgrade()
                    .is_some_and(|nonce| Arc::ptr_eq(&nonce, &self.nonce))
            });
        if current {
            state.command_interrupt = None;
        }
        current
    }
}

#[derive(Debug)]
pub struct CommandCompletionCapture {
    pub connection_generation: u64,
    pub command: Arc<wezterm_term::CompletedCommand>,
}

pub type CommandCompletionResult = Result<CommandCompletionCapture, &'static str>;

#[derive(Debug)]
struct CompletionWait {
    target: RunningCommandTarget,
    reply: tokio::sync::oneshot::Sender<CommandCompletionResult>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SshConnectionPhase {
    Queued,
    Connecting,
    Reconnecting { attempt: Option<u32> },
    Connected,
    Disconnected,
    Stopped,
    Failed(Arc<str>),
}

impl SshConnectionPhase {
    pub(crate) fn as_json(&self) -> Value {
        use serde_json::json;

        match self {
            Self::Queued => json!({"phase": "queued"}),
            Self::Connecting => json!({"phase": "connecting"}),
            Self::Reconnecting { attempt } => json!({"phase": "reconnecting", "attempt": attempt}),
            Self::Connected => json!({"phase": "connected"}),
            Self::Disconnected => json!({"phase": "disconnected"}),
            Self::Stopped => json!({"phase": "stopped"}),
            Self::Failed(reason) => json!({"phase": "failed", "reason": reason.as_ref()}),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Batch {
    pub events: Vec<Event>,
    pub next: u64,
    pub oldest: u64,
    pub lost: bool,
}

#[derive(Debug, Default)]
enum PaneProcessState {
    #[default]
    Unavailable,
    Running,
    Exited(u64),
}

#[derive(Debug)]
pub(crate) struct PaneExit {
    pub exit_token: u64,
    pub restart_token: Option<u64>,
    pub ssh_connection: Option<SshConnectionPhase>,
}

#[derive(Debug, Default)]
struct State {
    next: u64,
    events: VecDeque<Event>,
    active_sftp_transfer: Option<TransferId>,
    recent_sftp_transfers: VecDeque<RecentSftpTransfer>,
    progress: wezterm_term::Progress,
    program_status: Option<Arc<wezterm_term::ProgramStatus>>,
    recording: Option<(u64, Arc<Path>)>,
    replay: Option<crate::session::ReplayState>,
    inbox: Option<(u64, Arc<str>, Inbox)>,
    kitty_notifications: kitty_notification::Notifications,
    command_watch: Option<CommandWatch>,
    running_output: Vec<running_output::Waiter>,
    command_completion: Vec<CompletionWait>,
    command_interrupt: Option<(RunningCommandTarget, std::sync::Weak<()>)>,
    group_run: Option<GroupRun>,
    group_prompt_pattern: Option<GroupPromptPattern>,
    group_serial: u64,
    next_prompt: u64,
    notify_long_commands: bool,
    ssh_connection_phase: Option<SshConnectionPhase>,
    connected_generation: Option<u64>,
    dotfile_sync: Option<Arc<DotfileSyncReport>>,
    tool_bundle_sync: Option<Arc<ToolBundleSyncReport>>,
    pane_process: PaneProcessState,
    ssh_attempts: ssh_attempt::Attempts,
    restart_token: Option<u64>,
    restart_closed: bool,
}

#[derive(Debug)]
struct CommandWatch {
    generation: u64,
    armed: Option<ManualCommandWatch>,
    running: Option<RunningCommand>,
    prompt: ShellPrompt,
    saw_markers: bool,
}

#[derive(Debug)]
enum ManualCommandWatch {
    CurrentOrNext,
    Running(RunningCommandTarget),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptToken {
    pub(crate) connection: u64,
    serial: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellPrompt {
    Unverified,
    Opening,
    Ready(PromptToken),
    Matched(PromptToken),
    InUse,
}

impl CommandWatch {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            armed: None,
            running: None,
            prompt: ShellPrompt::Unverified,
            saw_markers: false,
        }
    }

    fn reset(&mut self) {
        let saw_markers = self.saw_markers;
        *self = Self::new(self.generation);
        self.saw_markers = saw_markers;
    }
}

#[derive(Debug)]
struct RunningCommand {
    /// Keep a fixed-size identity even if the shell supplies a large OSC 133 aid.
    aid: Option<[u8; 32]>,
    started: Instant,
    linkable: bool,
}

fn application_id(aid: Option<&str>) -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    aid.map(|id| Sha256::digest(id.as_bytes()).into())
}

impl State {
    fn watch_state(&self) -> Option<bool> {
        self.command_watch
            .as_ref()
            .map(|watch| watch.armed.is_some())
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
    pub(crate) fn with_group_prompt_pattern(pattern: Option<GroupPromptPattern>) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                group_prompt_pattern: pattern,
                ..State::default()
            })),
            changed: Arc::new(Notify::new()),
        }
    }

    pub fn group_prompt_pattern(&self) -> Option<GroupPromptPattern> {
        self.state.lock().unwrap().group_prompt_pattern.clone()
    }

    pub(crate) fn change_group_prompt_pattern(
        &self,
        expected: Option<&GroupPromptPattern>,
        replacement: Option<&GroupPromptPattern>,
    ) -> Result<bool, &'static str> {
        let mut state = self.state.lock().unwrap();
        if state.group_prompt_pattern.as_ref() != expected {
            return Err("The pane's prompt pattern changed. Reopen prompt setup.");
        }
        if expected == replacement {
            return Ok(false);
        }
        state.group_prompt_pattern = replacement.cloned();
        if let Some(watch) = &mut state.command_watch
            && !watch.saw_markers
        {
            watch.prompt = ShellPrompt::Unverified;
        }
        if state
            .group_run
            .as_ref()
            .is_some_and(GroupRun::awaiting_prompt)
            && let Some(run) = state.group_run.take()
        {
            run.abandon("The prompt pattern changed before the shell reported a result.");
        }
        Ok(true)
    }

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
        self.append(state, kind, data, None);
        sequence
    }

    pub fn restart_token(&self) -> Option<u64> {
        self.state.lock().unwrap().restart_token
    }

    pub fn ssh_connection_phase(&self) -> Option<SshConnectionPhase> {
        self.state.lock().unwrap().ssh_connection_phase.clone()
    }

    pub(crate) fn set_ssh_connection_phase(&self, phase: SshConnectionPhase) {
        let mut state = self.state.lock().unwrap();
        let changed = Self::record_ssh_phase(&mut state, phase);
        drop(state);
        if changed {
            self.changed.notify_waiters();
        }
    }

    pub(crate) fn terminal_connected(&self, generation: u64, ssh: bool) {
        let mut state = self.state.lock().unwrap();
        if state.restart_closed {
            return;
        }
        let generation_changed = state.connected_generation.replace(generation) != Some(generation);
        let phase_changed =
            ssh && Self::record_ssh_phase(&mut state, SshConnectionPhase::Connected);
        drop(state);
        if generation_changed || phase_changed {
            self.changed.notify_waiters();
        }
    }

    fn record_ssh_phase(state: &mut State, phase: SshConnectionPhase) -> bool {
        let invalidated =
            phase != SshConnectionPhase::Connected && state.connected_generation.take().is_some();
        if state.ssh_connection_phase.as_ref() == Some(&phase) {
            return invalidated;
        }
        let data = phase.as_json();
        state.ssh_connection_phase = Some(phase);
        Self::append_locked(state, "ssh-connection", data, EventLinks::default());
        true
    }

    pub(crate) fn restart_exited(
        &self,
        input: &tokio::sync::mpsc::UnboundedSender<crate::session::Command>,
        token: u64,
    ) -> anyhow::Result<()> {
        self.queue_restart(input, token, None)
    }

    pub(crate) fn queue_restart(
        &self,
        input: &tokio::sync::mpsc::UnboundedSender<crate::session::Command>,
        token: u64,
        login_permits: Option<Arc<tokio::sync::Semaphore>>,
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        anyhow::ensure!(
            state.restart_token == Some(token),
            "Pane is running, unavailable, or the exit token has expired"
        );
        state.restart_token = None;
        let phase = if login_permits.is_some() {
            SshConnectionPhase::Queued
        } else {
            SshConnectionPhase::Reconnecting { attempt: None }
        };
        let sent = input.send(crate::session::Command::RestartExited(
            crate::session::ClaimedRestart {
                token,
                login_permits,
            },
        ));
        if sent.is_ok() {
            state.pane_process = PaneProcessState::Running;
            if state.ssh_connection_phase.is_some() {
                Self::record_ssh_phase(&mut state, phase);
            }
        }
        if let Some((_, _, inbox)) = &state.inbox {
            inbox.refresh();
        }
        drop(state);
        if sent.is_ok() {
            self.changed.notify_waiters();
        }
        sent.map_err(|_| anyhow::anyhow!("The terminal pane has closed"))
    }

    pub(crate) fn track_pane_process(&self) {
        self.state.lock().unwrap().pane_process = PaneProcessState::Running;
    }

    pub(crate) fn terminal_connection_is(&self, expected: u64) -> bool {
        self.state.lock().unwrap().connected_generation == Some(expected)
    }

    pub(crate) async fn terminal_connection_changed(&self, expected: u64) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !self.terminal_connection_is(expected) {
                return;
            }
            changed.await;
        }
    }

    pub(crate) fn pane_exited(
        &self,
        token: u64,
        ssh_phase: Option<SshConnectionPhase>,
        restartable: bool,
    ) {
        let mut state = self.state.lock().unwrap();
        if state.restart_closed {
            return;
        }
        state.pane_process = PaneProcessState::Exited(token);
        state.connected_generation = None;
        state.restart_token = restartable.then_some(token);
        if let Some(phase) = ssh_phase {
            Self::record_ssh_phase(&mut state, phase);
        }
        if !restartable {
            drop(state);
            self.changed.notify_waiters();
            return;
        }
        if let Some((_, _, inbox)) = &state.inbox {
            inbox.refresh();
        }
        self.append(
            state,
            "pane-exited",
            serde_json::json!({"exit_token":token}),
            None,
        );
    }

    pub(crate) fn close_restart(&self) {
        self.disable_restart(None);
    }

    pub(crate) async fn wait_pane_exit(
        &self,
        after_exit_token: u64,
        deadline: tokio::time::Instant,
    ) -> Result<PaneExit> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state.lock().unwrap();
                ensure!(!state.restart_closed, "pane is closed");
                match state.pane_process {
                    PaneProcessState::Unavailable => {
                        anyhow::bail!("pane does not run an SSH session or local program")
                    }
                    PaneProcessState::Exited(token) if token > after_exit_token => {
                        return Ok(PaneExit {
                            exit_token: token,
                            restart_token: state.restart_token,
                            ssh_connection: state.ssh_connection_phase.clone(),
                        });
                    }
                    PaneProcessState::Exited(_) if state.restart_token.is_none() => {
                        anyhow::bail!("pane has ended and cannot restart")
                    }
                    _ => {}
                }
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "wait-pane-exit timed out"
            );
            let _ = tokio::time::timeout_at(deadline, changed).await;
        }
    }

    pub(crate) fn close_if_exited(&self, token: u64) -> bool {
        self.disable_restart(Some(token))
    }

    fn disable_restart(&self, expected_exit: Option<u64>) -> bool {
        let mut state = self.state.lock().unwrap();
        if expected_exit.is_some() && state.restart_token != expected_exit {
            return false;
        }
        let closed = !std::mem::replace(&mut state.restart_closed, true);
        state.connected_generation = None;
        let changed = state.ssh_connection_phase.is_some()
            && Self::record_ssh_phase(&mut state, SshConnectionPhase::Disconnected);
        if state.restart_token.take().is_some()
            && let Some((_, _, inbox)) = &state.inbox
        {
            inbox.refresh();
        }
        drop(state);
        if closed || changed {
            self.changed.notify_waiters();
        }
        true
    }

    /// None while disconnected or on streams that may repaint historical bytes.
    pub fn command_watch(&self) -> Option<bool> {
        self.state.lock().unwrap().watch_state()
    }

    pub fn long_command_notifications(&self) -> Option<bool> {
        let state = self.state.lock().unwrap();
        state
            .command_watch
            .as_ref()
            .map(|_| state.notify_long_commands)
    }

    pub fn set_long_command_notifications(&self, connection: u64, enabled: bool) {
        let mut state = self.state.lock().unwrap();
        if state
            .command_watch
            .as_ref()
            .is_none_or(|watch| watch.generation != connection)
            || state.notify_long_commands == enabled
        {
            return;
        }
        state.notify_long_commands = enabled;
        if let Some((_, _, inbox)) = &state.inbox {
            inbox.refresh();
        }
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

    pub fn shell_prompt(&self) -> Option<ShellPrompt> {
        Some(self.state.lock().unwrap().command_watch.as_ref()?.prompt)
    }

    pub(crate) fn invalidate_shell_prompt(&self) {
        if let Some(watch) = &mut self.state.lock().unwrap().command_watch
            && matches!(
                watch.prompt,
                ShellPrompt::Opening | ShellPrompt::Ready(_) | ShellPrompt::Matched(_)
            )
        {
            watch.prompt = ShellPrompt::InUse;
        }
    }

    pub(crate) fn invalidate_matched_shell_prompt(&self) {
        if let Some(watch) = &mut self.state.lock().unwrap().command_watch
            && matches!(watch.prompt, ShellPrompt::Matched(_))
        {
            watch.prompt = ShellPrompt::InUse;
        }
    }

    pub(crate) fn observe_matched_shell_prompt(&self) {
        let mut state = self.state.lock().unwrap();
        let Some(watch) = &state.command_watch else {
            return;
        };
        if watch.saw_markers || matches!(watch.prompt, ShellPrompt::Matched(_)) {
            return;
        }
        let connection = watch.generation;
        state.next_prompt += 1;
        let token = PromptToken {
            connection,
            serial: state.next_prompt,
        };
        if let Some(watch) = &mut state.command_watch {
            watch.prompt = ShellPrompt::Matched(token);
        }
        if let Some(run) = state.group_run.take() {
            run.prompt_returned();
        }
    }

    pub(crate) fn claim_shell_prompt(&self, token: PromptToken) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(watch) = &mut state.command_watch else {
            return false;
        };
        if !matches!(watch.prompt, ShellPrompt::Ready(current) | ShellPrompt::Matched(current) if current == token)
        {
            return false;
        }
        watch.prompt = ShellPrompt::InUse;
        true
    }

    pub(crate) fn observe_group_command(
        &self,
        generation: u64,
        text: Arc<str>,
        status: tokio::sync::watch::Sender<GroupExecution>,
        transcript: GroupTranscriptHost,
    ) {
        let mut state = self.state.lock().unwrap();
        state.group_serial += 1;
        let target = GroupInterruptTarget {
            generation,
            serial: state.group_serial,
        };
        let run = GroupRun::new(target, text, status, transcript);
        if let Some(previous) = state.group_run.replace(run) {
            previous.abandon("The pane began a new group submission.");
        }
    }

    pub(crate) fn live_shell_output(
        &self,
        id: u64,
        label: Option<&str>,
        start_token: Option<u64>,
        output: wezterm_term::LiveCommandOutput<'_>,
    ) {
        let mut state = self.state.lock().unwrap();
        if let Some(run) = &mut state.group_run {
            run.live_output(id, label, &output);
        }
        state.running_output.retain(|wait| !wait.reply.is_closed());
        let mut index = 0;
        while let Some(wait) = state.running_output.get_mut(index) {
            if Some(wait.target.start_token) == start_token
                && let Some(result) = wait.observe(&output)
            {
                let wait = state.running_output.swap_remove(index);
                let _ = wait.reply.send(result);
            } else {
                index += 1;
            }
        }
    }

    pub(crate) fn watch_running_output(
        &self,
        target: RunningCommandTarget,
        pattern: regex::Regex,
        reply: tokio::sync::oneshot::Sender<RunningOutputResult>,
    ) {
        let mut state = self.state.lock().unwrap();
        state.running_output.retain(|wait| !wait.reply.is_closed());
        if !reply.is_closed() {
            state.running_output.push(running_output::Waiter {
                target,
                reply,
                pattern,
                cursor: Default::default(),
            });
        }
    }

    pub(crate) fn watch_command_completion(
        &self,
        target: RunningCommandTarget,
        reply: tokio::sync::oneshot::Sender<CommandCompletionResult>,
    ) {
        let mut state = self.state.lock().unwrap();
        state
            .command_completion
            .retain(|wait| !wait.reply.is_closed());
        if !reply.is_closed() {
            state
                .command_completion
                .push(CompletionWait { target, reply });
        }
    }

    pub(crate) fn prune_command_waits(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        state.running_output.retain(|wait| !wait.reply.is_closed());
        state
            .command_completion
            .retain(|wait| !wait.reply.is_closed());
        !state.running_output.is_empty()
    }

    pub(crate) fn arm_command_interrupt(
        &self,
        target: RunningCommandTarget,
    ) -> Option<CommandInterruptPermit> {
        let mut state = self.state.lock().unwrap();
        if state.command_watch.as_ref()?.generation != target.generation {
            return None;
        }
        let nonce = Arc::new(());
        state.command_interrupt = Some((target, Arc::downgrade(&nonce)));
        Some(CommandInterruptPermit {
            events: self.clone(),
            nonce,
        })
    }

    pub(crate) fn cancel_command_interrupt(&self) {
        self.state.lock().unwrap().command_interrupt = None;
    }

    pub(crate) fn group_command_line(&self, id: u64, label: Option<&str>) {
        if let Some(run) = &self.state.lock().unwrap().group_run {
            run.identify(id, label);
        }
    }

    pub(crate) fn group_command_running(&self, target: GroupInterruptTarget) -> bool {
        self.state
            .lock()
            .unwrap()
            .group_run
            .as_ref()
            .is_some_and(|run| run.is_running(target))
    }

    pub fn set_command_watch(&self, armed: bool) -> bool {
        let mut state = self.state.lock().unwrap();
        let previous = state.watch_state();
        let Some(watch) = &mut state.command_watch else {
            return false;
        };
        watch.armed = armed.then_some(ManualCommandWatch::CurrentOrNext);
        state.refresh_watch(previous);
        true
    }

    /// The pane can watch this capture if its semantic start is unambiguous.
    pub fn running_command_watch(&self, generation: u64) -> Option<bool> {
        let state = self.state.lock().unwrap();
        let watch = state.command_watch.as_ref().filter(|watch| {
            watch.generation == generation && watch.running.as_ref().is_some_and(|run| run.linkable)
        })?;
        Some(watch.armed.is_some())
    }

    // The session worker has just checked the target against its terminal parser.
    pub(crate) fn set_running_command_watch(&self, target: RunningCommandTarget, armed: bool) {
        let mut state = self.state.lock().unwrap();
        let previous = state.watch_state();
        if let Some(watch) = &mut state.command_watch
            && watch.generation == target.generation
            && watch.running.as_ref().is_some_and(|run| run.linkable)
        {
            watch.armed = armed.then_some(ManualCommandWatch::Running(target));
            state.refresh_watch(previous);
        }
    }

    pub(crate) fn retain_running_capture(&self, running: Option<RunningCommandTarget>) {
        let mut state = self.state.lock().unwrap();
        let previous = state.watch_state();
        if state
            .command_interrupt
            .as_ref()
            .is_some_and(|(target, _)| Some(*target) != running)
        {
            state.command_interrupt = None;
        }
        if let Some(watch) = &mut state.command_watch
            && matches!(watch.armed, Some(ManualCommandWatch::Running(target)) if Some(target) != running)
        {
            watch.armed = None;
            state.refresh_watch(previous);
        }
        for wait in state.running_output.extract_if(.., |wait| {
            wait.reply.is_closed() || Some(wait.target) != running
        }) {
            let _ = wait
                .reply
                .send(Err("command capture ended before an output match"));
        }
        for wait in state.command_completion.extract_if(.., |wait| {
            wait.reply.is_closed() || Some(wait.target) != running
        }) {
            let _ = wait
                .reply
                .send(Err("command capture ended before completion was reported"));
        }
    }

    pub(crate) fn command_stream(&self, generation: Option<u64>) {
        let mut state = self.state.lock().unwrap();
        state.command_interrupt = None;
        let previous = state.watch_state();
        if let Some(run) = state.group_run.take() {
            run.abandon("The connection closed before the shell reported a result.");
        }
        for wait in state.running_output.drain(..) {
            let _ = wait
                .reply
                .send(Err("shell connection changed before an output match"));
        }
        for wait in state.command_completion.drain(..) {
            let _ = wait
                .reply
                .send(Err("shell connection changed before command completion"));
        }
        state.command_watch = generation.map(CommandWatch::new);
        state.refresh_watch(previous);
    }

    pub(crate) fn reset_command_watch(&self) {
        let mut state = self.state.lock().unwrap();
        state.command_interrupt = None;
        let previous = state.watch_state();
        if let Some(run) = state.group_run.take() {
            run.abandon("The terminal reset before the shell reported a result.");
        }
        for wait in state.running_output.drain(..) {
            let _ = wait
                .reply
                .send(Err("terminal reset before an output match"));
        }
        for wait in state.command_completion.drain(..) {
            let _ = wait
                .reply
                .send(Err("terminal reset before command completion"));
        }
        if let Some(watch) = &mut state.command_watch {
            watch.reset();
        }
        state.refresh_watch(previous);
    }

    pub(crate) fn shell_command_started(&self, id: u64, start_token: u64, started_unix_ms: u64) {
        let state = self.state.lock().unwrap();
        let Some(watch) = &state.command_watch else {
            return;
        };
        let data = serde_json::json!({
            "id": id,
            "connection_generation": watch.generation,
            "start_token": start_token,
            "started_unix_ms": started_unix_ms,
        });
        self.append(state, "command-started", data, None);
    }

    pub(crate) fn semantic_prompt(
        &self,
        marker: &wezterm_term::FinalTermSemanticPrompt,
        started: Option<(u64, Option<&str>)>,
        completed: Option<wezterm_term::ShellCommandCompletion<'_>>,
    ) {
        use wezterm_term::{FinalTermPromptKind, FinalTermSemanticPrompt::*};
        let mut state = self.state.lock().unwrap();
        if completed.is_some()
            || matches!(
                marker,
                FreshLineAndStartPrompt { .. }
                    | StartPrompt(FinalTermPromptKind::Initial)
                    | MarkEndOfCommandWithFreshLine { .. }
                    | MarkEndOfPromptAndStartOfInputUntilNextMarker
                    | MarkEndOfPromptAndStartOfInputUntilEndOfLine
                    | MarkEndOfInputAndStartOfOutput { .. }
            )
        {
            state.command_interrupt = None;
        }
        let now = Instant::now();
        if let Some(run) = &mut state.group_run
            && !run.marker(
                marker,
                started,
                completed.as_ref().map(|ended| ended.command.as_ref()),
                now,
            )
        {
            state.group_run = None;
        }
        if let Some(ended) = completed.as_ref().filter(|ended| ended.had_output_start)
            && let Some(token) = ended.command.start_token
        {
            for wait in state.command_completion.extract_if(.., |wait| {
                wait.target.start_token == token && wait.target.id == ended.command.id
            }) {
                let _ = wait.reply.send(Ok(CommandCompletionCapture {
                    connection_generation: wait.target.generation,
                    command: ended.command.clone(),
                }));
            }
        }
        self.process_semantic_prompt(
            state,
            marker,
            completed
                .filter(|ended| ended.had_output_start)
                .map(|ended| ended.command.id),
            now,
        );
    }

    #[cfg(test)]
    fn semantic_prompt_at(
        &self,
        marker: &wezterm_term::FinalTermSemanticPrompt,
        completed: Option<u64>,
        now: Instant,
    ) {
        self.process_semantic_prompt(self.state.lock().unwrap(), marker, completed, now);
    }

    fn process_semantic_prompt(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        marker: &wezterm_term::FinalTermSemanticPrompt,
        completed: Option<u64>,
        now: Instant,
    ) {
        use wezterm_term::FinalTermSemanticPrompt::*;
        let previous = state.watch_state();
        let notify_long_commands = state.notify_long_commands;
        let input_marker = matches!(
            marker,
            MarkEndOfPromptAndStartOfInputUntilNextMarker
                | MarkEndOfPromptAndStartOfInputUntilEndOfLine
        );
        let serial = if input_marker
            && state
                .command_watch
                .as_ref()
                .is_some_and(|watch| watch.prompt == ShellPrompt::Opening)
        {
            state.next_prompt += 1;
            Some(state.next_prompt)
        } else {
            None
        };
        let Some(watch) = &mut state.command_watch else {
            return;
        };
        use wezterm_term::FinalTermPromptKind;
        if !matches!(
            marker,
            FreshLine | StartPrompt(FinalTermPromptKind::RightSide)
        ) {
            watch.saw_markers = true;
            if matches!(watch.prompt, ShellPrompt::Matched(_)) {
                watch.prompt = ShellPrompt::InUse;
            }
        }
        match marker {
            FreshLineAndStartPrompt { .. } | StartPrompt(FinalTermPromptKind::Initial) => {
                watch.prompt = ShellPrompt::Opening;
            }
            FreshLine | StartPrompt(FinalTermPromptKind::RightSide) => {}
            _ if !input_marker => watch.prompt = ShellPrompt::InUse,
            _ => {}
        }
        let (aid, exit_code) = match marker {
            MarkEndOfInputAndStartOfOutput { aid, .. } => {
                if matches!(watch.armed, Some(ManualCommandWatch::Running(_))) {
                    watch.armed = None;
                }
                if let Some(running) = &mut watch.running {
                    // The terminal's output capture restarts at each C marker.
                    // Retain the original watch, but its output is now ambiguous.
                    running.linkable = false;
                } else {
                    watch.running = Some(RunningCommand {
                        aid: application_id(aid.as_deref()),
                        started: now,
                        linkable: true,
                    });
                }
                state.refresh_watch(previous);
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
                if matches!(watch.armed, Some(ManualCommandWatch::Running(_))) {
                    watch.armed = None;
                }
                if watch
                    .running
                    .as_ref()
                    .is_some_and(|command| command.aid.is_none())
                {
                    watch.reset();
                }
                if let Some(serial) = serial {
                    watch.prompt = ShellPrompt::Ready(PromptToken {
                        connection: watch.generation,
                        serial,
                    });
                } else if matches!(watch.prompt, ShellPrompt::Ready(_)) {
                    // Redrawing a prompt suffix can leave partially edited input in place.
                    watch.prompt = ShellPrompt::InUse;
                }
                state.refresh_watch(previous);
                return;
            }
            StartPrompt(_) | FreshLine => return,
        };
        let Some(running) = watch
            .running
            .as_ref()
            .filter(|command| command.aid == application_id(aid.as_deref()))
        else {
            return;
        };
        let command = completed
            .filter(|_| running.linkable)
            .map(|id| CommandLink {
                generation: watch.generation,
                id,
            });
        let elapsed = now.saturating_duration_since(running.started);
        let automatic = notify_long_commands && elapsed >= LONG_COMMAND_THRESHOLD;
        watch.running = None;
        let manual = match watch.armed.take() {
            Some(ManualCommandWatch::Running(target)) => command
                .is_some_and(|link| link.generation == target.generation && link.id == target.id),
            Some(ManualCommandWatch::CurrentOrNext) => true,
            None => false,
        };
        if manual || automatic {
            let data = if automatic {
                serde_json::json!({"exit_code":exit_code,"duration_ms":elapsed.as_millis() as u64})
            } else {
                serde_json::json!({"exit_code":exit_code})
            };
            self.append(state, "command-finished", data, command);
        } else {
            state.refresh_watch(previous);
        }
    }

    /// Progress is current pane state. Frequent updates must not evict journal entries.
    pub fn progress(&self) -> wezterm_term::Progress {
        self.state.lock().unwrap().progress
    }

    /// Both indicators belong to the same pane state and change together.
    pub fn terminal_activity(
        &self,
    ) -> (
        wezterm_term::Progress,
        Option<Arc<wezterm_term::ProgramStatus>>,
    ) {
        let state = self.state.lock().unwrap();
        (state.progress, state.program_status.clone())
    }

    pub(crate) fn set_terminal_activity(
        &self,
        progress: wezterm_term::Progress,
        status: Option<Arc<wezterm_term::ProgramStatus>>,
    ) {
        let mut state = self.state.lock().unwrap();
        if state.progress != progress || state.program_status != status {
            state.progress = progress;
            state.program_status = status;
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
                if event.data["protocol"] != "kitty"
                    || state.is_kitty_notification_live(event.sequence)
                {
                    inbox.receive(pane, &label, event, false);
                }
            }
            state.inbox = Some((pane, label, inbox.clone()));
        }
    }

    pub fn push(&self, kind: &str, data: Value) {
        let data = Self::bounded_data(kind, data);
        self.append(self.state.lock().unwrap(), kind, data, None);
    }

    pub(crate) fn record_dotfile_sync(&self, report: DotfileSyncReport) {
        const KIND: &str = "dotfiles-sync";
        let data = (report.status != DotfileSyncStatus::NotRun)
            .then(|| Self::bounded_data(KIND, serde_json::json!(&report)));
        let mut state = self.state.lock().unwrap();
        state.dotfile_sync = Some(Arc::new(report));
        if let Some(data) = data {
            Self::append_locked(&mut state, KIND, data, EventLinks::default());
        }
        drop(state);
        self.changed.notify_waiters();
    }

    /// The login outcome is retained independently of the event journal.
    pub fn dotfile_sync(&self, generation: u64) -> Option<Arc<DotfileSyncReport>> {
        self.state
            .lock()
            .unwrap()
            .dotfile_sync
            .as_ref()
            .filter(|report| report.generation == generation)
            .cloned()
    }

    pub(crate) fn record_tool_bundle_sync(&self, report: ToolBundleSyncReport) {
        const KIND: &str = "tool-bundle-sync";
        let data = (report.status != ToolBundleSyncStatus::NotRun)
            .then(|| Self::bounded_data(KIND, serde_json::json!(&report)));
        let mut state = self.state.lock().unwrap();
        state.tool_bundle_sync = Some(Arc::new(report));
        if let Some(data) = data {
            Self::append_locked(&mut state, KIND, data, EventLinks::default());
        }
        drop(state);
        self.changed.notify_waiters();
    }

    /// The login outcome is retained independently of the event journal.
    pub fn tool_bundle_sync(&self, generation: u64) -> Option<Arc<ToolBundleSyncReport>> {
        self.state
            .lock()
            .unwrap()
            .tool_bundle_sync
            .as_ref()
            .filter(|report| report.generation == generation)
            .cloned()
    }

    pub fn sftp_transfer_finished(
        &self,
        transfer_id: TransferId,
        outcome: TransferOutcome,
        message: &str,
        saved_download: Option<SavedDownload>,
        details: Option<Arc<SftpTransferDetails>>,
        background: bool,
    ) {
        let message = inbox::sftp_transfer_text(message);
        let mut state = self.state.lock().unwrap();
        if state.active_sftp_transfer == Some(transfer_id) {
            state.active_sftp_transfer = None;
        }
        if state.recent_sftp_transfers.len() == RECENT_SFTP_TRANSFER_LIMIT {
            state.recent_sftp_transfers.pop_front();
        }
        let finished_at = Instant::now();
        let finished_unix_ms = Self::append_locked(
            &mut state,
            "sftp-transfer",
            serde_json::json!({ "transfer_id": transfer_id, "outcome": outcome, "message": message, "background": background }),
            EventLinks {
                saved_download: saved_download.clone(),
                ..EventLinks::default()
            },
        );
        state.recent_sftp_transfers.push_back(RecentSftpTransfer {
            id: transfer_id,
            finished_at,
            finished_unix_ms,
            outcome,
            message: Arc::from(message.as_str()),
            background,
            saved_download,
            details,
        });
        drop(state);
        self.changed.notify_waiters();
    }

    pub fn recent_sftp_transfers(&self) -> Vec<RecentSftpTransfer> {
        self.state
            .lock()
            .unwrap()
            .recent_sftp_transfers
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    pub fn sftp_transfer_started(&self, transfer_id: TransferId) {
        let mut state = self.state.lock().unwrap();
        if state.active_sftp_transfer != Some(transfer_id) {
            state.active_sftp_transfer = Some(transfer_id);
            drop(state);
            self.changed.notify_waiters();
        }
    }

    pub(crate) fn sftp_transfer_result(&self, id: TransferId) -> Option<Event> {
        Self::settled_sftp_transfer(&self.state.lock().unwrap(), id)
    }

    fn settled_sftp_transfer(state: &State, id: TransferId) -> Option<Event> {
        let token = id.to_string();
        state
            .events
            .iter()
            .rev()
            .find(|event| {
                event.kind == "sftp-transfer"
                    && event.data["transfer_id"].as_str() == Some(token.as_str())
            })
            .cloned()
    }

    pub(crate) async fn wait_sftp_transfer(
        &self,
        id: TransferId,
        deadline: tokio::time::Instant,
    ) -> Result<Event> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let state = self.state.lock().unwrap();
                if state.active_sftp_transfer != Some(id) {
                    return Self::settled_sftp_transfer(&state, id)
                        .context("SFTP transfer ID is unknown or its result has expired");
                }
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "SFTP transfer is still active after the wait timeout"
            );
            let _ = tokio::time::timeout_at(deadline, changed).await;
        }
    }

    fn bounded_data(kind: &str, data: Value) -> Value {
        let serialized = serde_json::to_string(&data).unwrap_or_default();
        if serialized.len() > 4096 {
            let mut preview = serde_json::json!({"truncated":true,"preview":serialized.chars().take(1024).collect::<String>()});
            if kind == "notification" {
                preview["summary"] = inbox::notification_text(&data).into();
                if let Some(protocol) = data.get("protocol") {
                    preview["protocol"] = protocol.clone();
                }
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

    fn append(
        &self,
        mut state: std::sync::MutexGuard<'_, State>,
        kind: &str,
        data: Value,
        command: Option<CommandLink>,
    ) {
        Self::append_locked(
            &mut state,
            kind,
            data,
            EventLinks {
                command,
                ..EventLinks::default()
            },
        );
        drop(state);
        self.changed.notify_waiters();
    }

    fn append_locked(state: &mut State, kind: &str, data: Value, links: EventLinks) -> u64 {
        state.next += 1;
        let sequence = state.next;
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let eviction = (kind == "notification" && data["protocol"] == "kitty")
            .then(|| state.kitty_notifications.eviction.clone());
        let event = Event {
            sequence,
            timestamp_ms,
            kind: kind.into(),
            data,
            command: links.command,
            saved_download: links.saved_download,
            eviction,
        };
        if let Some((pane, label, inbox)) = &state.inbox {
            inbox.receive(*pane, label, &event, true);
        }
        state.events.push_back(event);
        if state.events.len() > CAPACITY {
            state.events.pop_front();
        }
        timestamp_ms
    }
    pub(crate) fn cursor(&self) -> u64 {
        self.state.lock().unwrap().next
    }

    pub fn since(&self, cursor: u64) -> Batch {
        self.since_kind(cursor, None)
    }

    fn since_kind(&self, cursor: u64, kind: Option<&str>) -> Batch {
        let state = self.state.lock().unwrap();
        let oldest = state.events.front().map_or(state.next + 1, |e| e.sequence);
        let start = state
            .events
            .partition_point(|event| event.sequence <= cursor);
        Batch {
            events: state
                .events
                .range(start..)
                .filter(|event| kind.is_none_or(|kind| event.kind == kind))
                .cloned()
                .collect(),
            next: state.next,
            oldest,
            lost: cursor < oldest.saturating_sub(1),
        }
    }

    pub async fn wait(&self, cursor: u64, timeout: Duration) -> Batch {
        self.wait_kind(cursor, timeout, None).await
    }

    pub(crate) async fn wait_kind(
        &self,
        mut cursor: u64,
        timeout: Duration,
        kind: Option<&str>,
    ) -> Batch {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let batch = self.since_kind(cursor, kind);
            if !batch.events.is_empty() || batch.lost || tokio::time::Instant::now() >= deadline {
                return batch;
            }
            // Unmatched entries were already examined. Their later eviction
            // must not look like unread history was lost.
            cursor = cursor.max(batch.next);
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                return self.since_kind(cursor, kind);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotfile_sync_report_survives_journal_eviction_and_is_scoped_to_the_connection() {
        use crate::sftp::DotfileShell;

        let events = Events::default();
        events.record_dotfile_sync(DotfileSyncReport {
            generation: 1,
            shell: DotfileShell::Bash,
            status: DotfileSyncStatus::Skipped,
            detail: Some("Startup hook changed".into()),
        });
        let event = events.since(0).events.pop().unwrap();
        assert_eq!(event.kind, "dotfiles-sync");
        assert_eq!(
            event.data,
            serde_json::json!({
                "generation": 1, "status": "skipped", "detail": "Startup hook changed",
            })
        );
        for _ in 0..CAPACITY {
            events.push("title", Value::Null);
        }
        assert!(
            events
                .since_kind(0, Some("dotfiles-sync"))
                .events
                .is_empty()
        );
        let report = events.dotfile_sync(1).unwrap();
        assert_eq!(report.detail.as_deref(), Some("Startup hook changed"));
        assert_eq!(report.shell, DotfileShell::Bash);
        assert!(events.dotfile_sync(2).is_none());

        let cursor = events.cursor();
        events.record_dotfile_sync(DotfileSyncReport {
            generation: 2,
            shell: DotfileShell::Zsh,
            status: DotfileSyncStatus::NotRun,
            detail: Some("Alternate startup".into()),
        });
        assert!(events.dotfile_sync(1).is_none());
        let report = events.dotfile_sync(2).unwrap();
        assert_eq!(report.status, DotfileSyncStatus::NotRun);
        assert_eq!(report.shell, DotfileShell::Zsh);
        assert!(events.since(cursor).events.is_empty());
    }

    #[test]
    fn tool_sync_report_survives_journal_eviction_and_matches_only_its_connection() {
        let events = Events::default();
        events.record_tool_bundle_sync(ToolBundleSyncReport {
            generation: 1,
            bundle: "portable".into(),
            status: ToolBundleSyncStatus::Updated,
            path: Some(ToolPathSyncStatus::Skipped),
            detail: Some("Managed PATH conflict".into()),
        });
        let event = events.since(0).events.pop().unwrap();
        assert_eq!(event.kind, "tool-bundle-sync");
        assert_eq!(
            event.data,
            serde_json::json!({
                "generation": 1, "status": "updated", "path": "skipped", "detail": "Managed PATH conflict",
            })
        );
        for _ in 0..CAPACITY {
            events.push("title", Value::Null);
        }
        assert!(
            events
                .since(0)
                .events
                .iter()
                .all(|event| event.kind != "tool-bundle-sync")
        );
        assert_eq!(
            events.tool_bundle_sync(1).unwrap().detail.as_deref(),
            Some("Managed PATH conflict")
        );
        assert!(events.tool_bundle_sync(2).is_none());

        let cursor = events.cursor();
        events.record_tool_bundle_sync(ToolBundleSyncReport {
            generation: 2,
            bundle: "portable".into(),
            status: ToolBundleSyncStatus::NotRun,
            path: None,
            detail: Some("Alternate startup".into()),
        });
        assert!(events.tool_bundle_sync(1).is_none());
        assert_eq!(
            events.tool_bundle_sync(2).unwrap().status,
            ToolBundleSyncStatus::NotRun
        );
        assert!(events.since(cursor).events.is_empty());
    }

    #[test]
    fn recent_sftp_results_keep_the_latest_transfers_independently_of_other_events() {
        let events = Events::default();
        let last = RECENT_SFTP_TRANSFER_LIMIT as u64 + 1;
        let saved = SavedDownload::File(Arc::from(Path::new("download.txt")));
        for serial in 1..=last {
            let id = TransferId {
                pane: 5,
                generation: 2,
                serial,
            };
            events.sftp_transfer_started(id);
            if serial == last {
                assert_eq!(events.recent_sftp_transfers()[0].id.serial, last - 1);
            }
            events.sftp_transfer_finished(
                id,
                TransferOutcome::Completed,
                "downloaded",
                Some(saved.clone()),
                None,
                serial % 2 == 0,
            );
        }
        for _ in 0..CAPACITY {
            events.push("title", Value::Null);
        }
        let results = events.recent_sftp_transfers();
        assert_eq!(results.len(), RECENT_SFTP_TRANSFER_LIMIT);
        assert!(
            results
                .iter()
                .map(|result| result.id.serial)
                .eq((2..=last).rev())
        );
        assert_eq!(results[0].outcome, TransferOutcome::Completed);
        assert_eq!(&*results[0].message, "downloaded");
        assert_eq!(results[0].saved_download.as_ref(), Some(&saved));
    }

    #[tokio::test(start_paused = true)]
    async fn transfer_wait_returns_the_exact_retained_result_and_reports_eviction() {
        use tokio::time::Instant;
        let events = Events::default();
        let id = TransferId {
            pane: 5,
            generation: 2,
            serial: 1,
        };
        let other = TransferId { serial: 2, ..id };
        events.sftp_transfer_started(id);
        let deadline = Instant::now() + Duration::from_secs(5);
        let waiting = events.wait_sftp_transfer(id, deadline);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::ZERO, waiting.as_mut())
                .await
                .is_err()
        );
        events.sftp_transfer_finished(
            other,
            TransferOutcome::Completed,
            "other",
            None,
            None,
            false,
        );
        assert!(
            tokio::time::timeout(Duration::ZERO, waiting.as_mut())
                .await
                .is_err()
        );
        events.sftp_transfer_finished(
            id,
            TransferOutcome::Cancelled,
            "cancelled",
            None,
            None,
            false,
        );
        let result = waiting.await.unwrap();
        assert_eq!(result.data["outcome"], "cancelled");
        assert_eq!(
            events
                .wait_sftp_transfer(id, deadline)
                .await
                .unwrap()
                .sequence,
            result.sequence
        );
        for _ in 0..CAPACITY {
            events.push("title", Value::Null);
        }
        assert!(
            events
                .wait_sftp_transfer(id, deadline)
                .await
                .unwrap_err()
                .to_string()
                .contains("expired")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn transfer_wait_timeout_leaves_the_transfer_active() {
        use tokio::time::Instant;
        let events = Events::default();
        let id = TransferId {
            pane: 5,
            generation: 2,
            serial: 1,
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(
            events
                .wait_sftp_transfer(id, deadline)
                .await
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
        events.sftp_transfer_started(id);
        assert!(
            events
                .wait_sftp_transfer(id, deadline)
                .await
                .unwrap_err()
                .to_string()
                .contains("still active")
        );
        events.sftp_transfer_finished(
            id,
            TransferOutcome::Completed,
            "verified",
            None,
            None,
            false,
        );
        assert_eq!(
            events
                .wait_sftp_transfer(id, Instant::now())
                .await
                .unwrap()
                .data["outcome"],
            "completed"
        );
    }

    #[test]
    fn closing_an_exited_pane_and_restarting_it_claim_the_same_exit() {
        use SshConnectionPhase::*;
        let (commands, mut received) = tokio::sync::mpsc::unbounded_channel();
        let events = Events::default();
        events.pane_exited(4, Some(Stopped), true);

        assert!(!events.close_if_exited(3));
        assert!(events.restart_exited(&commands, 4).is_ok());
        assert!(!events.close_if_exited(4));
        assert!(matches!(
            received.try_recv(),
            Ok(crate::session::Command::RestartExited(_))
        ));

        events.pane_exited(5, Some(Stopped), true);
        assert!(events.close_if_exited(5));
        assert!(events.restart_exited(&commands, 5).is_err());
        assert!(received.try_recv().is_err());
        events.pane_exited(6, Some(Stopped), true);
        assert_eq!(
            events.connection_snapshot(),
            SshConnectionSnapshot {
                phase: Some(Disconnected),
                ..Default::default()
            }
        );
    }

    #[test]
    fn ssh_lifecycle_records_each_change_and_keeps_the_exit_token_with_the_phase() {
        use SshConnectionPhase::*;
        use serde_json::json;
        let events = Events::default();
        assert_eq!(
            events.connection_snapshot(),
            SshConnectionSnapshot::default()
        );
        events.set_ssh_connection_phase(Connecting);
        events.set_ssh_connection_phase(Connecting);
        events.set_ssh_connection_phase(Reconnecting { attempt: Some(2) });
        events.pane_exited(7, Some(Stopped), true);
        assert_eq!(
            events.connection_snapshot(),
            SshConnectionSnapshot {
                restart_token: Some(7),
                phase: Some(Stopped),
                ..Default::default()
            }
        );

        let (commands, _received) = tokio::sync::mpsc::unbounded_channel();
        events.restart_exited(&commands, 7).unwrap();
        assert_eq!(
            events.connection_snapshot(),
            SshConnectionSnapshot {
                phase: Some(Reconnecting { attempt: None }),
                ..Default::default()
            }
        );
        events.terminal_connected(1, true);
        events.set_ssh_connection_phase(Disconnected);
        events.pane_exited(8, Some(Failed("fixture failure".into())), true);
        let changes: Vec<_> = events
            .since(0)
            .events
            .into_iter()
            .filter(|event| event.kind == "ssh-connection")
            .map(|event| event.data)
            .collect();
        assert_eq!(
            changes,
            [
                json!({"phase":"connecting"}),
                json!({"phase":"reconnecting","attempt":2}),
                json!({"phase":"stopped"}),
                json!({"phase":"reconnecting","attempt":null}),
                json!({"phase":"connected"}),
                json!({"phase":"disconnected"}),
                json!({"phase":"failed","reason":"fixture failure"}),
            ],
        );
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_connection_watch_wakes_on_ssh_loss_and_local_exit() {
        use tokio::time::{sleep, timeout};

        for ssh in [false, true] {
            let events = Events::default();
            events.track_pane_process();
            events.terminal_connected(1, ssh);
            let ended = async {
                sleep(Duration::from_secs(1)).await;
                assert!(events.terminal_connection_is(1));
                if ssh {
                    events.set_ssh_connection_phase(SshConnectionPhase::Disconnected);
                } else {
                    events.pane_exited(1, None, true);
                }
            };
            let (disconnected, ()) = tokio::join!(
                timeout(
                    Duration::from_secs(2),
                    events.terminal_connection_changed(1)
                ),
                ended
            );
            disconnected.unwrap();
            assert!(!events.terminal_connection_is(1));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pane_exit_wait_returns_the_current_exit_and_follows_the_next_run() {
        use SshConnectionPhase::*;
        use tokio::time::{Instant, sleep};

        let events = Events::default();
        events.track_pane_process();
        events.terminal_connected(1, true);
        let deadline = Instant::now() + Duration::from_secs(10);
        let first_exit = async {
            sleep(Duration::from_secs(1)).await;
            events.set_ssh_connection_phase(Disconnected);
            sleep(Duration::from_secs(1)).await;
            events.set_ssh_connection_phase(Reconnecting { attempt: Some(1) });
            sleep(Duration::from_secs(1)).await;
            events.pane_exited(1, Some(Stopped), true);
        };
        let (exit, ()) = tokio::join!(events.wait_pane_exit(0, deadline), first_exit);
        let exit = exit.unwrap();
        assert_eq!(exit.exit_token, 1);
        assert_eq!(exit.restart_token, Some(1));
        assert_eq!(exit.ssh_connection, Some(Stopped));
        assert_eq!(
            events.wait_pane_exit(0, deadline).await.unwrap().exit_token,
            1
        );

        let (commands, _worker) = tokio::sync::mpsc::unbounded_channel();
        let next_exit = async {
            sleep(Duration::from_secs(1)).await;
            events.restart_exited(&commands, 1).unwrap();
            assert!(events.restart_token().is_none());
            events.terminal_connected(3, true);
            sleep(Duration::from_secs(1)).await;
            events.pane_exited(2, Some(Disconnected), true);
        };
        let (exit, ()) = tokio::join!(events.wait_pane_exit(1, deadline), next_exit);
        assert_eq!(exit.unwrap().restart_token, Some(2));
    }

    #[tokio::test(start_paused = true)]
    async fn pane_exit_wait_handles_nonrestartable_exits_and_unavailable_panes() {
        use tokio::time::Instant;

        let events = Events::default();
        let deadline = Instant::now() + Duration::from_secs(10);
        assert!(events.wait_pane_exit(0, deadline).await.is_err());
        events.track_pane_process();
        assert_eq!(
            events
                .wait_pane_exit(0, deadline)
                .await
                .unwrap_err()
                .to_string(),
            "wait-pane-exit timed out"
        );

        events.pane_exited(
            1,
            Some(SshConnectionPhase::Failed("remote command ended".into())),
            false,
        );
        let exit = events.wait_pane_exit(0, deadline).await.unwrap();
        assert_eq!(exit.exit_token, 1);
        assert_eq!(exit.restart_token, None);
        assert!(matches!(
            exit.ssh_connection,
            Some(SshConnectionPhase::Failed(_))
        ));
        assert_eq!(
            events
                .wait_pane_exit(1, deadline)
                .await
                .unwrap_err()
                .to_string(),
            "pane has ended and cannot restart"
        );
        events.close_restart();
        assert_eq!(
            events
                .wait_pane_exit(0, deadline)
                .await
                .unwrap_err()
                .to_string(),
            "pane is closed"
        );
    }

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
        assert!(!moved_to.acknowledge_wake().flash_taskbar);
        let last = pane.since(0).events.pop().unwrap();
        assert_eq!(last.kind, "recording-error");
        assert_eq!(last.data["error"], reason);
        assert_eq!(
            notice.target,
            NoticeTarget::Pane {
                pane: 11,
                sequence: last.sequence,
            }
        );
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
            pane.set_terminal_activity(Progress::Percentage(value), None);
        }
        assert_eq!(pane.progress(), Progress::Percentage(100));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(inbox.unread_count(), 0);
        assert!(pane.since(0).events.is_empty());
        inbox.acknowledge_wake();
        pane.set_terminal_activity(Progress::Percentage(100), None);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        pane.set_terminal_activity(Progress::None, None);
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

    #[tokio::test(start_paused = true)]
    async fn event_kind_wait_skips_unrelated_output_and_returns_matching_entries_in_order() {
        use std::{future::poll_fn, task::Poll};

        let events = Events::default();
        events.push("title", Value::Null);
        let wait = events.wait_kind(0, Duration::from_secs(1), Some("bell"));
        tokio::pin!(wait);
        assert!(poll_fn(|cx| Poll::Ready(wait.as_mut().poll(cx).is_pending())).await);
        events.push("notification", Value::Null);
        assert!(poll_fn(|cx| Poll::Ready(wait.as_mut().poll(cx).is_pending())).await);
        events.push("bell", Value::Null);
        events.push("title", Value::Null);
        events.push("bell", Value::Null);
        let batch = wait.await;
        assert!(!batch.lost);
        assert_eq!(batch.oldest, 1);
        assert_eq!(batch.next, 5);
        assert_eq!(
            batch
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [3, 5]
        );

        events.push("notification", Value::Null);
        let timeout = events
            .wait_kind(batch.next, Duration::from_secs(1), Some("bell"))
            .await;
        assert!(timeout.events.is_empty());
        assert!(!timeout.lost);
        assert_eq!(timeout.next, 6);
    }

    #[tokio::test(start_paused = true)]
    async fn event_kind_wait_reports_unread_evictions_only() {
        use std::{future::poll_fn, task::Poll};

        let events = Events::default();
        events.push("title", Value::Null);
        let wait = events.wait_kind(0, Duration::from_secs(1), Some("bell"));
        tokio::pin!(wait);
        assert!(poll_fn(|cx| Poll::Ready(wait.as_mut().poll(cx).is_pending())).await);
        for _ in 1..CAPACITY {
            events.push("title", Value::Null);
        }
        events.push("bell", Value::Null);
        let scanned = wait.await;
        assert!(!scanned.lost);
        assert_eq!(scanned.oldest, 2);
        assert_eq!(scanned.events[0].sequence, CAPACITY as u64 + 1);

        let missed = events
            .wait_kind(0, Duration::from_secs(1), Some("sound"))
            .await;
        assert!(missed.lost);
        assert!(missed.events.is_empty());
        assert_eq!(missed.next, scanned.next);
    }

    #[test]
    fn selected_running_command_watch_links_only_its_capture_and_fires_once() {
        use wezterm_term::FinalTermSemanticPrompt::{
            CommandStatus, MarkEndOfInputAndStartOfOutput,
        };
        let events = Events::default();
        let start = MarkEndOfInputAndStartOfOutput {
            aid: None,
            command: None,
        };
        let end = CommandStatus {
            aid: None,
            status: Some(7),
        };
        let now = Instant::now();
        let target = RunningCommandTarget {
            generation: 1,
            id: 3,
            start_token: 1,
        };
        events.command_stream(Some(1));
        events.semantic_prompt_at(&start, None, now);
        events.set_running_command_watch(target, true);
        assert_eq!(events.running_command_watch(1), Some(true));
        events.semantic_prompt_at(&end, Some(3), now);
        events.semantic_prompt_at(&start, None, now);
        events.semantic_prompt_at(&end, Some(4), now);
        let batch = events.since(0);
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].kind, "command-finished");
        assert_eq!(batch.events[0].data, serde_json::json!({"exit_code":7}));
        assert_eq!(
            batch.events[0].command,
            Some(CommandLink {
                generation: 1,
                id: 3
            })
        );
    }

    #[test]
    fn selected_running_command_watch_clears_on_replacement_or_disconnect() {
        use wezterm_term::FinalTermSemanticPrompt::{
            CommandStatus, MarkEndOfInputAndStartOfOutput,
        };
        let events = Events::default();
        let start = MarkEndOfInputAndStartOfOutput {
            aid: None,
            command: None,
        };
        let end = CommandStatus {
            aid: None,
            status: Some(0),
        };
        let now = Instant::now();
        let target = RunningCommandTarget {
            generation: 1,
            id: 3,
            start_token: 1,
        };
        events.command_stream(Some(1));
        events.semantic_prompt_at(&start, None, now);
        events.set_running_command_watch(target, true);
        assert_eq!(events.command_watch(), Some(true));
        events.semantic_prompt_at(&start, None, now);
        assert_eq!(events.command_watch(), Some(false));
        // A second C reuses the capture ID. It must never inherit the alert.
        events.semantic_prompt_at(&end, Some(3), now);

        events.semantic_prompt_at(&start, None, now);
        events.set_running_command_watch(target, true);
        assert_eq!(events.command_watch(), Some(true));
        events.command_stream(None);
        events.command_stream(Some(2));
        events.semantic_prompt_at(&start, None, now);
        events.set_running_command_watch(target, true);
        assert_eq!(events.command_watch(), Some(false));
        events.semantic_prompt_at(&end, Some(3), now);
        assert!(events.since(0).events.is_empty());
    }

    #[test]
    fn automatic_command_notifications_use_the_first_matching_start_and_deduplicate() {
        use wezterm_term::FinalTermSemanticPrompt::{
            CommandStatus, MarkEndOfInputAndStartOfOutput,
        };
        let events = Events::default();
        let start = MarkEndOfInputAndStartOfOutput {
            aid: Some("shell".into()),
            command: None,
        };
        let end = CommandStatus {
            aid: Some("shell".into()),
            status: Some(17),
        };
        let unrelated = CommandStatus {
            aid: Some("nested".into()),
            status: Some(5),
        };
        let now = Instant::now();
        let long = now + LONG_COMMAND_THRESHOLD;
        events.command_stream(Some(1));
        events.semantic_prompt_at(&start, None, now);
        events.semantic_prompt_at(&end, None, long);
        assert!(events.since(0).events.is_empty());

        events.set_long_command_notifications(1, true);
        events.semantic_prompt_at(&start, None, long);
        events.semantic_prompt_at(
            &end,
            None,
            long + LONG_COMMAND_THRESHOLD - Duration::from_millis(1),
        );
        assert!(events.since(0).events.is_empty());

        let next = long + LONG_COMMAND_THRESHOLD;
        events.semantic_prompt_at(&start, None, next);
        events.set_command_watch(true);
        events.semantic_prompt_at(&start, None, next + Duration::from_secs(8));
        events.semantic_prompt_at(&unrelated, None, next + LONG_COMMAND_THRESHOLD);
        assert!(events.since(0).events.is_empty());
        events.semantic_prompt_at(&end, None, next + LONG_COMMAND_THRESHOLD);
        events.semantic_prompt_at(&end, None, next + LONG_COMMAND_THRESHOLD);
        let batch = events.since(0);
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].kind, "command-finished");
        assert_eq!(
            batch.events[0].data,
            serde_json::json!({"exit_code":17,"duration_ms":10000})
        );
        assert_eq!(events.command_watch(), Some(false));

        let short = next + LONG_COMMAND_THRESHOLD;
        events.semantic_prompt_at(&start, None, short);
        events.set_command_watch(true);
        events.semantic_prompt_at(&end, None, short + Duration::from_secs(1));
        let batch = events.since(batch.next);
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].data, serde_json::json!({"exit_code":17}));
    }

    #[test]
    fn automatic_preference_survives_reset_and_reconnect_but_an_old_command_does_not() {
        use wezterm_term::FinalTermSemanticPrompt::{
            CommandStatus, MarkEndOfInputAndStartOfOutput,
        };
        let events = Events::default();
        let start = MarkEndOfInputAndStartOfOutput {
            aid: None,
            command: None,
        };
        let end = CommandStatus {
            aid: None,
            status: None,
        };
        let now = Instant::now();
        let long = now + LONG_COMMAND_THRESHOLD;
        events.command_stream(Some(1));
        events.set_long_command_notifications(1, true);
        events.semantic_prompt_at(&start, None, now);
        events.reset_command_watch();
        events.semantic_prompt_at(&end, None, long);
        assert!(events.since(0).events.is_empty());
        assert_eq!(events.long_command_notifications(), Some(true));

        events.semantic_prompt_at(&start, None, long);
        events.command_stream(None);
        assert_eq!(events.long_command_notifications(), None);
        events.command_stream(Some(2));
        events.set_long_command_notifications(1, false);
        assert_eq!(events.long_command_notifications(), Some(true));
        let next = long + LONG_COMMAND_THRESHOLD;
        events.semantic_prompt_at(&end, None, next);
        assert!(events.since(0).events.is_empty());
        events.semantic_prompt_at(&start, None, next);
        events.semantic_prompt_at(&end, None, next + LONG_COMMAND_THRESHOLD);
        assert_eq!(events.since(0).events.len(), 1);
        events.set_long_command_notifications(2, false);
        events.semantic_prompt_at(&start, None, next + LONG_COMMAND_THRESHOLD);
        events.semantic_prompt_at(&end, None, next + 2 * LONG_COMMAND_THRESHOLD);
        assert_eq!(events.since(0).events.len(), 1);
    }
}
