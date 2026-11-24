use super::*;
use std::{borrow::Cow, collections::BTreeSet};
use tokio::sync::{Semaphore, oneshot, oneshot::error::TryRecvError, watch};
use w11term::events::{
    CommandLink, GroupExecution, GroupInterruptTarget, GroupOutput, GroupTranscript, PromptToken,
    ShellPrompt, SshConnectionPhase,
};
use w11term::options::{
    SshGroupCommandInterval, SshGroupParallelLimit, SshGroupQueueExpiry, SshRosterDefaults,
    SshRosterOrder as RosterOrder,
};
use w11term::prompts::Question;
mod address;
mod alert_rule_presets;
mod alerted_send;
mod close_ended;
mod command_order;
mod command_queue;
mod compare;
mod completion;
mod connection_notice;
mod control_api;
mod disconnect;
mod dispatch_policy;
mod environment;
mod filter;
mod host_list_export;
mod import;
mod live_output;
mod membership;
mod output_recording;
mod parallel_limit;
mod progress;
mod prompt_setup;
mod queue_activity;
mod recent_commands;
mod recipient_sets;
mod reconnect;
mod recorded_send;
mod report;
mod retry;
mod reuse;
mod roster_commands;
mod roster_report;
mod run_search;
mod save_command;
mod saved;
mod transfer;

pub(super) use live_output::displayed_line;
pub use recipient_sets::{Action as RecipientSetAction, Offer as RecipientSetOffer};
pub(super) use reconnect::FailedLogin;
pub use reuse::Offer as LaunchReviewOffer;
pub use roster_commands::RosterCommand;

pub(super) const LABEL: &str = "SSH host group";
pub(super) const NEW_TAB_LABEL: &str = "New SSH group tab";
pub(super) const RECENT_COMMANDS_LABEL: &str = "Recent group commands…";
pub(super) const FIND_RUN_LABEL: &str = "Find group run…";
pub(super) use run_search::{
    WINDOW_OUTPUT_ALERT_SEARCH_LABEL, WINDOW_RUN_SEARCH_LABEL, WindowRunScope,
};
const REQUIRE_READY: &str = "Require all recipients ready";
const REVIEW_CHANGED: &str =
    "The command, hosts or prompts changed. Review them before sending again.";
const ACK_TIMEOUT: Duration = Duration::from_secs(10);
const SUBMISSION_LIMIT: usize = 8;

#[derive(Default)]
pub(super) struct HostGroup {
    saved: Box<saved::SavedUi>,
    roster: Option<Box<MemberRoster>>,
    selected: BTreeSet<u64>,
    ready_waiters: Vec<control_api::ReadinessWait>,
    filter: filter::Filter,
    roster_preferences: SshRosterDefaults,
    login_queue: Option<(usize, Arc<Semaphore>)>,
    roster_copy_feedback: String,
    draft: String,
    canary: Option<u64>,
    notify_canary: bool,
    preview_command_order: bool,
    manual_command_order: Vec<u64>,
    previous_draft: Option<DraftBackup>,
    command_editor: ElementRef<TextBox>,
    focus_loaded_command: bool,
    command_save: save_command::State,
    recorded_send: recorded_send::State,
    alerted_send: alerted_send::State,
    submissions: VecDeque<Submission>,
    recent_commands: recent_commands::RecentCommands,
    viewing: u64,
    next_submission: u64,
    copy_feedback: String,
    result_filter: progress::Filter,
    recipient_search: filter::Filter,
    result_order: progress::ResultOrder,
    live_output: Option<Box<live_output::State>>,
    comparison: compare::State,
    import: import::State,
    host_list_export: Box<host_list_export::State>,
    reconnect_feedback: String,
    connection_notice: connection_notice::State,
    close_ended: Box<close_ended::State>,
    disconnect: Box<disconnect::State>,
    retry_feedback: String,
    prompt_setup: prompt_setup::State,
    transfer: transfer::State,
    environment: environment::State,
}

#[derive(Default)]
struct MemberRoster {
    panes: BTreeSet<u64>,
    show_window: bool,
    membership: membership::State,
}

enum PaneLaunch {
    Select,
    Add,
}

struct LaunchedGroupPane {
    pane_id: u64,
    tab_id: TabId,
}

pub(super) struct WindowGroup {
    pub(super) name: String,
    pub(super) profiles: Vec<String>,
    pub(super) members: Vec<(String, SshOptions)>,
    pub(super) login_limit: Option<usize>,
}

struct DraftBackup {
    text: String,
    scope: Option<DraftScope>,
}

struct DraftScope {
    selected: BTreeSet<u64>,
    filter: filter::Filter,
}

struct Submission {
    id: u64,
    content: SubmissionContent,
    results: Vec<Delivery>,
    transcript: GroupTranscript,
    recording: output_recording::State,
    control_recording: Option<output_recording::ControlRecording>,
    saved_recordings: output_recording::saved::State,
    command_queue: Option<command_queue::Queue>,
    dispatch_policy: Option<dispatch_policy::Policy>,
    show_queue_activity: bool,
    control_waiters: Vec<control_api::ControlReply>,
    output_lifetime: Option<watch::Sender<()>>,
}

impl Drop for Submission {
    fn drop(&mut self) {
        self.transcript.cancel_output_alert();
    }
}

#[derive(Clone)]
pub struct CommandOffer {
    text: Arc<str>,
    targets: Vec<Target>,
    limit: SshGroupParallelLimit,
    interval: SshGroupCommandInterval,
    queue_expiry: SshGroupQueueExpiry,
    stop_queue_on_failure: bool,
    canary: Option<u64>,
    notify_canary: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SubmissionOrigin {
    Interactive,
    Control,
}

enum SubmissionContent {
    Command(Arc<str>),
    Environment(BTreeMap<u64, Arc<str>>),
}

impl SubmissionContent {
    fn label(&self) -> &str {
        match self {
            Self::Command(text) => text,
            Self::Environment(_) => environment::LABEL,
        }
    }

    fn command(&self) -> Option<&str> {
        match self {
            Self::Command(text) => Some(text),
            Self::Environment(_) => None,
        }
    }

    fn host_command(&self, pane: u64) -> Option<&str> {
        match self {
            Self::Command(_) => None,
            Self::Environment(commands) => commands.get(&pane).map(AsRef::as_ref),
        }
    }
}

impl HostGroup {
    pub(super) fn deactivated(&mut self) {
        self.import.editor_unmounted();
        self.focus_loaded_command = false;
    }
}

#[derive(Clone)]
pub struct Target {
    pane: u64,
    name: String,
    names: filter::Names,
    profile: Option<String>,
    connected: bool,
    state: TargetState,
}

impl Target {
    fn pending_prompt(&self) -> Option<(u64, u64)> {
        Some((self.pane, self.state.authentication_prompt()?))
    }

    fn status_caption(&self) -> Cow<'static, str> {
        let state = self.state.label();
        if self.connected {
            format!("Connected · {state}").into()
        } else {
            state
        }
    }
}

#[derive(Clone, Copy)]
enum TargetState {
    Offline,
    Failed(Option<ExitedPane>),
    Stopped(Option<ExitedPane>),
    Queued,
    Connecting,
    Reconnecting(Option<u32>),
    Restartable(ExitedPane),
    HostKey(u64),
    Authentication(Option<u64>),
    ReadOnly,
    Alternate,
    Unverified,
    InUse,
    Ready(PromptToken),
    Matched(PromptToken),
}

#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RosterStatus {
    #[default]
    All,
    Ready,
    Waiting,
    Unverified,
    Queued,
    Connecting,
    Reconnecting,
    Disconnected,
    Failed,
    Stopped,
    HostKey,
    Authentication,
    ReadOnly,
    Alternate,
}

impl RosterStatus {
    const OPTIONS: [Self; 14] = [
        Self::All,
        Self::Ready,
        Self::Waiting,
        Self::Unverified,
        Self::Queued,
        Self::Connecting,
        Self::Reconnecting,
        Self::Disconnected,
        Self::Failed,
        Self::Stopped,
        Self::HostKey,
        Self::Authentication,
        Self::ReadOnly,
        Self::Alternate,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All host states",
            Self::Ready => "Ready",
            Self::Waiting => "Waiting for a fresh prompt",
            Self::Unverified => "Prompt unverified",
            Self::Queued => "Waiting to connect",
            Self::Connecting => "Connecting",
            Self::Reconnecting => "Reconnecting",
            Self::Disconnected => "Disconnected",
            Self::Failed => "Connection failed",
            Self::Stopped => "Connection stopped",
            Self::HostKey => "Host key verification required",
            Self::Authentication => "Authentication required",
            Self::ReadOnly => "Read-only",
            Self::Alternate => "Alternate screen",
        }
    }
}

const ROSTER_ORDERS: [(RosterOrder, &str); 4] = [
    (RosterOrder::Tabs, "Tab order"),
    (RosterOrder::Label, "Host label"),
    (RosterOrder::CheckedFirst, "Checked first"),
    (RosterOrder::AttentionFirst, "Needs attention first"),
];

fn sort_roster(order: RosterOrder, targets: &mut [&Target], selected: &BTreeSet<u64>) {
    match order {
        RosterOrder::Tabs => {}
        RosterOrder::Label => {
            targets.sort_by_cached_key(|target| target.names.label.to_lowercase())
        }
        RosterOrder::CheckedFirst => targets.sort_by_key(|target| !selected.contains(&target.pane)),
        RosterOrder::AttentionFirst => targets.sort_by_key(|target| match target.state {
            TargetState::Ready(_) | TargetState::Matched(_) => 2,
            TargetState::InUse | TargetState::Connecting | TargetState::Reconnecting(_) => 1,
            _ => 0,
        }),
    }
}

impl TargetState {
    fn is_authenticating(self) -> bool {
        matches!(self, Self::HostKey(_) | Self::Authentication(_))
    }

    fn authentication_prompt(self) -> Option<u64> {
        match self {
            Self::HostKey(id) | Self::Authentication(Some(id)) => Some(id),
            _ => None,
        }
    }

    fn is_ready(self) -> bool {
        self.prompt().is_some()
    }

    fn prompt(self) -> Option<PromptToken> {
        match self {
            Self::Ready(prompt) | Self::Matched(prompt) => Some(prompt),
            _ => None,
        }
    }

    fn status(self) -> RosterStatus {
        match self {
            Self::Offline | Self::Restartable(_) => RosterStatus::Disconnected,
            Self::Failed(_) => RosterStatus::Failed,
            Self::Stopped(_) => RosterStatus::Stopped,
            Self::Queued => RosterStatus::Queued,
            Self::Connecting => RosterStatus::Connecting,
            Self::Reconnecting(_) => RosterStatus::Reconnecting,
            Self::HostKey(_) => RosterStatus::HostKey,
            Self::Authentication(_) => RosterStatus::Authentication,
            Self::ReadOnly => RosterStatus::ReadOnly,
            Self::Alternate => RosterStatus::Alternate,
            Self::Unverified => RosterStatus::Unverified,
            Self::InUse => RosterStatus::Waiting,
            Self::Ready(_) | Self::Matched(_) => RosterStatus::Ready,
        }
    }

    fn label(self) -> Cow<'static, str> {
        match self {
            Self::Restartable(_) => "Disconnected · reconnect available".into(),
            Self::Failed(Some(_)) => "Connection failed · reconnect available".into(),
            Self::Stopped(Some(_)) => "Connection stopped · reconnect available".into(),
            Self::Authentication(None) => "Authentication in progress".into(),
            Self::Matched(_) => "Ready · prompt match".into(),
            Self::Reconnecting(Some(attempt)) => format!("Reconnecting · attempt {attempt}").into(),
            _ => self.status().label().into(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum ShownSelection {
    All,
    None,
    Ready,
}

impl ShownSelection {
    fn includes(self, state: TargetState) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Ready => state.is_ready(),
        }
    }
}

struct Delivery {
    target: Target,
    status: DeliveryStatus,
    execution: Option<watch::Receiver<GroupExecution>>,
    interrupt: Option<DeliveryStatus>,
}

enum DeliveryStatus {
    Queued(command_queue::Waiting),
    Pending(Instant, oneshot::Receiver<Result<(), String>>),
    Sent,
    Rejected(String),
    Skipped,
    Unconfirmed,
}

impl DeliveryStatus {
    fn poll(&mut self) {
        let Self::Pending(started, reply) = self else {
            return;
        };
        *self = match reply.try_recv() {
            Ok(Ok(())) => Self::Sent,
            Ok(Err(error)) => Self::Rejected(error),
            Err(TryRecvError::Empty) if started.elapsed() < ACK_TIMEOUT => return,
            Err(_) => Self::Unconfirmed,
        };
    }
}

impl Delivery {
    fn poll(&mut self) {
        self.status.poll();
        if matches!(
            self.status,
            DeliveryStatus::Rejected(_) | DeliveryStatus::Unconfirmed
        ) {
            self.execution = None;
        }
        if let Some(interrupt) = &mut self.interrupt {
            interrupt.poll();
        }
    }

    fn interrupt_target(&self) -> Option<GroupInterruptTarget> {
        if !matches!(self.status, DeliveryStatus::Sent)
            || matches!(self.interrupt, Some(DeliveryStatus::Pending(..)))
        {
            return None;
        }
        match *self.execution.as_ref()?.borrow() {
            GroupExecution::Running { target, .. } => Some(target),
            _ => None,
        }
    }

    fn description(&self) -> String {
        let state = match &self.status {
            DeliveryStatus::Queued(_) => "Queued".into(),
            DeliveryStatus::Pending(..) => "Sending".to_owned(),
            DeliveryStatus::Sent => "Sent to SSH channel".to_owned(),
            DeliveryStatus::Rejected(reason) => format!("Not sent · {reason}"),
            DeliveryStatus::Skipped => "Skipped before sending".to_owned(),
            DeliveryStatus::Unconfirmed => "Delivery unconfirmed".to_owned(),
        };
        format!("{} · {state}", self.target.name)
    }

    fn execution(&self) -> Option<GroupExecution> {
        if !matches!(self.status, DeliveryStatus::Sent) {
            return None;
        }
        Some(self.execution.as_ref()?.borrow().clone())
    }

    fn interrupt_caption(&self) -> Option<Cow<'_, str>> {
        Some(match self.interrupt.as_ref()? {
            DeliveryStatus::Queued(_) | DeliveryStatus::Skipped => return None,
            DeliveryStatus::Pending(..) => "Sending Ctrl+C".into(),
            DeliveryStatus::Sent => "Ctrl+C sent".into(),
            DeliveryStatus::Rejected(reason) => format!("Ctrl+C withheld · {reason}").into(),
            DeliveryStatus::Unconfirmed => "Ctrl+C delivery unconfirmed".into(),
        })
    }
}

fn execution_caption(result: &GroupExecution) -> Cow<'_, str> {
    match result {
        GroupExecution::AwaitingMarkers => "Waiting for shell markers".into(),
        GroupExecution::AwaitingPrompt => "Waiting for a matching prompt".into(),
        GroupExecution::Running { .. } => "Running".into(),
        GroupExecution::PromptReturned => "Prompt returned · exit status unavailable".into(),
        GroupExecution::Unavailable(reason) => format!("Result unavailable · {reason}").into(),
        GroupExecution::Finished(result) => result.exit_code.map_or_else(
            || "Completed · exit status unavailable".into(),
            |code| format!("Exit {code}").into(),
        ),
    }
}

fn execution_time_caption(result: &GroupExecution, now: Instant) -> Option<String> {
    let time = result.shell_time(now)?;
    Some(match result {
        GroupExecution::Running { .. } => format!("Running for {} s", time.as_secs()),
        _ if time < Duration::from_secs(1) => format!("Shell time · {} ms", time.as_millis()),
        _ => format!("Shell time · {:.2} s", time.as_secs_f64()),
    })
}

fn roster_state_summary(matching: &[&Target], shown: usize) -> Option<String> {
    let total = matching.len();
    if total == 0 {
        return None;
    }
    let mut counts = BTreeMap::<RosterStatus, usize>::new();
    for target in matching {
        *counts.entry(target.state.status()).or_default() += 1;
    }
    let hosts = if total == 1 { "host" } else { "hosts" };
    let scope = if shown == total {
        format!("{total} matching {hosts}")
    } else {
        format!("Showing {shown} of {total} matching {hosts}")
    };
    let breakdown = RosterStatus::OPTIONS.into_iter().filter_map(|status| {
        let count = counts.get(&status)?;
        Some(format!("{} ({count})", status.label()))
    });
    Some(
        std::iter::once(scope)
            .chain(breakdown)
            .collect::<Vec<_>>()
            .join(" · "),
    )
}

impl HostGroup {
    fn includes_pane(&self, pane: u64) -> bool {
        self.roster
            .as_ref()
            .is_none_or(|roster| roster.show_window || roster.panes.contains(&pane))
    }

    fn show_window_roster(&mut self, show: bool) {
        if let Some(roster) = &mut self.roster
            && roster.show_window != show
        {
            roster.show_window = show;
            if !show {
                self.selected.retain(|pane| roster.panes.contains(pane));
            }
        }
    }

    fn scoped_targets<'a>(&self, targets: &'a [Target]) -> impl Iterator<Item = &'a Target> {
        targets
            .iter()
            .filter(move |target| self.includes_pane(target.pane))
    }

    fn shown<'a>(&self, targets: &'a [Target]) -> impl Iterator<Item = &'a Target> {
        self.scoped_targets(targets)
            .filter(move |target| self.filter.matches(target))
    }

    fn matching_query<'a>(&self, targets: &'a [Target]) -> impl Iterator<Item = &'a Target> {
        self.scoped_targets(targets)
            .filter(move |target| self.filter.matches_query(target))
    }

    fn sort_shown(&self, targets: &mut [&Target]) {
        sort_roster(self.roster_preferences.order, targets, &self.selected);
    }

    fn selected_shown(&self, targets: &[Target]) -> BTreeSet<u64> {
        self.shown(targets)
            .map(|target| target.pane)
            .filter(|pane| self.selected.contains(pane))
            .collect()
    }

    fn orders_commands(&self) -> bool {
        self.roster_preferences
            .command_limit
            .concurrency()
            .is_some()
            || self.roster_preferences.command_interval != SshGroupCommandInterval::None
            || self.roster_preferences.command_queue_expiry != SshGroupQueueExpiry::Never
    }

    fn readiness_allows_send(&self, ready: usize, total: usize) -> bool {
        ready > 0 && (!self.roster_preferences.require_all_ready || ready == total)
    }

    fn sending(&self) -> bool {
        self.submissions.back().is_some_and(|submission| {
            submission.results.iter().any(|item| {
                matches!(
                    item.status,
                    DeliveryStatus::Queued(_) | DeliveryStatus::Pending(..)
                )
            })
        })
    }

    fn command_offer(
        &self,
        text: Arc<str>,
        targets: Vec<Target>,
        canary: Option<u64>,
    ) -> CommandOffer {
        CommandOffer {
            text,
            targets,
            limit: self.roster_preferences.command_limit,
            interval: self.roster_preferences.command_interval,
            queue_expiry: self.roster_preferences.command_queue_expiry,
            stop_queue_on_failure: self.roster_preferences.stop_command_queue_on_failure,
            canary,
            notify_canary: canary.is_some() && self.notify_canary,
        }
    }

    fn submit(
        &mut self,
        content: SubmissionContent,
        results: Vec<Delivery>,
        transcript: GroupTranscript,
    ) -> &mut Submission {
        self.retain_submission(content, results, transcript, SubmissionOrigin::Interactive)
    }

    fn retain_submission(
        &mut self,
        content: SubmissionContent,
        results: Vec<Delivery>,
        transcript: GroupTranscript,
        origin: SubmissionOrigin,
    ) -> &mut Submission {
        transcript.finish_dispatch();
        self.next_submission += 1;
        let id = self.next_submission;
        if origin == SubmissionOrigin::Interactive {
            self.preview_command_order = false;
            self.viewing = id;
            self.result_filter = progress::Filter::All;
            self.recipient_search.clear();
            self.copy_feedback.clear();
            self.retry_feedback.clear();
        }
        if let SubmissionContent::Command(command) = &content {
            self.recent_commands.record(id, command.clone());
            if origin == SubmissionOrigin::Interactive {
                self.previous_draft = None;
            }
        }
        self.submissions.push_back(Submission {
            id,
            content,
            results,
            transcript,
            recording: Default::default(),
            control_recording: None,
            saved_recordings: Default::default(),
            command_queue: None,
            dispatch_policy: None,
            show_queue_activity: false,
            control_waiters: Vec::new(),
            output_lifetime: None,
        });
        if self.submissions.len() > SUBMISSION_LIMIT
            && self
                .submissions
                .pop_front()
                .is_some_and(|old| old.id == self.viewing)
        {
            self.viewing = self.submissions[0].id;
            self.result_filter = progress::Filter::All;
            self.copy_feedback.clear();
            self.retry_feedback.clear();
        }
        self.submissions
            .back_mut()
            .expect("the new submission is retained")
    }

    fn viewed_submission(&self, id: u64) -> Option<&Submission> {
        if self.viewing != id {
            return None;
        }
        self.submissions
            .iter()
            .find(|submission| submission.id == id)
    }

    fn matching_submissions(&self, query: &str) -> impl Iterator<Item = &Submission> {
        let needle = query.trim().to_lowercase();
        let run_id = needle
            .strip_prefix('#')
            .and_then(|value| value.parse::<u64>().ok());
        self.submissions.iter().rev().filter(move |run| {
            run_id.map_or_else(
                || needle.is_empty() || run.content.label().to_lowercase().contains(&needle),
                |id| run.id == id,
            )
        })
    }

    fn load_command(&mut self, id: u64) -> bool {
        let Some(command) = self
            .viewed_submission(id)
            .and_then(|item| item.content.command())
        else {
            return false;
        };
        self.load_draft(command.to_owned());
        true
    }

    fn load_draft(&mut self, command: String) {
        if self.draft != command {
            let previous = std::mem::replace(&mut self.draft, command);
            self.previous_draft.get_or_insert(DraftBackup {
                text: previous,
                scope: None,
            });
        }
    }

    fn load_scoped_draft(&mut self, command: String, panes: BTreeSet<u64>) {
        let previous = std::mem::replace(&mut self.draft, command);
        let selected = std::mem::replace(&mut self.selected, panes);
        let backup = self.previous_draft.get_or_insert(DraftBackup {
            text: previous,
            scope: None,
        });
        backup.scope.get_or_insert_with(|| DraftScope {
            selected,
            filter: self.filter.clone(),
        });
        self.retry_feedback.clear();
    }

    fn restore_draft(&mut self) -> bool {
        let Some(previous) = self.previous_draft.take() else {
            return false;
        };
        self.draft = previous.text;
        if let Some(scope) = previous.scope {
            self.selected = scope.selected;
            self.filter = scope.filter;
        }
        self.retry_feedback.clear();
        true
    }
}

pub enum Action {
    Open,
    NewTab,
    OpenNamedGroup {
        name: String,
        members: Vec<String>,
    },
    ReviewNamedGroup(LaunchReviewOffer),
    ControlRecordingPrepared(Box<control_api::PreparedControlRecording>),
    Saved(TabId, saved::Action),
    Tick,
    Edit(TabId, String),
    Filter(TabId, String),
    FilterWildcards(TabId, bool),
    FilterStatus(TabId, RosterStatus),
    OrderRoster(TabId, RosterOrder),
    ShowLastOutput(TabId, bool),
    ShowTerminalLine(TabId, bool),
    RequireAllReady(TabId, bool),
    CopyRoster(TabId, BTreeSet<u64>),
    ShowWindowRoster(TabId, bool),
    Membership(TabId, membership::Action),
    RecipientSets(TabId, recipient_sets::Action),
    Reuse(TabId, reuse::Action),
    Select(TabId, u64, bool),
    SelectShown(TabId, BTreeSet<u64>, ShownSelection),
    Visit(TabId, u64),
    VisitPrompt(TabId, u64, u64),
    FindHost(TabId),
    ViewSubmission(TabId, u64),
    OpenRunPicker(TabId),
    NotifyCompletion(TabId, u64, bool),
    CancelOutputAlert(TabId, u64),
    LiveOutput(TabId, u64, live_output::Action),
    RecordOutput(TabId, u64, output_recording::Action),
    RecordedSend(TabId, recorded_send::Action),
    AlertedSend(TabId, alerted_send::Action),
    Compare(TabId, u64, compare::Action),
    Import(TabId, import::Action),
    HostListExport(TabId, host_list_export::Action),
    Reconnect(TabId, reconnect::Request),
    StopPending(TabId, Vec<reconnect::Attempt>),
    ConnectionNotice(TabId, connection_notice::Action),
    CloseEnded(TabId, close_ended::Action),
    Disconnect(TabId, disconnect::Action),
    FilterResults(TabId, u64, progress::Filter),
    SearchRecipients(TabId, u64, String),
    OrderResults(TabId, u64, progress::ResultOrder),
    LoadCommand(TabId, u64),
    PrepareRetry(TabId, u64, BTreeSet<u64>),
    PromptSetup(TabId, prompt_setup::Action),
    Transfer(TabId, transfer::Action),
    Environment(TabId, environment::Action),
    OpenSavedCommands(TabId),
    OpenRecentCommands(TabId),
    FocusLoadedCommand(TabId),
    LoadSavedCommand(TabId, w11term::saved_commands::Entry),
    LoadRecentCommand(TabId, u64),
    SaveCommand(TabId, save_command::Action),
    RestoreDraft(TabId),
    CopyResults(TabId, u64, Option<Arc<[u64]>>),
    SaveResults(
        TabId,
        u64,
        text_export::GroupReportFormat,
        Option<Arc<[u64]>>,
    ),
    ViewOutput(TabId, u64, CommandLink),
    Interrupt(TabId, u64, Vec<(u64, GroupInterruptTarget)>),
    Send(TabId, CommandOffer),
    CommandLimit(TabId, SshGroupParallelLimit),
    CommandInterval(TabId, SshGroupCommandInterval),
    CommandQueueExpiry(TabId, SshGroupQueueExpiry),
    CommandCanary(TabId, Option<u64>),
    NotifyCanary(TabId, bool),
    PreviewCommandOrder(TabId, bool),
    EditCommandOrder(TabId, command_order::Edit),
    StopCommandQueueOnFailure(TabId, bool),
    CommandQueue(TabId, u64, command_queue::Action),
    ShowQueueActivity(TabId, u64, bool),
}

impl TerminalApp {
    pub(super) fn group_run_choices(
        &self,
        group: TabId,
        query: &str,
    ) -> impl Iterator<Item = (u64, &str)> {
        let page = match self.tabs.get(group).map(|tab| &tab.page) {
            Some(Page::HostGroup(page)) if self.host_group_command_target() == Some(group) => {
                Some(page)
            }
            _ => None,
        };
        page.into_iter().flat_map(move |page| {
            page.matching_submissions(query)
                .map(|run| (run.id, run.content.label()))
        })
    }

    fn recent_host_group_tab(&self) -> Option<TabId> {
        self.mru_groups
            .iter()
            .copied()
            .chain(self.tabs.entries().iter().map(|entry| entry.id))
            .find(|&id| {
                self.tabs
                    .get(id)
                    .is_some_and(|tab| matches!(tab.page, Page::HostGroup(_)))
            })
    }

    fn unique_host_group_label(&self, base: &str, existing: Option<TabId>) -> String {
        let available = |label: &str| {
            self.tabs
                .entries()
                .iter()
                .all(|entry| Some(entry.id) == existing || entry.value.label != label)
        };
        if available(base) {
            return base.to_owned();
        }
        (2..)
            .map(|index| format!("{base} ({index})"))
            .find(|label| available(label))
            .expect("There are finitely many tabs")
    }

    fn new_host_group_tab(&mut self, name: Option<&str>) -> TabId {
        let base = name.map(|name| format!("SSH group · {name}"));
        let label = self.unique_host_group_label(base.as_deref().unwrap_or(LABEL), None);
        self.tabs.insert(AppTab {
            label,
            page: Page::HostGroup(Box::new(HostGroup {
                roster: name.map(|_| Box::default()),
                ..Default::default()
            })),
        })
    }

    pub(super) fn host_group_command_target(&self) -> Option<TabId> {
        let id = self.tabs.active_id()?;
        match &self.tabs.get(id)?.page {
            Page::HostGroup(page) if page.can_view_run() => Some(id),
            _ => None,
        }
    }

    fn start_host_group_panes(
        &mut self,
        id: TabId,
        opened: BTreeSet<u64>,
        targets: impl IntoIterator<Item = (Option<String>, SshOptions)>,
        mode: PaneLaunch,
        login_limit: Option<usize>,
        ctx: &ComponentContext<Self>,
    ) {
        self.install_host_group_panes(id, opened, targets, mode, login_limit);
        self.tabs.select(id);
        self.sync_selection(ctx);
        self.activate(ctx);
    }

    fn install_host_group_panes(
        &mut self,
        id: TabId,
        mut opened: BTreeSet<u64>,
        targets: impl IntoIterator<Item = (Option<String>, SshOptions)>,
        mode: PaneLaunch,
        login_limit: Option<usize>,
    ) -> Vec<LaunchedGroupPane> {
        let launch_gate = self
            .host_group_by_id_mut(id)
            .and_then(|page| page.login_permits(login_limit));
        let mut launched = Vec::new();
        for (profile, ssh) in targets {
            let session = launch_gate.as_ref().map(|gate| {
                Session::start_group_ssh(
                    ssh.clone(),
                    TerminalSize::default(),
                    self.appearance
                        .read()
                        .unwrap()
                        .interaction
                        .remote_clipboard_policy(),
                    gate.clone(),
                )
            });
            let source = Source::Ssh(ssh);
            let label = profile.clone().unwrap_or_else(|| source_label(&source));
            let tab = match session {
                Some(session) => live_tab_for_session(label, source, session).with_profile(profile),
                None => self.make_live_tab(label, source, profile),
            };
            let pane_id = tab.pane_id().expect("an SSH tab has a terminal pane");
            opened.insert(pane_id);
            let tab_id = self.tabs.insert(tab);
            launched.push(LaunchedGroupPane { pane_id, tab_id });
        }
        if let Some(page) = self.host_group_by_id_mut(id) {
            if let Some(roster) = &mut page.roster {
                roster.panes.extend(opened.iter().copied());
            }
            if matches!(mode, PaneLaunch::Select) {
                page.selected = opened;
                page.filter.clear();
                page.saved.feedback.clear();
                page.import.feedback.clear();
                page.reconnect_feedback.clear();
            }
        }
        launched
    }

    pub(super) fn update_host_group_timer(&mut self, ctx: &ComponentContext<Self>) {
        if matches!(
            self.tabs.active().map(|tab| &tab.page),
            Some(Page::HostGroup(_))
        ) || self.window_group_run_picker_open()
            || self.tabs.entries().iter().any(|entry| {
                matches!(&entry.value.page, Page::HostGroup(page)
                if page.connection_notice.is_armed() || !page.ready_waiters.is_empty()
                    || page.submissions.iter().any(|run| {
                        run.has_queue() || !run.control_waiters.is_empty()
                    }))
            })
        {
            if self.host_group_timer.is_none() {
                let sender = ctx.sender();
                self.host_group_timer =
                    crate::ui_timer::RetryTimer::with_next_delay(250, move || {
                        sender.send(Message::HostGroup(Action::Tick));
                        250
                    })
                    .ok();
                if self.host_group_timer.is_none() {
                    ctx.spawn_background(|_| {
                        std::thread::sleep(Duration::from_millis(250));
                        Message::HostGroup(Action::Tick)
                    });
                }
            }
        } else {
            self.host_group_timer = None;
        }
    }

    fn host_group_mut(&mut self, id: TabId) -> Option<&mut HostGroup> {
        if self.tabs.active_id() != Some(id) {
            return None;
        }
        self.host_group_by_id_mut(id)
    }

    fn host_group_by_id_mut(&mut self, id: TabId) -> Option<&mut HostGroup> {
        match &mut self.tabs.get_mut(id)?.page {
            Page::HostGroup(page) => Some(page),
            _ => None,
        }
    }

    fn host_group_targets(&self) -> Vec<Target> {
        self.tabs
            .entries()
            .iter()
            .filter_map(|entry| {
                let Page::Terminal(live) = &entry.value.page else {
                    return None;
                };
                let Source::Ssh(options) = &live.source else {
                    return None;
                };
                if !live.session.is_direct_ssh_shell() {
                    return None;
                }
                let pane = live.session.pane_id;
                let connected = live.session.ssh_connection().is_some();
                let state = if let Some(prompt) = live.session.prompts.current() {
                    match prompt.question {
                        Question::HostKey { .. } => TargetState::HostKey(prompt.id),
                        Question::Authentication { .. } => {
                            TargetState::Authentication(Some(prompt.id))
                        }
                    }
                } else if live.device_busy {
                    TargetState::Authentication(None)
                } else if !connected {
                    match live.session.events.ssh_connection_phase() {
                        Some(SshConnectionPhase::Queued) => TargetState::Queued,
                        Some(SshConnectionPhase::Connecting) => TargetState::Connecting,
                        Some(SshConnectionPhase::Reconnecting { attempt }) => {
                            TargetState::Reconnecting(attempt)
                        }
                        Some(SshConnectionPhase::Failed(_)) => {
                            TargetState::Failed(entry.value.exited_pane())
                        }
                        Some(SshConnectionPhase::Stopped) => {
                            TargetState::Stopped(entry.value.exited_pane())
                        }
                        _ => entry
                            .value
                            .exited_pane()
                            .map_or(TargetState::Offline, TargetState::Restartable),
                    }
                } else if live.read_only.get() {
                    TargetState::ReadOnly
                } else if live.session.frames.load().alternate {
                    TargetState::Alternate
                } else {
                    match live.session.events.shell_prompt() {
                        Some(ShellPrompt::Ready(prompt)) => TargetState::Ready(prompt),
                        Some(ShellPrompt::Matched(prompt)) => TargetState::Matched(prompt),
                        Some(ShellPrompt::Opening | ShellPrompt::InUse) => TargetState::InUse,
                        _ => TargetState::Unverified,
                    }
                };
                let label = entry
                    .value
                    .pane_name()
                    .map_or(entry.value.label.as_str(), |name| name.as_str());
                let names = filter::Names::new(label, &options.host, &options.user, options.port);
                Some(Target {
                    pane,
                    name: names.display(pane),
                    names,
                    profile: live.profile_name.clone(),
                    connected,
                    state,
                })
            })
            .collect()
    }

    fn dispatch_to_host(
        &self,
        target: Target,
        text: Arc<str>,
        host: w11term::events::GroupTranscriptHost,
    ) -> Delivery {
        let receipt = host.delivery_receipt();
        let mut execution = None;
        let status = match target.state {
            TargetState::Ready(prompt) | TargetState::Matched(prompt) => {
                match self
                    .tab_for_pane(target.pane)
                    .and_then(|id| self.tabs.get(id))
                {
                    Some(AppTab {
                        page: Page::Terminal(live),
                        ..
                    }) if live.session.is_direct_ssh_shell() => {
                        let (reply, received) = oneshot::channel();
                        let (observe, observed) =
                            watch::channel(if matches!(target.state, TargetState::Matched(_)) {
                                GroupExecution::AwaitingPrompt
                            } else {
                                GroupExecution::AwaitingMarkers
                            });
                        let queued = live.session.input.send(Command::SendGroupCommand {
                            prompt,
                            text,
                            reply,
                            execution: observe,
                            transcript: host,
                        });
                        if queued.is_ok() {
                            execution = Some(observed);
                            DeliveryStatus::Pending(Instant::now(), received)
                        } else {
                            receipt.reject("The pane closed.");
                            DeliveryStatus::Rejected("The pane closed.".into())
                        }
                    }
                    _ => DeliveryStatus::Rejected("The SSH shell is no longer available.".into()),
                }
            }
            state => DeliveryStatus::Rejected(state.label().into()),
        };
        if let DeliveryStatus::Rejected(reason) = &status {
            receipt.reject(reason);
        }
        Delivery {
            target,
            status,
            execution,
            interrupt: None,
        }
    }

    fn group_send_matches(&self, id: TabId, offer: &CommandOffer, verify_prompts: bool) -> bool {
        if self.host_group_command_target() != Some(id) {
            return false;
        }
        let Some(AppTab {
            page: Page::HostGroup(page),
            ..
        }) = self.tabs.get(id)
        else {
            return false;
        };
        let CommandOffer {
            text,
            targets,
            limit,
            interval,
            queue_expiry,
            stop_queue_on_failure,
            canary,
            notify_canary,
        } = offer;
        let roster = self.host_group_targets();
        let current = command_order::recipients(page, page.shown(&roster));
        let ready = targets
            .iter()
            .filter(|target| target.state.is_ready())
            .count();
        !page.sending()
            && page.readiness_allows_send(ready, targets.len())
            && current
                .iter()
                .map(|target| target.pane)
                .eq(targets.iter().map(|target| target.pane))
            && page.roster_preferences.command_limit == *limit
            && page.roster_preferences.command_interval == *interval
            && page.roster_preferences.command_queue_expiry == *queue_expiry
            && page.roster_preferences.stop_command_queue_on_failure == *stop_queue_on_failure
            && page.draft == text.as_ref()
            && page.canary == *canary
            && (page.canary.is_some() && page.notify_canary) == *notify_canary
            && command_queue::canary_readiness(targets, *canary).is_ok()
            && w11term::saved_commands::valid_command_text(text)
            && ((!verify_prompts
                && !page.preview_command_order
                && canary.is_none()
                && !page.roster_preferences.require_all_ready)
                || current.iter().zip(targets).all(|(current, reviewed)| {
                    current.state.prompt() == reviewed.state.prompt()
                        && current.name == reviewed.name
                }))
    }

    fn submit_group_command(
        &mut self,
        id: TabId,
        offer: CommandOffer,
        transcript: GroupTranscript,
        recording: output_recording::State,
        origin: SubmissionOrigin,
    ) -> Option<u64> {
        if !matches!(
            self.tabs.get(id).map(|tab| &tab.page),
            Some(Page::HostGroup(_))
        ) {
            return None;
        }
        let policy = offer.dispatch_policy();
        let CommandOffer {
            text,
            targets,
            canary,
            notify_canary,
            ..
        } = offer;
        let interval = Duration::from_millis(policy.interval_ms);
        let deadline = policy
            .queue_expiry_ms
            .map(|ms| Instant::now() + Duration::from_millis(ms));
        let slots = policy.parallel_limit.or_else(|| {
            (canary.is_some() || !interval.is_zero() || deadline.is_some()).then_some(targets.len())
        });
        let results = targets
            .into_iter()
            .map(|target| {
                let host = transcript.host(target.pane);
                if slots.is_some() && target.state.is_ready() {
                    Delivery {
                        target,
                        status: DeliveryStatus::Queued(command_queue::Waiting::new(host)),
                        execution: None,
                        interrupt: None,
                    }
                } else {
                    self.dispatch_to_host(target, text.clone(), host)
                }
            })
            .collect();
        let Page::HostGroup(page) = &mut self.tabs.get_mut(id)?.page else {
            unreachable!("the group remains open during dispatch");
        };
        if origin == SubmissionOrigin::Interactive {
            page.manual_command_order.clear();
        }
        let submission = page.retain_submission(
            SubmissionContent::Command(text),
            results,
            transcript,
            origin,
        );
        if origin == SubmissionOrigin::Control {
            submission.control_recording = recording.control_watcher();
        }
        submission.recording = recording;
        submission.command_queue = slots.map(|slots| {
            command_queue::Queue::new(
                slots,
                policy.stop_on_failure,
                canary,
                notify_canary,
                interval,
                deadline,
            )
        });
        submission.dispatch_policy = Some(policy);
        let run = submission.id;
        self.poll_command_queues();
        Some(run)
    }

    fn interrupt_hosts(
        &mut self,
        id: TabId,
        submission: u64,
        targets: Vec<(u64, GroupInterruptTarget)>,
    ) {
        let roster = self.host_group_targets();
        let Some(page) = self
            .host_group_mut(id)
            .filter(|page| page.viewing == submission)
        else {
            return;
        };
        let Some(run) = page.submissions.iter().find(|run| run.id == submission) else {
            return;
        };
        let eligible = page.selected_shown(&roster);
        let selected: Vec<_> = targets
            .into_iter()
            .filter(|&(pane, link)| {
                eligible.contains(&pane)
                    && run.results.iter().any(|delivery| {
                        delivery.target.pane == pane
                            && page.matches_result(delivery, progress::Outcome::of(delivery))
                            && delivery.interrupt_target() == Some(link)
                    })
            })
            .collect();
        let mut outcomes: BTreeMap<_, _> = selected
            .into_iter()
            .map(|(pane, link)| {
                let status = match self.tab_for_pane(pane).and_then(|id| self.tabs.get(id)) {
                    Some(AppTab {
                        page: Page::Terminal(live),
                        ..
                    }) if live.session.is_direct_ssh_shell() => {
                        let (reply, received) = oneshot::channel();
                        if live
                            .session
                            .input
                            .send(Command::InterruptGroupCommand {
                                target: link,
                                reply,
                            })
                            .is_ok()
                        {
                            DeliveryStatus::Pending(Instant::now(), received)
                        } else {
                            DeliveryStatus::Rejected("The pane closed.".into())
                        }
                    }
                    _ => DeliveryStatus::Rejected("The SSH shell is no longer available.".into()),
                };
                (pane, status)
            })
            .collect();
        if let Some(run) = self
            .host_group_mut(id)
            .and_then(|page| page.submissions.iter_mut().find(|run| run.id == submission))
        {
            for delivery in &mut run.results {
                if let Some(status) = outcomes.remove(&delivery.target.pane) {
                    delivery.interrupt = Some(status);
                }
            }
        }
    }

    fn focus_host_group_pane(&mut self, id: TabId, pane: u64, ctx: &ComponentContext<Self>) {
        if self.host_group_mut(id).is_some()
            && let Some(tab) = self.tab_for_pane(pane)
            && self.tabs.select(tab)
        {
            self.sync_selection(ctx);
            self.activate(ctx);
        }
    }

    pub(super) fn roster_picker_hosts(&self, id: TabId) -> Vec<(u64, String)> {
        if self.host_group_command_target() != Some(id) {
            return Vec::new();
        }
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page) else {
            return Vec::new();
        };
        let targets = self.host_group_targets();
        let mut shown: Vec<_> = page.shown(&targets).collect();
        page.sort_shown(&mut shown);
        shown
            .into_iter()
            .map(|target| (target.pane, target.names.address().to_owned()))
            .collect()
    }

    pub(super) fn handle_host_group(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Open | Action::NewTab => {
                self.refresh_profiles();
                let existing = matches!(action, Action::Open)
                    .then(|| self.recent_host_group_tab())
                    .flatten();
                let id = existing.unwrap_or_else(|| self.new_host_group_tab(None));
                self.tabs.select(id);
                self.sync_selection(ctx);
                self.activate(ctx);
            }
            Action::OpenNamedGroup { name, members } => {
                self.open_host_group_from_palette(&name, &members, ctx);
            }
            Action::ReviewNamedGroup(offer) => self.review_host_group_from_palette(offer, ctx),
            Action::ControlRecordingPrepared(prepared) => {
                self.finish_control_recording(*prepared, ctx);
            }
            Action::Saved(id, action) => self.handle_saved_host_group(id, action, ctx),
            Action::Import(id, action) => self.handle_host_import(id, action, ctx),
            Action::Membership(id, action) => self.handle_group_membership(id, action),
            Action::Reuse(id, action) => self.handle_host_group_reuse(id, action, ctx),
            Action::Reconnect(id, request) => self.reconnect_host_group(id, request),
            Action::StopPending(id, attempts) => self.stop_pending_host_group(id, &attempts),
            Action::ConnectionNotice(id, action) => self.handle_connection_notice(id, action, ctx),
            Action::CloseEnded(id, action) => self.handle_close_ended(id, action, ctx),
            Action::Disconnect(id, action) => self.handle_group_disconnect(id, action),
            Action::PromptSetup(id, action) => self.handle_group_prompt_setup(id, action),
            Action::HostListExport(id, action) => self.handle_host_list_export(id, action, ctx),
            Action::RecipientSets(id, action) => self.handle_recipient_sets(id, action),
            Action::Transfer(id, action) => self.handle_host_group_transfer(id, action, ctx),
            Action::Environment(id, action) => self.handle_host_group_environment(id, action),
            Action::RecordedSend(id, action) => self.handle_recorded_send(id, action, ctx),
            Action::Tick => {
                self.poll_connection_notices();
                self.poll_command_queues();
                self.poll_ssh_group_waiters();
                self.poll_ssh_group_ready_waiters();
                let watching_runs = self.window_group_run_picker_open();
                if !matches!(
                    self.tabs.active().map(|tab| &tab.page),
                    Some(Page::HostGroup(_))
                ) && !watching_runs
                {
                    self.update_host_group_timer(ctx);
                    return;
                }
                let panes: BTreeSet<_> = self
                    .host_group_targets()
                    .into_iter()
                    .map(|target| target.pane)
                    .collect();
                let groups: Vec<_> = self
                    .tabs
                    .entries()
                    .iter()
                    .filter(|entry| matches!(entry.value.page, Page::HostGroup(_)))
                    .map(|entry| entry.id)
                    .collect();
                for id in groups {
                    if let Some(Page::HostGroup(page)) =
                        self.tabs.get_mut(id).map(|tab| &mut tab.page)
                    {
                        page.selected.retain(|pane| panes.contains(pane));
                        page.prompt_setup.poll();
                        page.disconnect.poll();
                        for submission in &mut page.submissions {
                            if let Some(path) = submission.recording.poll() {
                                submission.saved_recordings.add(path);
                            }
                            submission.refresh_deliveries();
                        }
                    }
                }
                if watching_runs {
                    self.refresh_open_commands();
                }
                self.refresh_roster_picker(ctx);
                self.update_host_group_timer(ctx);
            }
            Action::Edit(id, text) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.draft = text;
                }
            }
            Action::Filter(id, query) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.filter.set_query(query);
                }
            }
            Action::FilterWildcards(id, wildcards) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.filter.set_wildcards(wildcards);
                }
            }
            Action::FilterStatus(id, status) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.filter.set_status(status);
                }
            }
            Action::OrderRoster(id, order) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.roster_preferences.order = order;
                }
            }
            Action::ShowLastOutput(id, show) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.roster_preferences.show_last_output = show;
                }
            }
            Action::ShowTerminalLine(id, show) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.roster_preferences.show_terminal_line = show;
                }
            }
            Action::RequireAllReady(id, require) => {
                if let Some(page) = self
                    .host_group_mut(id)
                    .filter(|page| !page.recorded_send.pending())
                {
                    page.roster_preferences.require_all_ready = require;
                }
            }
            Action::CopyRoster(id, offered) => self.copy_host_group_roster(id, &offered),
            Action::ShowWindowRoster(id, show) => {
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self.host_group_mut(id)
                {
                    page.show_window_roster(show);
                }
            }
            Action::Select(id, pane, selected) => {
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self.host_group_mut(id)
                    && page.includes_pane(pane)
                {
                    if selected {
                        page.selected.insert(pane);
                    } else {
                        page.selected.remove(&pane);
                    }
                }
            }
            Action::SelectShown(id, shown, selection) => {
                let roster = self.host_group_targets();
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self.host_group_mut(id)
                {
                    let visible: Vec<_> = page.shown(&roster).collect();
                    for target in visible
                        .into_iter()
                        .filter(|target| shown.contains(&target.pane))
                    {
                        if selection.includes(target.state) {
                            page.selected.insert(target.pane);
                        } else {
                            page.selected.remove(&target.pane);
                        }
                    }
                }
            }
            Action::Visit(id, pane) => self.focus_host_group_pane(id, pane, ctx),
            Action::FindHost(id) => self.open_roster_picker(id, ctx),
            Action::VisitPrompt(id, pane, prompt) => {
                if self.host_group_command_target() == Some(id) {
                    let targets = self.host_group_targets();
                    if self.host_group_mut(id).is_some_and(|page| {
                        page.shown(&targets).any(|target| {
                            target.pane == pane
                                && target.state.authentication_prompt() == Some(prompt)
                        })
                    }) {
                        self.focus_host_group_pane(id, pane, ctx);
                    }
                }
            }
            Action::ViewSubmission(id, submission) => {
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self.host_group_mut(id)
                    && page.submissions.iter().any(|item| item.id == submission)
                {
                    if page.viewing != submission {
                        page.viewing = submission;
                        page.result_filter = progress::Filter::All;
                        page.copy_feedback.clear();
                    }
                    page.preview_command_order = false;
                }
            }
            Action::OpenRunPicker(id) => {
                if self.group_run_choices(id, "").next().is_some() {
                    self.open_group_run_picker(id, ctx);
                }
            }
            Action::NotifyCompletion(id, submission, armed) => {
                self.set_group_run_completion_notice(id, submission, armed);
            }
            Action::CancelOutputAlert(id, run) => {
                if self.tabs.active_id() == Some(id)
                    && let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page)
                    && let Some(submission) = page.viewed_submission(run)
                {
                    submission.transcript.cancel_output_alert();
                }
            }
            Action::LiveOutput(id, submission, action) => {
                if matches!(action, live_output::Action::ArmOutputAlert) {
                    self.arm_group_output_alert(id, submission);
                    return;
                }
                if matches!(
                    action,
                    live_output::Action::CopyShown | live_output::Action::SaveShown
                ) {
                    self.share_group_live_output(
                        id,
                        submission,
                        matches!(action, live_output::Action::SaveShown),
                        ctx,
                    );
                    return;
                }
                if matches!(action, live_output::Action::Open)
                    && self.host_group_command_target() != Some(id)
                {
                    return;
                }
                if let Some(page) = self.host_group_mut(id) {
                    page.update_live_output(submission, action);
                }
            }
            Action::RecordOutput(id, submission, action) => {
                self.handle_group_output_recording(id, submission, action, ctx);
            }
            Action::AlertedSend(id, action) => self.handle_alerted_send(id, action, ctx),
            Action::Compare(id, submission, action) => {
                self.handle_host_group_comparison(id, submission, action, ctx);
            }
            Action::FilterResults(id, submission, filter) => {
                if let Some(page) = self.host_group_mut(id)
                    && page.viewed_submission(submission).is_some()
                {
                    page.result_filter = filter;
                }
            }
            Action::SearchRecipients(id, submission, query) => {
                if let Some(page) = self.host_group_mut(id)
                    && page.viewed_submission(submission).is_some()
                {
                    page.recipient_search.set_query(query);
                }
            }
            Action::OrderResults(id, submission, order) => {
                if let Some(page) = self.host_group_mut(id)
                    && page.viewed_submission(submission).is_some()
                {
                    page.result_order = order;
                }
            }
            Action::LoadCommand(id, submission) => {
                if let Some(page) = self.host_group_mut(id)
                    && page.load_command(submission)
                {
                    let _ = page.command_editor.request_focus();
                }
            }
            Action::PrepareRetry(id, submission, offered) => {
                if self.host_group_command_target() == Some(id) {
                    let roster = self.host_group_targets();
                    if let Some(page) = self.host_group_mut(id)
                        && page.prepare_retry(submission, &roster, offered)
                    {
                        let _ = page.command_editor.request_focus();
                    }
                }
            }
            Action::OpenSavedCommands(id) => {
                if self.host_group_command_target() == Some(id) {
                    self.open_group_command_library(id, ctx);
                }
            }
            Action::OpenRecentCommands(id) => {
                if self.host_group_command_target() == Some(id) {
                    self.open_group_recent_commands(id, ctx);
                }
            }
            Action::FocusLoadedCommand(id) => {
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self.host_group_mut(id)
                    && std::mem::take(&mut page.focus_loaded_command)
                {
                    let _ = page.command_editor.request_focus();
                }
            }
            Action::LoadSavedCommand(id, entry) => {
                if self.host_group_command_target() == Some(id)
                    && self.confirm_saved_command(&entry)
                    && let Some(page) = self.host_group_mut(id)
                {
                    page.load_draft(entry.text);
                    let _ = page.command_editor.request_focus();
                }
            }
            Action::LoadRecentCommand(id, submission) => {
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self.host_group_mut(id)
                    && let Some(command) = page.recent_commands.get(submission)
                {
                    page.load_draft(command.to_owned());
                    let _ = page.command_editor.request_focus();
                }
            }
            Action::SaveCommand(id, action) => {
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self.host_group_mut(id)
                    && let Some(commands) =
                        page.command_save
                            .update(action, &page.draft, saved_commands::create)
                {
                    self.saved_commands = commands;
                }
            }
            Action::RestoreDraft(id) => {
                if let Some(page) = self.host_group_mut(id)
                    && page.restore_draft()
                {
                    let _ = page.command_editor.request_focus();
                }
            }
            Action::CopyResults(id, submission, offered) => {
                self.copy_host_group_results(id, submission, offered.as_deref())
            }
            Action::SaveResults(id, submission, format, offered) => {
                if self.export.pending.is_none()
                    && let Some(report) =
                        self.capture_host_group_report(id, submission, offered.as_deref())
                {
                    self.begin_text_export(
                        text_export::Subject::HostGroupResults {
                            submission,
                            format,
                            shown: offered.is_some(),
                        },
                        move |file| match format {
                            text_export::GroupReportFormat::Text => report.write_to(file),
                            text_export::GroupReportFormat::Json => report.write_json(file),
                        },
                        ctx,
                    );
                }
            }
            Action::ViewOutput(id, pane, link) => {
                if self.host_group_mut(id).is_some() {
                    self.open_command_history(pane, Some(link), ctx);
                }
            }
            Action::Interrupt(id, submission, targets) => {
                self.interrupt_hosts(id, submission, targets)
            }
            Action::CommandLimit(id, limit) => {
                if let Some(page) = self
                    .host_group_mut(id)
                    .filter(|page| !page.recorded_send.pending())
                {
                    page.roster_preferences.command_limit = limit;
                }
            }
            Action::CommandInterval(id, interval) => {
                if let Some(page) = self
                    .host_group_mut(id)
                    .filter(|page| !page.recorded_send.pending())
                {
                    page.roster_preferences.command_interval = interval;
                }
            }
            Action::CommandQueueExpiry(id, expiry) => {
                if let Some(page) = self
                    .host_group_mut(id)
                    .filter(|page| !page.recorded_send.pending())
                {
                    page.roster_preferences.command_queue_expiry = expiry;
                }
            }
            Action::CommandCanary(id, pane) => {
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self
                        .host_group_mut(id)
                        .filter(|page| !page.recorded_send.pending())
                {
                    page.canary = pane;
                }
            }
            Action::NotifyCanary(id, enabled) => {
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self
                        .host_group_mut(id)
                        .filter(|page| page.canary.is_some() && !page.recorded_send.pending())
                {
                    page.notify_canary = enabled;
                }
            }
            Action::PreviewCommandOrder(id, show) => {
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self.host_group_mut(id)
                {
                    page.preview_command_order = show;
                }
            }
            Action::EditCommandOrder(id, edit) => self.edit_command_order(id, edit),
            Action::CommandQueue(id, run, action) => self.handle_command_queue(id, run, action),
            Action::ShowQueueActivity(id, run, show) => {
                if let Some(page) = self.host_group_mut(id).filter(|page| page.viewing == run)
                    && let Some(submission) =
                        page.submissions.iter_mut().find(|item| item.id == run)
                {
                    submission.show_queue_activity = show;
                }
            }
            Action::StopCommandQueueOnFailure(id, stop) => {
                if let Some(page) = self
                    .host_group_mut(id)
                    .filter(|page| !page.recorded_send.pending() && page.orders_commands())
                {
                    page.roster_preferences.stop_command_queue_on_failure = stop;
                }
            }
            Action::Send(id, offer) => {
                if !self.group_send_matches(id, &offer, false)
                    || self
                        .host_group_mut(id)
                        .is_none_or(|page| page.recorded_send.pending())
                {
                    return;
                }
                if let Some(page) = self.host_group_mut(id) {
                    page.recorded_send.clear_feedback();
                    page.alerted_send.clear_feedback();
                }
                self.submit_group_command(
                    id,
                    offer,
                    GroupTranscript::default(),
                    Default::default(),
                    SubmissionOrigin::Interactive,
                );
            }
        }
    }

    fn capture_full_host_group_report(&self, submission: &Submission) -> report::Report {
        report::Report::capture(submission, |pane, link| {
            self.history_frame(pane)?.command_for_link(link).cloned()
        })
    }

    fn capture_host_group_report(
        &mut self,
        id: TabId,
        submission_id: u64,
        offered: Option<&[u64]>,
    ) -> Option<report::Report> {
        if self.tabs.active_id() != Some(id) {
            return None;
        }
        let Page::HostGroup(page) = &self.tabs.get(id)?.page else {
            return None;
        };
        let submission = page.viewed_submission(submission_id)?;
        let Some(offered) = offered else {
            return Some(self.capture_full_host_group_report(submission));
        };
        let retained = |pane, link| self.history_frame(pane)?.command_for_link(link).cloned();
        let now = Instant::now();
        let (summary, rows) = page.result_rows(submission, now);
        if rows.is_empty()
            || rows.len() == summary.total()
            || !rows
                .iter()
                .map(|(delivery, _)| delivery.target.pane)
                .eq(offered.iter().copied())
        {
            if let Some(page) = self.host_group_mut(id) {
                page.copy_feedback =
                    "Results changed. Review the shown recipients and try again.".into();
            }
            return None;
        }
        Some(report::Report::capture_rows(
            submission, now, rows, retained,
        ))
    }

    fn copy_host_group_results(&mut self, id: TabId, submission_id: u64, offered: Option<&[u64]>) {
        let Some(report) = self.capture_host_group_report(id, submission_id, offered) else {
            return;
        };
        let copied = report
            .clipboard(w11term::clipboard::MAX_TEXT)
            .and_then(|text| w11term::clipboard::write(&text));
        if let Some(page) = self.host_group_mut(id) {
            let scope = if offered.is_some() {
                "shown results"
            } else {
                "results"
            };
            page.copy_feedback = match copied {
                Ok(()) => format!("Copied {scope}"),
                Err(error) => format!("Could not copy {scope} · {error}"),
            };
        }
    }

    fn host_group_failure_reason(&self, pane: u64) -> Option<Arc<str>> {
        let Page::Terminal(live) = &self.tabs.get(self.tab_for_pane(pane)?)?.page else {
            return None;
        };
        match live.session.events.ssh_connection_phase() {
            Some(SshConnectionPhase::Failed(reason)) => Some(reason),
            _ => None,
        }
    }

    fn host_group_output_preview(
        &self,
        id: TabId,
        target: &Target,
        ctx: &ViewContext<Self>,
    ) -> Option<View> {
        if !target.connected
            || target.state.is_authenticating()
            || matches!(target.state, TargetState::Alternate)
        {
            return None;
        }
        let pane = target.pane;
        let Page::Terminal(live) = &self.tabs.get(self.tab_for_pane(pane)?)?.page else {
            return None;
        };
        let frame = live.session.frames.load();
        if frame.alternate
            || frame.prompt.is_some()
            || live
                .session
                .ssh_connection_at(frame.connection_generation)
                .is_none()
        {
            return None;
        }
        let (link, caption) = frame.latest_command_output_preview()?;
        let caption = format!("Last command · {caption}");
        Some(
            Grid::new()
                .column_spacing(8.0)
                .columns([GridLength::STAR, GridLength::Auto])
                .children((
                    TextBlock::new()
                        .automation_name(format!("Pane {pane} · {caption}"))
                        .vertical_alignment(VerticalAlignment::Center)
                        .text_trimming(TextTrimming::CharacterEllipsis)
                        .text(caption),
                    Button::new()
                        .grid_column(1)
                        .automation_name(format!("Open last command for pane {pane}"))
                        .on_click(ctx.callback(move |_| {
                            Message::HostGroup(Action::ViewOutput(id, pane, link))
                        }))
                        .content("Open history"),
                )),
        )
    }

    fn host_group_terminal_line(&self, target: &Target) -> Option<String> {
        if !target.connected || target.state.is_authenticating() {
            return None;
        }
        let session = self.direct_ssh_session(target.pane)?;
        let frame = session.live_snapshot();
        if frame.alternate
            || frame.prompt.is_some()
            || session
                .ssh_connection_at(frame.connection_generation)
                .is_none()
        {
            return None;
        }
        frame.last_visible_text_line()
    }

    pub(super) fn host_group_view(
        &self,
        id: TabId,
        page: &HostGroup,
        ctx: &mut ViewContext<Self>,
    ) -> View {
        if let Some(view) = page.alerted_send.view(id, ctx) {
            return view;
        }
        if let Some(view) = page.close_ended.review(id, ctx) {
            return view;
        }
        if let Some(view) = page.disconnect.review(id, ctx) {
            return view;
        }
        if page.environment.is_open() {
            return page
                .environment
                .view(id, page, &self.host_group_targets(), ctx);
        }
        if let Some(view) = page.transfer.view(id, ctx) {
            return view;
        }
        if let Some(view) = page.prompt_setup.view(id, ctx) {
            return view;
        }
        if let Some(view) = page.import.view(id, ctx) {
            return view;
        }
        if let Some(view) = page
            .host_list_export
            .view(id, self.export.pending.is_some(), ctx)
        {
            return view;
        }
        if let Some(view) = page.saved.reuse.view(id, ctx) {
            return view;
        }
        if let Some(view) = page.saved.recipient_sets.view(id, ctx) {
            return view;
        }
        if let Some(roster) = page
            .roster
            .as_ref()
            .filter(|roster| roster.membership.is_open())
            && let Some(view) =
                roster
                    .membership
                    .view(id, &roster.panes, &self.host_group_targets(), ctx)
        {
            return view;
        }
        if let Some(editor) = self.saved_host_group_editor(id, &page.saved, ctx) {
            return editor;
        }
        if let Some(comparison) = self.host_group_comparison(id, page, ctx) {
            return comparison;
        }
        if let Some(output) = &page.live_output
            && let Some(submission) = page.viewed_submission(output.submission)
        {
            return self.host_group_live_output(id, page, output, submission, ctx);
        }
        if page.focus_loaded_command {
            let sender = ctx.sender();
            ctx.use_effect("focus host group draft", id, move || {
                sender.send(Message::HostGroup(Action::FocusLoadedCommand(id)));
                None
            });
        }
        let targets = self.host_group_targets();
        let matching: Vec<_> = page.matching_query(&targets).collect();
        let shown: Vec<_> = matching
            .iter()
            .copied()
            .filter(|target| page.filter.matches_state(target))
            .collect();
        let selected: Vec<_> = shown
            .iter()
            .copied()
            .filter(|target| page.selected.contains(&target.pane))
            .cloned()
            .collect::<Vec<_>>();
        let text: Arc<str> = page.draft.as_str().into();
        let valid_text = w11term::saved_commands::valid_command_text(&text);
        let ready = selected
            .iter()
            .filter(|target| target.state.is_ready())
            .count();
        let canary_error = command_queue::canary_readiness(&selected, page.canary).err();
        let enabled = valid_text
            && canary_error.is_none()
            && !page.sending()
            && !page.recorded_send.pending()
            && page.readiness_allows_send(ready, selected.len());
        let send_label = if page.canary.is_some() {
            "Run canary first".into()
        } else {
            format!(
                "Send to {} {}",
                selected.len(),
                if selected.len() == 1 { "host" } else { "hosts" }
            )
        };
        let mut roster_rows = shown.clone();
        page.sort_shown(&mut roster_rows);
        let mut prompts = roster_rows
            .iter()
            .copied()
            .filter_map(Target::pending_prompt);
        let next_prompt =
            prompts.next().map_or_else(View::empty, |(pane, prompt)| {
                let count = 1 + prompts.count();
                Button::new()
                    .automation_name("Open next SSH prompt")
                    .on_click(ctx.callback(move |_| {
                        Message::HostGroup(Action::VisitPrompt(id, pane, prompt))
                    }))
                    .content(format!("Open next SSH prompt ({count})"))
            });
        let mut details: Vec<KeyedView> = roster_rows
            .into_iter()
            .map(|target| {
                let pane = target.pane;
                let state = target.status_caption();
                let failure = if matches!(target.state, TargetState::Failed(_)) {
                    self.host_group_failure_reason(pane)
                } else {
                    None
                };
                KeyedView::new(
                    format!("target-{pane}"),
                    StackPanel::new().spacing(2.0).children((
                        CheckBox::new()
                            .automation_name(&target.name)
                            .is_checked(page.selected.contains(&pane))
                            .on_is_checked_changed(ctx.callback(move |checked| {
                                Message::HostGroup(Action::Select(id, pane, checked))
                            }))
                            .content(target.name.as_str()),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(10.0)
                            .children((
                                TextBlock::new()
                                    .automation_name(format!("Pane {pane} · {state}"))
                                    .text(state.as_ref()),
                                Button::new()
                                    .automation_name(format!("Focus pane {pane}"))
                                    .on_click(ctx.callback(move |_| {
                                        Message::HostGroup(Action::Visit(id, pane))
                                    }))
                                    .content("Focus pane"),
                                reconnect::host_button(id, target.state, ctx),
                                prompt_setup::button(id, target, ctx),
                            )),
                        failure.map_or_else(View::empty, |reason| {
                            TextBlock::new()
                                .automation_name(format!("Pane {pane} · {reason}"))
                                .text(reason.as_ref())
                                .text_wrapping(TextWrapping::Wrap)
                                .is_text_selection_enabled(true)
                                .into()
                        }),
                        if page.roster_preferences.show_last_output {
                            self.host_group_output_preview(id, target, ctx)
                                .unwrap_or_else(View::empty)
                        } else {
                            View::empty()
                        },
                        if page.roster_preferences.show_terminal_line {
                            self.host_group_terminal_line(target)
                                .map_or_else(View::empty, |line| {
                                    let caption = format!("Live line · {line}");
                                    TextBlock::new()
                                        .automation_name(format!("Pane {pane} · {caption}"))
                                        .text_trimming(TextTrimming::CharacterEllipsis)
                                        .text(caption)
                                        .into()
                                })
                        } else {
                            View::empty()
                        },
                    )),
                )
            })
            .collect();
        let filter_error = page.filter.error();
        if shown.is_empty() && filter_error.is_none() {
            details.push(KeyedView::new(
                "empty",
                TextBlock::new().text(if !targets.iter().any(|target| page.includes_pane(target.pane)) {
                    if page.roster.as_ref().is_some_and(|roster| !roster.show_window) {
                        "This group has no open SSH panes. Manage live panes or import a host list."
                    } else {
                        "Open a direct SSH shell to add it to this roster."
                    }
                } else {
                    "No SSH panes match this filter."
                }),
            ));
        }
        let selected_shown: BTreeSet<_> = selected.iter().map(|target| target.pane).collect();
        self.host_group_results(id, page, ctx, &shown, &selected_shown, &mut details);
        let hidden_count = targets
            .iter()
            .filter(|target| {
                page.includes_pane(target.pane)
                    && page.selected.contains(&target.pane)
                    && !selected_shown.contains(&target.pane)
            })
            .count();
        let shown_ids: BTreeSet<_> = shown.iter().map(|target| target.pane).collect();
        let bulk = |label, selection: ShownSelection| {
            let panes = shown_ids.clone();
            Button::new()
                .is_enabled(shown.iter().any(|target| {
                    page.selected.contains(&target.pane) != selection.includes(target.state)
                }))
                .on_click(ctx.callback(move |_| {
                    Message::HostGroup(Action::SelectShown(id, panes.clone(), selection))
                }))
                .content(label)
        };
        let filter_input = Grid::new()
            .column_spacing(8.0)
            .columns([
                GridLength::STAR,
                GridLength::Auto,
                GridLength::Auto,
                GridLength::Auto,
            ])
            .children((
                TextBox::new()
                    .automation_name("Host filter")
                    .placeholder_text(if page.filter.wildcards() {
                        "Patterns such as web-*, db-??, !*-canary"
                    } else {
                        "Filter by profile, pane, or SSH address"
                    })
                    .text(page.filter.query())
                    .on_text_changed(
                        ctx.callback(move |query| Message::HostGroup(Action::Filter(id, query))),
                    ),
                CheckBox::new()
                    .grid_column(1)
                    .automation_name("Use wildcards")
                    .is_checked(page.filter.wildcards())
                    .on_is_checked_changed(ctx.callback(move |checked| {
                        Message::HostGroup(Action::FilterWildcards(id, checked))
                    }))
                    .content("Use wildcards"),
                ComboBox::new()
                    .grid_column(2)
                    .automation_name("Filter host state")
                    .items_source(
                        RosterStatus::OPTIONS
                            .map(|status| status.label().to_owned())
                            .to_vec(),
                    )
                    .selected_index(
                        RosterStatus::OPTIONS
                            .iter()
                            .position(|&status| status == page.filter.status()),
                    )
                    .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                        index
                            .and_then(|index| RosterStatus::OPTIONS.get(index))
                            .map_or(Message::ControlIdle, |&status| {
                                Message::HostGroup(Action::FilterStatus(id, status))
                            })
                    })),
                ComboBox::new()
                    .grid_column(3)
                    .automation_name("Sort SSH roster")
                    .items_source(ROSTER_ORDERS.map(|(_, label)| label.to_owned()).to_vec())
                    .selected_index(
                        ROSTER_ORDERS
                            .iter()
                            .position(|&(order, _)| order == page.roster_preferences.order),
                    )
                    .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                        index
                            .and_then(|index| ROSTER_ORDERS.get(index))
                            .map_or(Message::ControlIdle, |&(order, _)| {
                                Message::HostGroup(Action::OrderRoster(id, order))
                            })
                    })),
            ));
        let filter = StackPanel::new().grid_row(2).spacing(6.0).children((
            filter_input,
            roster_state_summary(&matching, shown.len()).map_or_else(View::empty, |summary| {
                TextBlock::new()
                    .text(summary)
                    .text_wrapping(TextWrapping::Wrap)
                    .into()
            }),
            page.roster.as_ref().map_or_else(View::empty, |roster| {
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        CheckBox::new()
                            .automation_name("Show all window SSH panes")
                            .is_checked(roster.show_window)
                            .on_is_checked_changed(ctx.callback(move |show| {
                                Message::HostGroup(Action::ShowWindowRoster(id, show))
                            }))
                            .content("Show all window SSH panes"),
                        Button::new()
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(Action::Membership(id, membership::Action::Open))
                            }))
                            .content(membership::LABEL),
                        if page.saved.can_update_roster() {
                            saved::update_button(id, ctx)
                        } else {
                            View::empty()
                        },
                        saved::roster_defaults_button(id, page, ctx),
                    ))
            }),
            filter_error.map_or_else(View::empty, |error| {
                TextBlock::new()
                    .text(error)
                    .text_wrapping(TextWrapping::Wrap)
                    .into()
            }),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    bulk("Select shown", ShownSelection::All),
                    bulk("Deselect shown", ShownSelection::None),
                    bulk("Select only ready shown", ShownSelection::Ready),
                    Button::new()
                        .is_enabled(!selected_shown.is_empty())
                        .on_click(ctx.callback(move |_| {
                            Message::HostGroup(Action::Saved(
                                id,
                                saved::Action::NewFromSelection(selected_shown.clone()),
                            ))
                        }))
                        .content("Save selection as group…"),
                )),
            reconnect::selection_controls(
                id,
                page,
                &shown,
                self.pending_attempts(page, &shown),
                ctx,
            ),
            page.close_ended
                .controls(id, self.close_ended_recipients(selected.iter()), ctx),
            page.disconnect
                .controls(id, self.disconnect_recipients(selected.iter()), ctx),
            next_prompt,
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    transfer::button(id, &selected, transfer::Direction::Upload, ctx),
                    transfer::button(id, &selected, transfer::Direction::Download, ctx),
                    if page.saved.origin().is_some() {
                        recipient_sets::button(id, ctx)
                    } else {
                        View::empty()
                    },
                    host_list_export::button(id, &selected, ctx),
                )),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    environment::button(id, page, &selected, ctx),
                    Button::new()
                        .is_enabled(!shown_ids.is_empty())
                        .on_click(ctx.callback(move |_| {
                            Message::HostGroup(Action::CopyRoster(id, shown_ids.clone()))
                        }))
                        .content("Copy shown roster"),
                )),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    CheckBox::new()
                        .automation_name("Show last command output")
                        .is_checked(page.roster_preferences.show_last_output)
                        .on_is_checked_changed(ctx.callback(move |show| {
                            Message::HostGroup(Action::ShowLastOutput(id, show))
                        }))
                        .content("Show last command output"),
                    CheckBox::new()
                        .automation_name("Show current terminal line")
                        .is_checked(page.roster_preferences.show_terminal_line)
                        .on_is_checked_changed(ctx.callback(move |show| {
                            Message::HostGroup(Action::ShowTerminalLine(id, show))
                        }))
                        .content("Show current terminal line"),
                )),
            if page.roster_copy_feedback.is_empty() {
                View::empty()
            } else {
                TextBlock::new()
                    .text(&page.roster_copy_feedback)
                    .text_wrapping(TextWrapping::Wrap)
                    .into()
            },
        ));
        let instructions = if !page.draft.is_empty() && !valid_text {
            "Enter one command line of up to 4096 bytes.".into()
        } else if let Some(message) = canary_error {
            message.into()
        } else if selected.is_empty() {
            "Select panes with a fresh shell marker or a configured prompt match.".into()
        } else {
            let count = selected.len();
            let hosts = if count == 1 { "host" } else { "hosts" };
            if page.roster_preferences.require_all_ready && ready < count {
                format!("{ready} of {count} selected {hosts} ready · waiting for all")
            } else {
                format!("{ready} of {count} selected {hosts} ready")
            }
        };
        let entry = TextBox::new()
            .element_ref(&page.command_editor)
            .automation_name("Group command")
            .placeholder_text("Enter one shell command")
            .text(&page.draft)
            .on_text_changed(ctx.callback(move |text| Message::HostGroup(Action::Edit(id, text))));
        let offer = page.command_offer(
            text,
            command_order::recipients(page, shown.iter().copied())
                .into_iter()
                .cloned()
                .collect(),
            page.canary,
        );
        let (body_key, body) = if page.preview_command_order {
            (
                "command-order",
                command_order::preview(id, page, &offer, ctx),
            )
        } else {
            (
                "roster",
                StackPanel::new().spacing(10.0).keyed_children(details),
            )
        };
        let preview = command_order::button(
            id,
            page.preview_command_order,
            !offer.targets.is_empty(),
            ctx,
        );
        let send_recorded = recorded_send::button(id, offer.clone(), enabled, ctx);
        let send_alerted = alerted_send::button(id, offer.clone(), enabled, ctx);
        let send = Button::new()
            .style(ButtonStyle::Accent)
            .is_enabled(enabled)
            .on_click(ctx.callback(move |_| Message::HostGroup(Action::Send(id, offer.clone()))))
            .content(send_label);
        let saved_commands = Button::new()
            .on_click(ctx.callback(move |_| Message::HostGroup(Action::OpenSavedCommands(id))))
            .content("Saved commands…");
        let recall = ctx.sender();
        let recent_commands = Button::new()
            .automation_name("Recent commands…")
            .key_accelerators(KeyAccelerators::new([KeyAccelerator::new(
                AcceleratorKey::R,
                AcceleratorModifiers::Control,
                move || {
                    recall.send(Message::HostGroup(Action::OpenRecentCommands(id)));
                },
            )]))
            .on_click(ctx.callback(move |_| Message::HostGroup(Action::OpenRecentCommands(id))))
            .content("Recent commands…  Ctrl+R");
        let save_command = page.command_save.button(id, &page.draft, ctx);
        let restore = if page.previous_draft.is_some() {
            Button::new()
                .on_click(ctx.callback(move |_| Message::HostGroup(Action::RestoreDraft(id))))
                .content("Restore draft")
        } else {
            View::empty()
        };
        let hidden_selection = if hidden_count == 0 {
            View::empty()
        } else {
            let hosts = if hidden_count == 1 {
                "host is"
            } else {
                "hosts are"
            };
            TextBlock::new()
                .text(format!(
                    "{hidden_count} selected {hosts} hidden by the filter."
                ))
                .text_wrapping(TextWrapping::Wrap)
                .into()
        };
        Grid::new()
            .margin(Thickness::uniform(16.0))
            .rows([
                GridLength::Auto,
                GridLength::Auto,
                GridLength::Auto,
                GridLength::Auto,
                GridLength::STAR,
            ])
            .children((
                TextBlock::new().font_size(24.0).text(LABEL),
                self.saved_host_group_toolbar(id, page, ctx),
                filter,
                StackPanel::new().grid_row(3).spacing(7.0).children((
                    entry,
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(12.0)
                        .children((
                            parallel_limit::picker(
                                "Parallel SSH commands",
                                page.roster_preferences.command_limit,
                                !page.recorded_send.pending(),
                                move |limit| Message::HostGroup(Action::CommandLimit(id, limit)),
                                ctx,
                            ),
                            CheckBox::new()
                                .automation_name("Stop queue on failure")
                                .is_checked(page.roster_preferences.stop_command_queue_on_failure)
                                .is_enabled(!page.recorded_send.pending() && page.orders_commands())
                                .on_is_checked_changed(ctx.callback(move |stop| {
                                    Message::HostGroup(Action::StopCommandQueueOnFailure(id, stop))
                                }))
                                .content("Stop queue on failure"),
                            preview,
                        )),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(12.0)
                        .children((
                            command_queue::interval_picker(id, page, ctx),
                            command_queue::expiry_picker(id, page, ctx),
                        )),
                    command_queue::canary_picker(id, page, &selected, ctx),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            send,
                            send_recorded,
                            send_alerted,
                            recent_commands,
                            saved_commands,
                            save_command,
                            restore,
                        )),
                    page.command_save.view(id, ctx),
                    page.recorded_send.feedback(),
                    page.alerted_send.feedback(),
                    retry::feedback(page),
                    hidden_selection,
                    Grid::new()
                        .column_spacing(12.0)
                        .columns([GridLength::Auto, GridLength::STAR])
                        .children((
                            CheckBox::new()
                                .automation_name(REQUIRE_READY)
                                .is_checked(page.roster_preferences.require_all_ready)
                                .is_enabled(!page.recorded_send.pending())
                                .on_is_checked_changed(ctx.callback(move |require| {
                                    Message::HostGroup(Action::RequireAllReady(id, require))
                                }))
                                .content(REQUIRE_READY),
                            TextBlock::new()
                                .grid_column(1)
                                .vertical_alignment(VerticalAlignment::Center)
                                .text(instructions)
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                )),
                Grid::new()
                    .grid_row(4)
                    .keyed_children([KeyedView::new(body_key, ScrollViewer::new().content(body))]),
            ))
    }

    fn host_group_results(
        &self,
        id: TabId,
        page: &HostGroup,
        ctx: &ViewContext<Self>,
        roster_shown: &[&Target],
        selected_shown: &BTreeSet<u64>,
        details: &mut Vec<KeyedView>,
    ) {
        let Some(index) = page
            .submissions
            .iter()
            .position(|item| item.id == page.viewing)
        else {
            return;
        };
        let submission = &page.submissions[index];
        let submission_id = submission.id;
        let now = Instant::now();
        let (summary, shown) = page.result_rows(submission, now);
        let interrupt_targets: Vec<_> = shown
            .iter()
            .filter(|(delivery, _)| selected_shown.contains(&delivery.target.pane))
            .filter_map(|(delivery, _)| {
                delivery
                    .interrupt_target()
                    .map(|link| (delivery.target.pane, link))
            })
            .collect();
        let interrupt_label = format!("Interrupt selected ({})", interrupt_targets.len());
        let interrupt = Button::new()
            .automation_name("Interrupt selected")
            .is_enabled(!interrupt_targets.is_empty())
            .on_click(ctx.callback(move |_| {
                Message::HostGroup(Action::Interrupt(
                    id,
                    submission_id,
                    interrupt_targets.clone(),
                ))
            }))
            .content(interrupt_label);
        let navigate = |label, destination: Option<&Submission>| {
            let destination = destination.map(|item| item.id);
            Button::new()
                .is_enabled(destination.is_some())
                .on_click(ctx.callback(move |_| {
                    destination.map_or(Message::ControlIdle, |next| {
                        Message::HostGroup(Action::ViewSubmission(id, next))
                    })
                }))
                .content(label)
        };
        let result_filter = ComboBox::new()
            .automation_name("Filter submission results")
            .items_source(progress::FILTERS.map(|(_, label)| label))
            .selected_index(
                progress::FILTERS
                    .iter()
                    .position(|&(filter, _)| filter == page.result_filter),
            )
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index.and_then(|index| progress::FILTERS.get(index)).map_or(
                    Message::ControlIdle,
                    |&(filter, _)| {
                        Message::HostGroup(Action::FilterResults(id, submission_id, filter))
                    },
                )
            }));
        let result_order = ComboBox::new()
            .automation_name("Sort submission results")
            .items_source(progress::RESULT_ORDERS.map(|(_, label)| label))
            .selected_index(
                progress::RESULT_ORDERS
                    .iter()
                    .position(|&(order, _)| order == page.result_order),
            )
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index
                    .and_then(|index| progress::RESULT_ORDERS.get(index))
                    .map_or(Message::ControlIdle, |&(order, _)| {
                        Message::HostGroup(Action::OrderResults(id, submission_id, order))
                    })
            }));
        let recipient_search = Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                TextBox::new()
                    .automation_name("Search result recipients")
                    .placeholder_text("Search by profile, pane, or SSH address")
                    .text(page.recipient_search.query())
                    .on_text_changed(ctx.callback(move |query| {
                        Message::HostGroup(Action::SearchRecipients(id, submission_id, query))
                    })),
                Button::new()
                    .grid_column(1)
                    .is_enabled(!page.recipient_search.query().is_empty())
                    .on_click(ctx.callback(move |_| {
                        Message::HostGroup(Action::SearchRecipients(
                            id,
                            submission_id,
                            String::new(),
                        ))
                    }))
                    .content("Clear recipient search"),
            ));
        let result_count = if shown.len() != summary.total() {
            TextBlock::new()
                .text(format!(
                    "Showing {} of {} recipients",
                    shown.len(),
                    summary.total()
                ))
                .into()
        } else {
            View::empty()
        };
        let copy = |label, offered: Option<Arc<[u64]>>| {
            Button::new()
                .on_click(ctx.callback(move |_| {
                    Message::HostGroup(Action::CopyResults(id, submission_id, offered.clone()))
                }))
                .content(label)
        };
        let save = |label, format, offered: Option<Arc<[u64]>>| {
            Button::new()
                .is_enabled(self.export.pending.is_none())
                .on_click(ctx.callback(move |_| {
                    Message::HostGroup(Action::SaveResults(
                        id,
                        submission_id,
                        format,
                        offered.clone(),
                    ))
                }))
                .content(label)
        };
        let shown_exports = if !shown.is_empty() && shown.len() < summary.total() {
            let offered: Arc<[u64]> = shown.iter().map(|(row, _)| row.target.pane).collect();
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    copy("Copy shown results", Some(offered.clone())),
                    save(
                        text_export::GROUP_SHOWN_REPORT_LABEL,
                        text_export::GroupReportFormat::Text,
                        Some(offered.clone()),
                    ),
                    save(
                        text_export::GROUP_SHOWN_JSON_LABEL,
                        text_export::GroupReportFormat::Json,
                        Some(offered),
                    ),
                ))
        } else {
            View::empty()
        };
        details.push(KeyedView::new(
            "results",
            StackPanel::new().spacing(4.0).children((
                TextBlock::new()
                    .font_size(18.0)
                    .text(format!("Submitted · {}", submission.content.label()))
                    .text_trimming(TextTrimming::CharacterEllipsis),
                submission
                    .dispatch_policy
                    .as_ref()
                    .map_or_else(View::empty, |policy| {
                        TextBlock::new()
                            .text(policy.caption())
                            .text_wrapping(TextWrapping::Wrap)
                            .into()
                    }),
                TextBlock::new()
                    .text(summary.caption())
                    .text_wrapping(TextWrapping::Wrap),
                StackPanel::new().spacing(4.0).children((
                    output_recording::status_view(id, submission, ctx),
                    submission
                        .transcript
                        .output_alert_rule()
                        .as_deref()
                        .map_or_else(View::empty, |rule| {
                            live_output::armed_alert_view(id, submission_id, rule, ctx)
                        }),
                )),
                ProgressBar::new()
                    .automation_name("SSH group submission progress")
                    .maximum(summary.total().max(1) as f64)
                    .value(summary.settled() as f64),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        navigate(
                            "Previous submission",
                            index.checked_sub(1).and_then(|i| page.submissions.get(i)),
                        ),
                        navigate("Next submission", page.submissions.get(index + 1)),
                        Button::new()
                            .on_click(
                                ctx.callback(move |_| {
                                    Message::HostGroup(Action::OpenRunPicker(id))
                                }),
                            )
                            .content("Find run…"),
                        TextBlock::new().text(format!(
                            "{} of {}",
                            index + 1,
                            page.submissions.len()
                        )),
                    )),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        if submission.content.command().is_some() {
                            Button::new()
                                .on_click(ctx.callback(move |_| {
                                    Message::HostGroup(Action::LoadCommand(id, submission_id))
                                }))
                                .content("Load command")
                        } else {
                            View::empty()
                        },
                        copy("Copy results", None),
                        save(
                            text_export::GROUP_REPORT_LABEL,
                            text_export::GroupReportFormat::Text,
                            None,
                        ),
                        Button::new()
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(Action::LiveOutput(
                                    id,
                                    submission_id,
                                    live_output::Action::Open,
                                ))
                            }))
                            .content("Live output…"),
                        interrupt,
                    )),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((result_filter, result_order)),
                recipient_search,
                result_count,
                shown_exports,
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(Action::Compare(
                                    id,
                                    submission_id,
                                    compare::Action::Open,
                                ))
                            }))
                            .content("Compare results…"),
                        Button::new()
                            .is_enabled(index > 0)
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(Action::Compare(
                                    id,
                                    submission_id,
                                    compare::Action::RunOverview(compare::RunOverviewAction::Open),
                                ))
                            }))
                            .content("Compare runs…"),
                        save(
                            text_export::GROUP_JSON_LABEL,
                            text_export::GroupReportFormat::Json,
                            None,
                        ),
                        completion::button(id, submission, ctx),
                    )),
                command_queue::controls(id, submission, ctx),
                queue_activity::view(id, submission, ctx),
                retry::button(
                    id,
                    page,
                    submission,
                    roster_shown.iter().map(|target| target.pane),
                    ctx,
                ),
                if page.copy_feedback.is_empty() {
                    View::empty()
                } else {
                    TextBlock::new()
                        .text(&page.copy_feedback)
                        .text_wrapping(TextWrapping::Wrap)
                        .into()
                },
            )),
        ));
        if let Some(expectation) = submission.transcript.output_expectation()
            && let Some(view) = live_output::output_expectation_view(&expectation)
        {
            details.push(KeyedView::new("output-expectation", view));
        }
        if shown.is_empty() {
            details.push(KeyedView::new(
                "results-empty",
                TextBlock::new().text("No recipients match this result filter."),
            ));
        }
        details.extend(shown.into_iter().map(|(delivery, execution)| {
            KeyedView::new(
                format!("delivery-{}-{}", submission.id, delivery.target.pane),
                self.host_group_result_row(id, delivery, submission, execution.as_ref(), now, ctx),
            )
        }));
    }

    fn host_group_result_row(
        &self,
        id: TabId,
        delivery: &Delivery,
        submission: &Submission,
        execution: Option<&GroupExecution>,
        now: Instant,
        ctx: &ViewContext<Self>,
    ) -> View {
        let pane = delivery.target.pane;
        let mut rows = vec![KeyedView::new(
            "delivery",
            TextBlock::new()
                .text(delivery.description())
                .text_wrapping(TextWrapping::Wrap),
        )];
        if let Some(command) = submission.content.host_command(pane) {
            rows.push(KeyedView::new(
                "command",
                TextBlock::new()
                    .text(command)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if let Some(result) = execution {
            let name = &delivery.target.name;
            let caption = execution_caption(result);
            rows.push(KeyedView::new(
                "execution",
                TextBlock::new()
                    .automation_name(format!("{name} · {caption}"))
                    .text(caption.as_ref())
                    .text_wrapping(TextWrapping::Wrap),
            ));
            if let Some(timing) = execution_time_caption(result, now) {
                rows.push(KeyedView::new(
                    "shell-time",
                    TextBlock::new()
                        .automation_id(format!("ssh-group-result-time-{pane}"))
                        .automation_name(format!("{name} · {timing}"))
                        .text(timing),
                ));
            }
            if let GroupExecution::Finished(result) = result {
                let link = result.link;
                let (preview, truncated) = match &result.output {
                    GroupOutput::Captured { text, .. } if text.is_empty() => {
                        ("No output captured".into(), false)
                    }
                    GroupOutput::Captured { text, truncated } => {
                        (Cow::Borrowed(text.as_str()), *truncated)
                    }
                    GroupOutput::Unavailable(reason) => {
                        (Cow::Owned(format!("Output unavailable · {reason}")), false)
                    }
                };
                rows.push(KeyedView::new(
                    "output",
                    TextBlock::new()
                        .text(preview.trim_end_matches(['\r', '\n']))
                        .text_wrapping(TextWrapping::Wrap),
                ));
                if truncated {
                    rows.push(KeyedView::new(
                        "truncated",
                        TextBlock::new().text("Output preview truncated"),
                    ));
                }
                if self
                    .history_frame(pane)
                    .is_some_and(|frame| frame.has_command(link))
                {
                    rows.push(KeyedView::new(
                        "view-output",
                        Button::new()
                            .automation_name(format!("View output for pane {pane}"))
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(Action::ViewOutput(id, pane, link))
                            }))
                            .content("View full output"),
                    ));
                }
                if let Some(button) = self.compare_run_button(id, submission, pane, ctx) {
                    rows.push(KeyedView::new("compare-run", button));
                }
            }
        }
        if let Some(caption) = delivery.interrupt_caption() {
            rows.push(KeyedView::new(
                "interrupt",
                TextBlock::new()
                    .automation_name(format!("{} · {caption}", delivery.target.name))
                    .text(caption.as_ref())
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if matches!(delivery.status, DeliveryStatus::Queued(_))
            && submission
                .command_queue
                .as_ref()
                .is_some_and(|queue| !queue.is_canary(pane))
        {
            let run = submission.id;
            rows.push(KeyedView::new(
                "skip",
                Button::new()
                    .automation_name(format!("Skip queued pane {pane}"))
                    .on_click(ctx.callback(move |_| {
                        Message::HostGroup(Action::CommandQueue(
                            id,
                            run,
                            command_queue::Action::SkipHost(pane),
                        ))
                    }))
                    .content("Skip host"),
            ));
        }
        if self.tab_for_pane(pane).is_some() {
            rows.push(KeyedView::new(
                "focus",
                Button::new()
                    .automation_name(format!("Focus result pane {pane}"))
                    .on_click(ctx.callback(move |_| Message::HostGroup(Action::Visit(id, pane))))
                    .content("Focus pane"),
            ));
        }
        StackPanel::new().spacing(4.0).keyed_children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_sort_uses_stable_ties_and_keeps_recipient_order() {
        let targets = [
            ("Zulu", TargetState::InUse),
            ("ALPHA", TargetState::Offline),
            ("alpha", TargetState::ReadOnly),
            ("Beta", TargetState::Unverified),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (label, state))| Target {
            pane: index as u64 + 1,
            name: label.into(),
            names: filter::Names::new(label, "lab.example", "ops", 22),
            profile: None,
            connected: !matches!(state, TargetState::Offline),
            state,
        })
        .collect::<Vec<_>>();
        let mut group = HostGroup {
            selected: BTreeSet::from([1, 4]),
            ..Default::default()
        };
        group.roster_preferences.command_limit = SshGroupParallelLimit::One;
        for (order, expected) in [
            (RosterOrder::Tabs, [1, 2, 3, 4]),
            (RosterOrder::Label, [2, 3, 4, 1]),
            (RosterOrder::CheckedFirst, [1, 4, 2, 3]),
            (RosterOrder::AttentionFirst, [2, 3, 4, 1]),
        ] {
            let mut shown: Vec<_> = group.shown(&targets).collect();
            sort_roster(order, &mut shown, &group.selected);
            assert!(shown.iter().map(|target| target.pane).eq(expected));
            assert!(
                group
                    .shown(&targets)
                    .filter(|target| group.selected.contains(&target.pane))
                    .map(|target| target.pane)
                    .eq([1, 4])
            );
            group.roster_preferences.order = order;
            assert!(
                command_order::recipients(&group, group.shown(&targets))
                    .iter()
                    .map(|target| target.pane)
                    .eq(expected
                        .into_iter()
                        .filter(|pane| group.selected.contains(pane)))
            );
        }
        group.roster_preferences.order = RosterOrder::Label;
        group.roster_preferences.command_limit = SshGroupParallelLimit::Unlimited;
        assert!(
            command_order::recipients(&group, group.shown(&targets))
                .iter()
                .map(|target| target.pane)
                .eq([1, 4])
        );
        group.roster_preferences.command_interval = SshGroupCommandInterval::HalfSecond;
        assert!(
            command_order::recipients(&group, group.shown(&targets))
                .iter()
                .map(|target| target.pane)
                .eq([4, 1])
        );
        group.roster_preferences.command_interval = SshGroupCommandInterval::None;
        group.canary = Some(4);
        assert!(
            command_order::recipients(&group, group.shown(&targets))
                .iter()
                .map(|target| target.pane)
                .eq([4, 1])
        );
    }

    #[test]
    fn group_send_readiness_requires_a_recipient_and_respects_the_tab_option() {
        let mut group = HostGroup::default();
        assert!(!group.readiness_allows_send(0, 0));
        assert!(!group.readiness_allows_send(0, 2));
        assert!(group.readiness_allows_send(1, 2));

        group.roster_preferences.require_all_ready = true;
        assert!(!group.readiness_allows_send(0, 0));
        assert!(!group.readiness_allows_send(1, 2));
        assert!(group.readiness_allows_send(2, 2));
    }

    #[test]
    fn roster_filter_matches_profile_pane_and_address_and_scopes_selection() {
        let roster = [
            Target {
                pane: 7,
                name: "Primary database · ops@db.example:2222 · Pane 7".into(),
                names: filter::Names::new("Primary database", "db.example", "ops", 2222),
                profile: Some("East Prod".into()),
                connected: false,
                state: TargetState::Offline,
            },
            Target {
                pane: 8,
                name: "Replica · ops@db.example:22 · Pane 8".into(),
                names: filter::Names::new("Replica", "db.example", "ops", 22),
                profile: Some("West Prod".into()),
                connected: false,
                state: TargetState::Offline,
            },
        ];
        let mut group = HostGroup {
            selected: BTreeSet::from([7, 8]),
            ..Default::default()
        };
        for query in [" EAST    PRIMARY ", "ops@db.example:2222", "pane 7"] {
            group.filter.set_query(query.into());
            assert_eq!(group.selected_shown(&roster), BTreeSet::from([7]));
        }
        group.filter.set_query("missing".into());
        assert!(group.selected_shown(&roster).is_empty());
        group.filter.clear();
        assert_eq!(group.selected_shown(&roster), BTreeSet::from([7, 8]));
    }

    #[test]
    fn a_named_roster_scopes_bulk_targets_and_deselects_external_panes_on_return() {
        let targets = [1, 2, 3].map(|pane| Target {
            pane,
            name: format!("Host {pane}"),
            names: Default::default(),
            profile: None,
            connected: true,
            state: TargetState::Unverified,
        });
        let mut group = HostGroup {
            roster: Some(Box::new(MemberRoster {
                panes: BTreeSet::from([1, 3]),
                show_window: false,
                ..Default::default()
            })),
            selected: BTreeSet::from([1, 3]),
            ..Default::default()
        };
        assert_eq!(
            group
                .shown(&targets)
                .map(|target| target.pane)
                .collect::<Vec<_>>(),
            [1, 3]
        );
        group.show_window_roster(true);
        group.selected.insert(2);
        group.filter.set_query("Host 2".into());
        assert_eq!(group.selected_shown(&targets), BTreeSet::from([2]));
        group.show_window_roster(false);
        assert!(group.selected_shown(&targets).is_empty());
        assert_eq!(group.selected, BTreeSet::from([1, 3]));
        group.filter.clear();
        assert_eq!(group.selected_shown(&targets), BTreeSet::from([1, 3]));
    }

    #[test]
    fn state_counts_keep_query_matches_hidden_by_the_state_filter() {
        let roster = [
            (1, "web-east", TargetState::HostKey(1)),
            (2, "web-west", TargetState::Connecting),
            (3, "web-external", TargetState::Connecting),
            (4, "db-east", TargetState::Failed(None)),
        ]
        .map(|(pane, name, state)| Target {
            pane,
            name: name.into(),
            names: filter::Names::new(name, "lab.example", "ops", 22),
            profile: None,
            connected: false,
            state,
        });
        let mut group = HostGroup {
            roster: Some(Box::new(MemberRoster {
                panes: BTreeSet::from([1, 2, 4]),
                ..Default::default()
            })),
            selected: BTreeSet::from([1]),
            ..Default::default()
        };
        group.filter.set_wildcards(true);
        group.filter.set_query("web-*".into());
        group.filter.set_status(RosterStatus::Connecting);
        let summarize = |group: &HostGroup| {
            roster_state_summary(
                &group.matching_query(&roster).collect::<Vec<_>>(),
                group.shown(&roster).count(),
            )
        };
        assert_eq!(
            summarize(&group).as_deref(),
            Some(
                "Showing 1 of 2 matching hosts · Connecting (1) · Host key verification required (1)"
            )
        );
        group.show_window_roster(true);
        assert_eq!(
            summarize(&group).as_deref(),
            Some(
                "Showing 2 of 3 matching hosts · Connecting (2) · Host key verification required (1)"
            )
        );
        group.filter.set_query("cache-*".into());
        assert_eq!(summarize(&group), None);
    }

    #[test]
    fn submission_history_retains_the_latest_eight_and_selects_the_newest() {
        let mut group = HostGroup::default();
        for id in 1..=10 {
            group.submit(
                SubmissionContent::Command(format!("command {id}").into()),
                Vec::new(),
                GroupTranscript::default(),
            );
        }
        assert_eq!(group.submissions.len(), SUBMISSION_LIMIT);
        assert_eq!(group.submissions.front().unwrap().id, 3);
        assert_eq!(group.submissions.back().unwrap().id, group.viewing);
    }

    #[test]
    fn run_picker_keeps_repeated_commands_and_searches_full_text_or_exact_ids() {
        let mut group = HostGroup::default();
        let repeated = format!("{} ÉTAT", "long output ".repeat(90));
        for id in 1..=10 {
            let content = match id {
                3 | 10 => SubmissionContent::Command(repeated.as_str().into()),
                9 => SubmissionContent::Environment(BTreeMap::new()),
                _ => SubmissionContent::Command(format!("command {id}").into()),
            };
            group.submit(content, Vec::new(), GroupTranscript::default());
        }
        let ids = |query: &str| {
            group
                .matching_submissions(query)
                .map(|run| run.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(" état "), [10, 3]);
        assert_eq!(ids("GROUP VARIABLES"), [9]);
        assert_eq!(ids(" #10 "), [10]);
        assert!(ids("#1").is_empty());
        assert_eq!(ids(""), [10, 9, 8, 7, 6, 5, 4, 3]);
    }

    #[test]
    fn loading_previous_commands_preserves_the_draft_and_current_recipient_scope() {
        let mut group = HostGroup {
            draft: "in progress".into(),
            selected: BTreeSet::from([11, 12]),
            ..Default::default()
        };
        group.filter.set_query("east".into());
        group.submit(
            SubmissionContent::Command("show  inventory".into()),
            Vec::new(),
            GroupTranscript::default(),
        );
        group.submit(
            SubmissionContent::Command("show revision".into()),
            Vec::new(),
            GroupTranscript::default(),
        );

        assert!(!group.load_command(1));
        assert_eq!(group.draft, "in progress");
        group.viewing = 1;
        assert!(group.load_command(1));
        assert_eq!(group.draft, "show  inventory");
        group.viewing = 2;
        assert!(group.load_command(2));
        assert_eq!(group.draft, "show revision");
        assert_eq!(group.selected, BTreeSet::from([11, 12]));
        assert_eq!(group.filter.query(), "east");
        assert_eq!(group.submissions.len(), 2);

        assert!(group.restore_draft());
        assert_eq!(group.draft, "in progress");
        assert!(!group.restore_draft());
        assert!(group.load_command(2));
        group.submit(
            SubmissionContent::Command("show revision".into()),
            Vec::new(),
            GroupTranscript::default(),
        );
        assert!(!group.restore_draft());
    }
}
