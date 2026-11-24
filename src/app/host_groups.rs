use super::*;
use std::{borrow::Cow, collections::BTreeSet};
use tokio::sync::{oneshot, oneshot::error::TryRecvError, watch};
use w11term::events::{
    CommandLink, GroupExecution, GroupInterruptTarget, GroupOutput, GroupTranscript, PromptToken,
    ShellPrompt,
};
mod address;
mod compare;
mod completion;
mod environment;
mod filter;
mod host_list_export;
mod import;
mod live_output;
mod membership;
mod output_recording;
mod progress;
mod prompt_setup;
mod recent_commands;
mod recipient_sets;
mod reconnect;
mod recorded_send;
mod report;
mod retry;
mod reuse;
mod save_command;
mod saved;
mod transfer;

pub use recipient_sets::{Action as RecipientSetAction, Offer as RecipientSetOffer};

pub(super) const LABEL: &str = "SSH host group";
pub(super) const NEW_TAB_LABEL: &str = "New SSH group tab";
pub(super) const RECENT_COMMANDS_LABEL: &str = "Recent group commands…";
const ACK_TIMEOUT: Duration = Duration::from_secs(10);
const SUBMISSION_LIMIT: usize = 8;

#[derive(Default)]
pub(super) struct HostGroup {
    saved: Box<saved::SavedUi>,
    roster: Option<Box<MemberRoster>>,
    selected: BTreeSet<u64>,
    filter: filter::Filter,
    draft: String,
    previous_draft: Option<DraftBackup>,
    command_editor: ElementRef<TextBox>,
    focus_loaded_command: bool,
    command_save: save_command::State,
    recorded_send: recorded_send::State,
    submissions: VecDeque<Submission>,
    recent_commands: recent_commands::RecentCommands,
    viewing: u64,
    next_submission: u64,
    copy_feedback: String,
    result_filter: progress::Filter,
    result_order: progress::ResultOrder,
    live_output: Option<Box<live_output::State>>,
    comparison: compare::State,
    import: import::State,
    host_list_export: Box<host_list_export::State>,
    reconnect_feedback: String,
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
    saved_recordings: output_recording::saved::State,
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

#[derive(Clone, Copy)]
enum TargetState {
    Offline,
    Restartable(ExitedPane),
    Authentication,
    ReadOnly,
    Alternate,
    Unverified,
    InUse,
    Ready(PromptToken),
    Matched(PromptToken),
}

impl TargetState {
    fn is_ready(self) -> bool {
        self.prompt().is_some()
    }

    fn prompt(self) -> Option<PromptToken> {
        match self {
            Self::Ready(prompt) | Self::Matched(prompt) => Some(prompt),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Offline => "Disconnected",
            Self::Restartable(_) => "Disconnected · reconnect available",
            Self::Authentication => "Authentication required",
            Self::ReadOnly => "Read-only",
            Self::Alternate => "Alternate screen",
            Self::Unverified => "Prompt unverified",
            Self::InUse => "Waiting for a fresh prompt",
            Self::Ready(_) => "Ready",
            Self::Matched(_) => "Ready · prompt match",
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
    Pending(Instant, oneshot::Receiver<Result<(), String>>),
    Sent,
    Rejected(String),
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
            DeliveryStatus::Pending(..) => "Sending".to_owned(),
            DeliveryStatus::Sent => "Sent to SSH channel".to_owned(),
            DeliveryStatus::Rejected(reason) => format!("Not sent · {reason}"),
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

    fn shown<'a>(&self, targets: &'a [Target]) -> impl Iterator<Item = &'a Target> {
        targets
            .iter()
            .filter(move |target| self.includes_pane(target.pane) && self.filter.matches(target))
    }

    fn selected_shown(&self, targets: &[Target]) -> BTreeSet<u64> {
        self.shown(targets)
            .map(|target| target.pane)
            .filter(|pane| self.selected.contains(pane))
            .collect()
    }

    fn sending(&self) -> bool {
        self.submissions.back().is_some_and(|submission| {
            submission
                .results
                .iter()
                .any(|item| matches!(item.status, DeliveryStatus::Pending(..)))
        })
    }

    fn submit(
        &mut self,
        content: SubmissionContent,
        results: Vec<Delivery>,
        transcript: GroupTranscript,
    ) -> &mut Submission {
        transcript.finish_dispatch();
        self.next_submission += 1;
        self.viewing = self.next_submission;
        self.result_filter = progress::Filter::All;
        if let SubmissionContent::Command(command) = &content {
            self.recent_commands.record(self.viewing, command.clone());
            self.previous_draft = None;
        }
        self.copy_feedback.clear();
        self.retry_feedback.clear();
        self.submissions.push_back(Submission {
            id: self.viewing,
            content,
            results,
            transcript,
            recording: Default::default(),
            saved_recordings: Default::default(),
        });
        if self.submissions.len() > SUBMISSION_LIMIT {
            self.submissions.pop_front();
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
    OpenNamedGroup { name: String, members: Vec<String> },
    Saved(TabId, saved::Action),
    Tick,
    Edit(TabId, String),
    Filter(TabId, String),
    FilterWildcards(TabId, bool),
    ShowWindowRoster(TabId, bool),
    Membership(TabId, membership::Action),
    RecipientSets(TabId, recipient_sets::Action),
    Reuse(TabId, reuse::Action),
    Select(TabId, u64, bool),
    SelectShown(TabId, BTreeSet<u64>, ShownSelection),
    Visit(TabId, u64),
    ViewSubmission(TabId, u64),
    NotifyCompletion(TabId, u64, bool),
    LiveOutput(TabId, u64, live_output::Action),
    RecordOutput(TabId, u64, output_recording::Action),
    RecordedSend(TabId, recorded_send::Action),
    Compare(TabId, u64, compare::Action),
    Import(TabId, import::Action),
    HostListExport(TabId, host_list_export::Action),
    Reconnect(TabId, reconnect::Request),
    FilterResults(TabId, u64, progress::Filter),
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
    CopyResults(TabId, u64),
    SaveResults(TabId, u64, text_export::GroupReportFormat),
    ViewOutput(TabId, u64, CommandLink),
    Interrupt(TabId, u64, Vec<(u64, GroupInterruptTarget)>),
    Send(TabId, Arc<str>, Vec<Target>),
}

impl TerminalApp {
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
            page: Page::HostGroup(HostGroup {
                roster: name.map(|_| Box::default()),
                ..Default::default()
            }),
        })
    }

    pub(super) fn host_group_command_target(&self) -> Option<TabId> {
        let id = self.tabs.active_id()?;
        match &self.tabs.get(id)?.page {
            Page::HostGroup(page)
                if !page.saved.is_editing()
                    && page.live_output.is_none()
                    && !page.comparison.is_open()
                    && !page.import.is_open()
                    && !page.host_list_export.is_open()
                    && !page.edit_membership_open()
                    && !page.saved.reuse.is_open()
                    && !page.prompt_setup.is_open()
                    && !page.transfer.is_open()
                    && !page.environment.is_open() =>
            {
                Some(id)
            }
            _ => None,
        }
    }

    fn start_host_group_panes(
        &mut self,
        id: TabId,
        mut selected: BTreeSet<u64>,
        targets: impl IntoIterator<Item = (Option<String>, SshOptions)>,
        ctx: &ComponentContext<Self>,
    ) {
        for (profile, ssh) in targets {
            let source = Source::Ssh(ssh);
            let label = profile.clone().unwrap_or_else(|| source_label(&source));
            let tab = self.make_live_tab(label, source, profile);
            if let Some(pane) = tab.pane_id() {
                selected.insert(pane);
            }
            self.tabs.insert(tab);
        }
        self.tabs.select(id);
        if let Some(page) = self.host_group_mut(id) {
            if let Some(roster) = &mut page.roster {
                roster.panes.extend(selected.iter().copied());
            }
            page.selected = selected;
            page.filter.clear();
            page.saved.feedback.clear();
            page.import.feedback.clear();
            page.reconnect_feedback.clear();
        }
        self.sync_selection(ctx);
        self.activate(ctx);
    }

    pub(super) fn update_host_group_timer(&mut self, ctx: &ComponentContext<Self>) {
        if matches!(
            self.tabs.active().map(|tab| &tab.page),
            Some(Page::HostGroup(_))
        ) {
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
                let state = if live.device_busy || live.session.prompts.current().is_some() {
                    TargetState::Authentication
                } else if !connected {
                    entry
                        .value
                        .exited_pane()
                        .map_or(TargetState::Offline, TargetState::Restartable)
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
        transcript: &GroupTranscript,
    ) -> Delivery {
        let host = transcript.host(target.pane);
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
                        if live
                            .session
                            .input
                            .send(Command::SendGroupCommand {
                                prompt,
                                text,
                                reply,
                                execution: observe,
                                transcript: host,
                            })
                            .is_ok()
                        {
                            execution = Some(observed);
                            DeliveryStatus::Pending(Instant::now(), received)
                        } else {
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

    fn group_send_matches(
        &self,
        id: TabId,
        text: &str,
        targets: &[Target],
        verify_prompts: bool,
    ) -> bool {
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
        let roster = self.host_group_targets();
        let selection = targets.iter().map(|target| target.pane).collect();
        !page.sending()
            && !targets.is_empty()
            && page.selected_shown(&roster) == selection
            && page.draft == text
            && w11term::saved_commands::valid_command_text(text)
            && (!verify_prompts
                || targets.iter().all(|reviewed| {
                    roster.iter().any(|current| {
                        current.pane == reviewed.pane
                            && current.state.prompt() == reviewed.state.prompt()
                            && current.name == reviewed.name
                    })
                }))
    }

    fn submit_group_command(
        &mut self,
        id: TabId,
        text: Arc<str>,
        targets: Vec<Target>,
        transcript: GroupTranscript,
        recording: output_recording::State,
    ) {
        let results = targets
            .into_iter()
            .map(|target| self.dispatch_to_host(target, text.clone(), &transcript))
            .collect();
        if let Some(page) = self.host_group_mut(id) {
            page.submit(SubmissionContent::Command(text), results, transcript)
                .recording = recording;
        }
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
                            && page.result_filter.includes(progress::Outcome::of(delivery))
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
            Action::Saved(id, action) => self.handle_saved_host_group(id, action, ctx),
            Action::Import(id, action) => self.handle_host_import(id, action, ctx),
            Action::Membership(id, action) => self.handle_group_membership(id, action),
            Action::Reuse(id, action) => self.handle_host_group_reuse(id, action, ctx),
            Action::Reconnect(id, request) => self.reconnect_host_group(id, request),
            Action::PromptSetup(id, action) => self.handle_group_prompt_setup(id, action),
            Action::HostListExport(id, action) => self.handle_host_list_export(id, action, ctx),
            Action::RecipientSets(id, action) => self.handle_recipient_sets(id, action),
            Action::Transfer(id, action) => self.handle_host_group_transfer(id, action, ctx),
            Action::Environment(id, action) => self.handle_host_group_environment(id, action),
            Action::RecordedSend(id, action) => self.handle_recorded_send(id, action, ctx),
            Action::Tick => {
                if !matches!(
                    self.tabs.active().map(|tab| &tab.page),
                    Some(Page::HostGroup(_))
                ) {
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
                        for submission in &mut page.submissions {
                            if let Some(path) = submission.recording.poll() {
                                submission.saved_recordings.add(path);
                            }
                            for delivery in &mut submission.results {
                                delivery.poll();
                            }
                        }
                    }
                }
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
            Action::Visit(id, pane) => {
                if self.host_group_mut(id).is_some()
                    && let Some(tab) = self.tab_for_pane(pane)
                    && self.tabs.select(tab)
                {
                    self.sync_selection(ctx);
                    self.activate(ctx);
                }
            }
            Action::ViewSubmission(id, submission) => {
                if self.host_group_command_target() == Some(id)
                    && let Some(page) = self.host_group_mut(id)
                    && page.submissions.iter().any(|item| item.id == submission)
                {
                    page.viewing = submission;
                    page.result_filter = progress::Filter::All;
                    page.copy_feedback.clear();
                }
            }
            Action::NotifyCompletion(id, submission, armed) => {
                self.set_group_run_completion_notice(id, submission, armed);
            }
            Action::LiveOutput(id, submission, action) => {
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
            Action::CopyResults(id, submission) => self.copy_host_group_results(id, submission),
            Action::SaveResults(id, submission, format) => {
                if self.export.pending.is_none()
                    && let Some(report) = self.capture_host_group_report(id, submission)
                {
                    self.begin_text_export(
                        text_export::Subject::HostGroupResults(submission, format),
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
            Action::Send(id, text, targets) => {
                if !self.group_send_matches(id, &text, &targets, false)
                    || self
                        .host_group_mut(id)
                        .is_none_or(|page| page.recorded_send.pending())
                {
                    return;
                }
                if let Some(page) = self.host_group_mut(id) {
                    page.recorded_send.clear_feedback();
                }
                self.submit_group_command(
                    id,
                    text,
                    targets,
                    GroupTranscript::default(),
                    Default::default(),
                );
            }
        }
    }

    fn capture_host_group_report(&self, id: TabId, submission_id: u64) -> Option<report::Report> {
        if self.tabs.active_id() != Some(id) {
            return None;
        }
        let Page::HostGroup(page) = &self.tabs.get(id)?.page else {
            return None;
        };
        let submission = page.viewed_submission(submission_id)?;
        Some(report::Report::capture(submission, |pane, link| {
            self.history_frame(pane)?.command_for_link(link).cloned()
        }))
    }

    fn copy_host_group_results(&mut self, id: TabId, submission_id: u64) {
        let Some(report) = self.capture_host_group_report(id, submission_id) else {
            return;
        };
        let copied = report
            .clipboard(w11term::clipboard::MAX_TEXT)
            .and_then(|text| w11term::clipboard::write(&text));
        if let Some(page) = self.host_group_mut(id) {
            page.copy_feedback = match copied {
                Ok(()) => "Copied results".into(),
                Err(error) => format!("Could not copy results · {error}"),
            };
        }
    }

    pub(super) fn host_group_view(
        &self,
        id: TabId,
        page: &HostGroup,
        ctx: &mut ViewContext<Self>,
    ) -> View {
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
            return self.host_group_live_output(id, output, submission, ctx);
        }
        if page.focus_loaded_command {
            let sender = ctx.sender();
            ctx.use_effect("focus host group draft", id, move || {
                sender.send(Message::HostGroup(Action::FocusLoadedCommand(id)));
                None
            });
        }
        let targets = self.host_group_targets();
        let shown: Vec<_> = page.shown(&targets).collect();
        let selected: Vec<_> = shown
            .iter()
            .copied()
            .filter(|target| page.selected.contains(&target.pane))
            .cloned()
            .collect::<Vec<_>>();
        let text: Arc<str> = page.draft.as_str().into();
        let valid_text = w11term::saved_commands::valid_command_text(&text);
        let enabled = valid_text
            && !page.sending()
            && !page.recorded_send.pending()
            && selected.iter().any(|target| target.state.is_ready());
        let send_label = format!(
            "Send to {} {}",
            selected.len(),
            if selected.len() == 1 { "host" } else { "hosts" }
        );
        let mut details: Vec<KeyedView> = shown
            .iter()
            .map(|target| {
                let pane = target.pane;
                let state = if target.connected {
                    format!("Connected · {}", target.state.label())
                } else {
                    target.state.label().to_owned()
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
                                    .text(state),
                                Button::new()
                                    .automation_name(format!("Focus pane {pane}"))
                                    .on_click(ctx.callback(move |_| {
                                        Message::HostGroup(Action::Visit(id, pane))
                                    }))
                                    .content("Focus pane"),
                                reconnect::host_button(id, target.state, ctx),
                                prompt_setup::button(id, target, ctx),
                            )),
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
            .columns([GridLength::STAR, GridLength::Auto])
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
            ));
        let filter = StackPanel::new().grid_row(2).spacing(6.0).children((
            filter_input,
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
            reconnect::selection_controls(id, page, &shown, ctx),
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
            environment::button(id, page, &selected, ctx),
        ));
        let instructions = if !page.draft.is_empty() && !valid_text {
            "Enter one command line of up to 4096 bytes.".into()
        } else if selected.is_empty() {
            "Select panes with a fresh shell marker or a configured prompt match.".into()
        } else {
            let ready = selected
                .iter()
                .filter(|target| target.state.is_ready())
                .count();
            let count = selected.len();
            let hosts = if count == 1 { "host" } else { "hosts" };
            format!("{ready} of {count} selected {hosts} ready")
        };
        let entry = TextBox::new()
            .element_ref(&page.command_editor)
            .automation_name("Group command")
            .placeholder_text("Enter one shell command")
            .text(&page.draft)
            .on_text_changed(ctx.callback(move |text| Message::HostGroup(Action::Edit(id, text))));
        let send_recorded = recorded_send::button(id, text.clone(), selected.clone(), enabled, ctx);
        let send = Button::new()
            .style(ButtonStyle::Accent)
            .is_enabled(enabled)
            .on_click(ctx.callback(move |_| {
                Message::HostGroup(Action::Send(id, text.clone(), selected.clone()))
            }))
            .content(send_label);
        let saved_commands = Button::new()
            .on_click(ctx.callback(move |_| Message::HostGroup(Action::OpenSavedCommands(id))))
            .content("Saved commands…");
        let recent_commands = Button::new()
            .on_click(ctx.callback(move |_| Message::HostGroup(Action::OpenRecentCommands(id))))
            .content("Recent commands…");
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
                        .spacing(8.0)
                        .children((
                            send,
                            send_recorded,
                            recent_commands,
                            saved_commands,
                            save_command,
                            restore,
                        )),
                    page.command_save.view(id, ctx),
                    page.recorded_send.feedback(),
                    retry::feedback(page),
                    hidden_selection,
                    TextBlock::new()
                        .text(instructions)
                        .text_wrapping(TextWrapping::Wrap),
                )),
                ScrollViewer::new()
                    .grid_row(4)
                    .content(StackPanel::new().spacing(10.0).keyed_children(details)),
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
        let mut summary = progress::Summary::default();
        let mut shown: Vec<_> = submission
            .results
            .iter()
            .filter_map(|delivery| {
                let execution = delivery.execution();
                let outcome =
                    progress::Outcome::with_execution(&delivery.status, execution.as_ref());
                summary.add(outcome);
                page.result_filter
                    .includes(outcome)
                    .then_some((delivery, execution))
            })
            .collect();
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
        details.push(KeyedView::new(
            "results",
            StackPanel::new().spacing(4.0).children((
                TextBlock::new()
                    .font_size(18.0)
                    .text(format!("Submitted · {}", submission.content.label()))
                    .text_trimming(TextTrimming::CharacterEllipsis),
                TextBlock::new()
                    .text(summary.caption())
                    .text_wrapping(TextWrapping::Wrap),
                output_recording::status_view(id, submission, ctx),
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
                        Button::new()
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(Action::CopyResults(id, submission_id))
                            }))
                            .content("Copy results"),
                        Button::new()
                            .is_enabled(self.export.pending.is_none())
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(Action::SaveResults(
                                    id,
                                    submission_id,
                                    text_export::GroupReportFormat::Text,
                                ))
                            }))
                            .content(text_export::GROUP_REPORT_LABEL),
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
                            .is_enabled(self.export.pending.is_none())
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(Action::SaveResults(
                                    id,
                                    submission_id,
                                    text_export::GroupReportFormat::Json,
                                ))
                            }))
                            .content(text_export::GROUP_JSON_LABEL),
                        completion::button(id, submission, ctx),
                    )),
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
        if shown.is_empty() {
            details.push(KeyedView::new(
                "results-empty",
                TextBlock::new().text("No recipients match this result filter."),
            ));
        }
        let now = Instant::now();
        page.result_order.apply(&mut shown, now);
        details.extend(shown.into_iter().map(|(delivery, execution)| {
            KeyedView::new(
                format!("delivery-{}-{}", submission.id, delivery.target.pane),
                self.host_group_result_row(
                    id,
                    delivery,
                    &submission.content,
                    execution.as_ref(),
                    now,
                    ctx,
                ),
            )
        }));
    }

    fn host_group_result_row(
        &self,
        id: TabId,
        delivery: &Delivery,
        content: &SubmissionContent,
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
        if let Some(command) = content.host_command(pane) {
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
