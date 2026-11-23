use super::*;
use std::{borrow::Cow, collections::BTreeSet};
use tokio::sync::{oneshot, oneshot::error::TryRecvError, watch};
use w11term::events::{CommandLink, GroupExecution, GroupOutput, PromptToken, ShellPrompt};
mod saved;

pub(super) const LABEL: &str = "SSH host group";
const ACK_TIMEOUT: Duration = Duration::from_secs(10);
const SUBMISSION_LIMIT: usize = 8;

#[derive(Default)]
pub(super) struct HostGroup {
    saved: saved::SavedUi,
    selected: BTreeSet<u64>,
    draft: String,
    submissions: VecDeque<Submission>,
    viewing: u64,
    next_submission: u64,
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
}

impl TargetState {
    fn label(self) -> &'static str {
        match self {
            Self::Offline => "Disconnected",
            Self::Authentication => "Authentication required",
            Self::ReadOnly => "Read-only",
            Self::Alternate => "Alternate screen",
            Self::Unverified => "Prompt unverified",
            Self::InUse => "Waiting for a fresh prompt",
            Self::Ready(_) => "Ready",
        }
    }
}

struct Delivery {
    target: Target,
    status: DeliveryStatus,
    execution: Option<watch::Receiver<GroupExecution>>,
}

enum DeliveryStatus {
    Pending(Instant, oneshot::Receiver<Result<(), String>>),
    Sent,
    Rejected(String),
    Unconfirmed,
}

impl Delivery {
    fn poll(&mut self) {
        let DeliveryStatus::Pending(started, reply) = &mut self.status else {
            return;
        };
        self.status = match reply.try_recv() {
            Ok(Ok(())) => DeliveryStatus::Sent,
            Ok(Err(error)) => DeliveryStatus::Rejected(error),
            Err(TryRecvError::Empty) if started.elapsed() < ACK_TIMEOUT => return,
            Err(_) => DeliveryStatus::Unconfirmed,
        };
        if !matches!(self.status, DeliveryStatus::Sent) {
            self.execution = None;
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
}

impl HostGroup {
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
        self.submissions.push_back(Submission {
            id: self.viewing,
            text,
            results,
        });
        if self.submissions.len() > SUBMISSION_LIMIT {
            self.submissions.pop_front();
        }
    }
}

pub enum Action {
    Open,
    Saved(TabId, saved::Action),
    Tick,
    Edit(TabId, String),
    Select(TabId, u64, bool),
    Visit(TabId, u64),
    ViewSubmission(TabId, u64),
    ViewOutput(TabId, u64, CommandLink),
    Send(TabId, Arc<str>, Vec<Target>),
}

impl TerminalApp {
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
                    connected,
                    state,
                })
            })
            .collect()
    }

    fn dispatch_to_host(&self, target: Target, text: Arc<str>) -> Delivery {
        let mut execution = None;
        let status = match target.state {
            TargetState::Ready(prompt) => {
                match self
                    .tab_for_pane(target.pane)
                    .and_then(|id| self.tabs.get(id))
                {
                    Some(AppTab {
                        page: Page::Terminal(live),
                        ..
                    }) if live.session.is_direct_ssh_shell() => {
                        let (reply, received) = oneshot::channel();
                        let (observe, observed) = watch::channel(GroupExecution::AwaitingMarkers);
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
        }
    }

    pub(super) fn handle_host_group(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Open => {
                self.refresh_profiles();
                if let Some(id) = self
                    .tabs
                    .entries()
                    .iter()
                    .find(|entry| matches!(entry.value.page, Page::HostGroup(_)))
                    .map(|entry| entry.id)
                {
                    self.tabs.select(id);
                    self.sync_selection(ctx);
                    self.activate(ctx);
                } else {
                    self.insert_tab(
                        AppTab {
                            label: LABEL.into(),
                            page: Page::HostGroup(HostGroup::default()),
                        },
                        ctx,
                    );
                }
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
            Action::Select(id, pane, selected) => {
                if let Some(page) = self.host_group_mut(id) {
                    if selected {
                        page.selected.insert(pane);
                    } else {
                        page.selected.remove(&pane);
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
                }
            }
            Action::ViewOutput(id, pane, link) => {
                if self.host_group_mut(id).is_some() {
                    self.open_command_history(pane, Some(link), ctx);
                }
            }
            Action::Send(id, text, targets) => {
                let Some(page) = self.host_group_mut(id) else {
                    return;
                };
                let selection: BTreeSet<_> = targets.iter().map(|target| target.pane).collect();
                if page.sending()
                    || page.selected != selection
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
        let selected = targets
            .iter()
            .filter(|target| page.selected.contains(&target.pane))
            .cloned()
            .collect::<Vec<_>>();
        let text: Arc<str> = page.draft.as_str().into();
        let valid_text = w11term::saved_commands::valid_command_text(&text);
        let enabled = valid_text
            && !page.sending()
            && selected
                .iter()
                .any(|target| matches!(target.state, TargetState::Ready(_)));
        let send_label = format!(
            "Send to {} {}",
            selected.len(),
            if selected.len() == 1 { "host" } else { "hosts" }
        );
        let mut details: Vec<KeyedView> = targets
            .into_iter()
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
        if details.is_empty() {
            details.push(KeyedView::new(
                "empty",
                TextBlock::new().text("Open a direct SSH shell to add it to this roster."),
            ));
        }
        self.host_group_results(id, page, ctx, &mut details);
        let instructions = if !page.draft.is_empty() && !valid_text {
            "Enter one command line of up to 4096 bytes."
        } else {
            "Select the panes to receive the command. A fresh shell integration prompt is required."
        };
        let entry = TextBox::new()
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
        Grid::new()
            .margin(Thickness::uniform(16.0))
            .rows([
                GridLength::Auto,
                GridLength::Auto,
                GridLength::Auto,
                GridLength::STAR,
            ])
            .children((
                TextBlock::new().font_size(24.0).text(LABEL),
                self.saved_host_group_toolbar(id, &page.saved, ctx),
                StackPanel::new().grid_row(2).spacing(7.0).children((
                    entry,
                    send,
                    TextBlock::new()
                        .text(instructions)
                        .text_wrapping(TextWrapping::Wrap),
                )),
                ScrollViewer::new()
                    .grid_row(3)
                    .content(StackPanel::new().spacing(10.0).keyed_children(details)),
            ))
    }

    fn host_group_results(
        &self,
        id: TabId,
        page: &HostGroup,
        ctx: &ViewContext<Self>,
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
            )),
        ));
        details.extend(submission.results.iter().map(|delivery| {
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
        if matches!(delivery.status, DeliveryStatus::Sent)
            && let Some(execution) = &delivery.execution
        {
            let result = execution.borrow().clone();
            let name = &delivery.target.name;
            let caption = match &result {
                GroupExecution::AwaitingMarkers => "Waiting for shell markers".into(),
                GroupExecution::Running => "Running".into(),
                GroupExecution::Unavailable(reason) => format!("Result unavailable · {reason}"),
                GroupExecution::Finished(result) => result.exit_code.map_or_else(
                    || "Completed · exit status unavailable".into(),
                    |code| format!("Exit {code}"),
                ),
            };
            rows.push(KeyedView::new(
                "execution",
                TextBlock::new()
                    .automation_name(format!("{name} · {caption}"))
                    .text(caption)
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
    fn submission_history_retains_the_latest_eight_and_selects_the_newest() {
        let mut group = HostGroup::default();
        for id in 1..=10 {
            group.submit(format!("command {id}").into(), Vec::new());
        }
        assert_eq!(group.submissions.len(), SUBMISSION_LIMIT);
        assert_eq!(group.submissions.front().unwrap().id, 3);
        assert_eq!(group.submissions.back().unwrap().id, group.viewing);
    }
}
