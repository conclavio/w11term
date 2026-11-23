use super::*;
use std::{borrow::Cow, collections::BTreeSet};
use tokio::sync::{oneshot, oneshot::error::TryRecvError, watch};
use w11term::events::{
    CommandLink, GroupExecution, GroupInterruptTarget, GroupOutput, PromptToken, ShellPrompt,
};
mod progress;
mod report;
mod save_command;
mod saved;

pub(super) const LABEL: &str = "SSH host group";
const ACK_TIMEOUT: Duration = Duration::from_secs(10);
const SUBMISSION_LIMIT: usize = 8;

#[derive(Default)]
pub(super) struct HostGroup {
    saved: saved::SavedUi,
    selected: BTreeSet<u64>,
    filter: String,
    draft: String,
    previous_draft: Option<String>,
    command_editor: ElementRef<TextBox>,
    command_save: save_command::State,
    submissions: VecDeque<Submission>,
    viewing: u64,
    next_submission: u64,
    copy_feedback: String,
    result_filter: progress::Filter,
}

struct Submission {
    id: u64,
    text: Arc<str>,
    results: Vec<Delivery>,
}

#[derive(Clone)]
pub struct Target {
    pane: u64,
    name: String,
    profile: Option<String>,
    connected: bool,
    state: TargetState,
}

#[derive(Clone, Copy)]
enum TargetState {
    Offline,
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
        matches!(self, Self::Ready(_) | Self::Matched(_))
    }

    fn label(self) -> &'static str {
        match self {
            Self::Offline => "Disconnected",
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
            GroupExecution::Running(link) => Some(link),
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
        GroupExecution::Running(_) => "Running".into(),
        GroupExecution::PromptReturned => "Prompt returned · exit status unavailable".into(),
        GroupExecution::Unavailable(reason) => format!("Result unavailable · {reason}").into(),
        GroupExecution::Finished(result) => result.exit_code.map_or_else(
            || "Completed · exit status unavailable".into(),
            |code| format!("Exit {code}").into(),
        ),
    }
}

impl HostGroup {
    fn shown<'a>(&self, targets: &'a [Target]) -> impl Iterator<Item = &'a Target> {
        let query = self.filter.trim().to_lowercase();
        targets.iter().filter(move |target| {
            if query.is_empty() {
                return true;
            }
            let name = target.name.to_lowercase();
            let profile = target.profile.as_deref().unwrap_or_default().to_lowercase();
            query
                .split_whitespace()
                .all(|term| name.contains(term) || profile.contains(term))
        })
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

    fn submit(&mut self, text: Arc<str>, results: Vec<Delivery>) {
        self.next_submission += 1;
        self.viewing = self.next_submission;
        self.result_filter = progress::Filter::All;
        self.previous_draft = None;
        self.copy_feedback.clear();
        self.submissions.push_back(Submission {
            id: self.viewing,
            text,
            results,
        });
        if self.submissions.len() > SUBMISSION_LIMIT {
            self.submissions.pop_front();
        }
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
        let Some(command) = self.viewed_submission(id).map(|item| item.text.to_string()) else {
            return false;
        };
        self.load_draft(command);
        true
    }

    fn load_draft(&mut self, command: String) {
        if self.draft != command {
            let previous = std::mem::replace(&mut self.draft, command);
            self.previous_draft.get_or_insert(previous);
        }
    }

    fn restore_draft(&mut self) -> bool {
        let Some(previous) = self.previous_draft.take() else {
            return false;
        };
        self.draft = previous;
        true
    }
}

pub enum Action {
    Open,
    OpenNamedGroup { name: String, members: Vec<String> },
    Saved(TabId, saved::Action),
    Tick,
    Edit(TabId, String),
    Filter(TabId, String),
    Select(TabId, u64, bool),
    SelectShown(TabId, BTreeSet<u64>, ShownSelection),
    Visit(TabId, u64),
    ViewSubmission(TabId, u64),
    FilterResults(TabId, u64, progress::Filter),
    LoadCommand(TabId, u64),
    OpenSavedCommands(TabId),
    LoadSavedCommand(TabId, w11term::saved_commands::Entry),
    SaveCommand(TabId, save_command::Action),
    RestoreDraft(TabId),
    CopyResults(TabId, u64),
    ViewOutput(TabId, u64, CommandLink),
    Interrupt(TabId, u64, Vec<(u64, GroupInterruptTarget)>),
    Send(TabId, Arc<str>, Vec<Target>),
}

impl TerminalApp {
    fn host_group_tab_id(&self) -> Option<TabId> {
        self.tabs
            .entries()
            .iter()
            .find(|entry| matches!(entry.value.page, Page::HostGroup(_)))
            .map(|entry| entry.id)
    }

    fn ensure_host_group_tab(&mut self) -> TabId {
        self.host_group_tab_id().unwrap_or_else(|| {
            self.tabs.insert(AppTab {
                label: LABEL.into(),
                page: Page::HostGroup(HostGroup::default()),
            })
        })
    }

    pub(super) fn host_group_command_target(&self) -> Option<TabId> {
        let id = self.tabs.active_id()?;
        match &self.tabs.get(id)?.page {
            Page::HostGroup(page) if !page.saved.is_editing() => Some(id),
            _ => None,
        }
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
                    TargetState::Offline
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
                let host = if options.host.contains(':') && !options.host.starts_with('[') {
                    format!("[{}]", options.host)
                } else {
                    options.host.clone()
                };
                let label = entry
                    .value
                    .pane_name()
                    .map_or(entry.value.label.as_str(), |name| name.as_str());
                Some(Target {
                    pane,
                    name: format!(
                        "{label} · {}@{host}:{} · Pane {pane}",
                        options.user, options.port
                    ),
                    profile: live.profile_name.clone(),
                    connected,
                    state,
                })
            })
            .collect()
    }

    fn dispatch_to_host(&self, target: Target, text: Arc<str>) -> Delivery {
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
        Delivery {
            target,
            status,
            execution,
            interrupt: None,
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
            Action::Open => {
                self.refresh_profiles();
                let id = self.ensure_host_group_tab();
                self.tabs.select(id);
                self.sync_selection(ctx);
                self.activate(ctx);
            }
            Action::OpenNamedGroup { name, members } => {
                self.open_host_group_from_palette(&name, &members, ctx);
            }
            Action::Saved(id, action) => self.handle_saved_host_group(id, action, ctx),
            Action::Tick => {
                let Some(id) = self.tabs.active_id() else {
                    return;
                };
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
                if let Some(page) = self.host_group_mut(id) {
                    page.selected.retain(|pane| panes.contains(pane));
                    for submission in &mut page.submissions {
                        for delivery in &mut submission.results {
                            delivery.poll();
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
                    page.filter = query;
                }
            }
            Action::Select(id, pane, selected) => {
                if let Some(page) = self.host_group_mut(id) {
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
                if let Some(page) = self.host_group_mut(id)
                    && page.submissions.iter().any(|item| item.id == submission)
                {
                    page.viewing = submission;
                    page.result_filter = progress::Filter::All;
                    page.copy_feedback.clear();
                }
            }
            Action::FilterResults(id, submission, filter) => {
                if let Some(page) = self.host_group_mut(id)
                    && page.viewed_submission(submission).is_some()
                {
                    page.result_filter = filter;
                }
            }
            Action::LoadCommand(id, submission) => {
                if let Some(page) = self.host_group_mut(id)
                    && page.load_command(submission)
                {
                    let _ = page.command_editor.request_focus();
                }
            }
            Action::OpenSavedCommands(id) => {
                if self.host_group_command_target() == Some(id) {
                    self.open_group_command_library(id, ctx);
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
            Action::ViewOutput(id, pane, link) => {
                if self.host_group_mut(id).is_some() {
                    self.open_command_history(pane, Some(link), ctx);
                }
            }
            Action::Interrupt(id, submission, targets) => {
                self.interrupt_hosts(id, submission, targets)
            }
            Action::Send(id, text, targets) => {
                let roster = self.host_group_targets();
                let Some(page) = self.host_group_mut(id) else {
                    return;
                };
                let selection: BTreeSet<_> = targets.iter().map(|target| target.pane).collect();
                if page.sending()
                    || selection.is_empty()
                    || page.selected_shown(&roster) != selection
                    || page.draft != text.as_ref()
                    || !w11term::saved_commands::valid_command_text(&text)
                {
                    return;
                }
                let results = targets
                    .into_iter()
                    .map(|target| self.dispatch_to_host(target, text.clone()))
                    .collect();
                if let Some(page) = self.host_group_mut(id) {
                    page.submit(text, results);
                }
            }
        }
    }

    fn copy_host_group_results(&mut self, id: TabId, submission_id: u64) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page) else {
            return;
        };
        let Some(submission) = page.viewed_submission(submission_id) else {
            return;
        };
        let copied = report::build(submission, w11term::clipboard::MAX_TEXT, |pane, link| {
            self.history_frame(pane)?.command_for_link(link).cloned()
        })
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
        ctx: &ViewContext<Self>,
    ) -> View {
        if let Some(editor) = self.saved_host_group_editor(id, &page.saved, ctx) {
            return editor;
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
        let enabled =
            valid_text && !page.sending() && selected.iter().any(|target| target.state.is_ready());
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
                            )),
                    )),
                )
            })
            .collect();
        if shown.is_empty() {
            details.push(KeyedView::new(
                "empty",
                TextBlock::new().text(if targets.is_empty() {
                    "Open a direct SSH shell to add it to this roster."
                } else {
                    "No SSH panes match this filter."
                }),
            ));
        }
        let selected_shown: BTreeSet<_> = selected.iter().map(|target| target.pane).collect();
        self.host_group_results(id, page, ctx, &selected_shown, &mut details);
        let hidden_count = targets
            .iter()
            .filter(|target| {
                page.selected.contains(&target.pane) && !selected_shown.contains(&target.pane)
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
        let filter = StackPanel::new().grid_row(2).spacing(6.0).children((
            TextBox::new()
                .automation_name("Host filter")
                .placeholder_text("Filter by profile, pane, or SSH address")
                .text(&page.filter)
                .on_text_changed(
                    ctx.callback(move |query| Message::HostGroup(Action::Filter(id, query))),
                ),
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
                self.saved_host_group_toolbar(id, &page.saved, ctx),
                filter,
                StackPanel::new().grid_row(3).spacing(7.0).children((
                    entry,
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((send, saved_commands, save_command, restore)),
                    page.command_save.view(id, ctx),
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
        let shown: Vec<_> = submission
            .results
            .iter()
            .filter(|delivery| {
                let outcome = progress::Outcome::of(delivery);
                summary.add(outcome);
                page.result_filter.includes(outcome)
            })
            .collect();
        let interrupt_targets: Vec<_> = shown
            .iter()
            .filter(|delivery| selected_shown.contains(&delivery.target.pane))
            .filter_map(|delivery| {
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
        details.push(KeyedView::new(
            "results",
            StackPanel::new().spacing(4.0).children((
                TextBlock::new()
                    .font_size(18.0)
                    .text(format!("Submitted · {}", submission.text))
                    .text_trimming(TextTrimming::CharacterEllipsis),
                TextBlock::new()
                    .text(summary.caption())
                    .text_wrapping(TextWrapping::Wrap),
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
                        Button::new()
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(Action::LoadCommand(id, submission_id))
                            }))
                            .content("Load command"),
                        Button::new()
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(Action::CopyResults(id, submission_id))
                            }))
                            .content("Copy results"),
                        interrupt,
                    )),
                ComboBox::new()
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
                    })),
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
        details.extend(shown.into_iter().map(|delivery| {
            KeyedView::new(
                format!("delivery-{}-{}", submission.id, delivery.target.pane),
                self.host_group_result_row(id, delivery, ctx),
            )
        }));
    }

    fn host_group_result_row(
        &self,
        id: TabId,
        delivery: &Delivery,
        ctx: &ViewContext<Self>,
    ) -> View {
        let pane = delivery.target.pane;
        let mut rows = vec![KeyedView::new(
            "delivery",
            TextBlock::new()
                .text(delivery.description())
                .text_wrapping(TextWrapping::Wrap),
        )];
        if let Some(result) = delivery.execution() {
            let name = &delivery.target.name;
            let caption = execution_caption(&result);
            rows.push(KeyedView::new(
                "execution",
                TextBlock::new()
                    .automation_name(format!("{name} · {caption}"))
                    .text(caption.as_ref())
                    .text_wrapping(TextWrapping::Wrap),
            ));
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
                profile: Some("East Prod".into()),
                connected: false,
                state: TargetState::Offline,
            },
            Target {
                pane: 8,
                name: "Replica · ops@db.example:22 · Pane 8".into(),
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
            group.filter = query.into();
            assert_eq!(group.selected_shown(&roster), BTreeSet::from([7]));
        }
        group.filter = "missing".into();
        assert!(group.selected_shown(&roster).is_empty());
        group.filter.clear();
        assert_eq!(group.selected_shown(&roster), BTreeSet::from([7, 8]));
    }

    #[test]
    fn submission_history_retains_the_latest_eight_and_selects_the_newest() {
        let mut group = HostGroup::default();
        for id in 1..=10 {
            group.submit(format!("command {id}").into(), Vec::new());
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
            filter: "east".into(),
            ..Default::default()
        };
        group.submit("show  inventory".into(), Vec::new());
        group.submit("show revision".into(), Vec::new());

        assert!(!group.load_command(1));
        assert_eq!(group.draft, "in progress");
        group.viewing = 1;
        assert!(group.load_command(1));
        assert_eq!(group.draft, "show  inventory");
        group.viewing = 2;
        assert!(group.load_command(2));
        assert_eq!(group.draft, "show revision");
        assert_eq!(group.selected, BTreeSet::from([11, 12]));
        assert_eq!(group.filter, "east");
        assert_eq!(group.submissions.len(), 2);

        assert!(group.restore_draft());
        assert_eq!(group.draft, "in progress");
        assert!(!group.restore_draft());
        assert!(group.load_command(2));
        group.submit("show revision".into(), Vec::new());
        assert!(!group.restore_draft());
    }
}
