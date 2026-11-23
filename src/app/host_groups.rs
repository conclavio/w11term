use super::*;
use std::collections::BTreeSet;
use tokio::sync::oneshot::{self, error::TryRecvError};
use w11term::events::{PromptToken, ShellPrompt};

pub(super) const LABEL: &str = "SSH host group";
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct HostGroup {
    selected: BTreeSet<u64>,
    draft: String,
    submitted: Option<Arc<str>>,
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
        self.results
            .iter()
            .any(|item| matches!(item.status, DeliveryStatus::Pending(..)))
    }
}

pub enum Action {
    Open,
    Tick,
    Edit(TabId, String),
    Select(TabId, u64, bool),
    Visit(TabId, u64),
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
                        if live
                            .session
                            .input
                            .send(Command::SendGroupCommand {
                                prompt,
                                text,
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
                }
            }
            state => DeliveryStatus::Rejected(state.label().into()),
        };
        Delivery { target, status }
    }

    pub(super) fn handle_host_group(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Open => {
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
                    for delivery in &mut page.results {
                        delivery.poll();
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
                    page.submitted = Some(text);
                    page.results = results;
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
        if let Some(submitted) = &page.submitted {
            details.push(KeyedView::new(
                "results",
                TextBlock::new()
                    .font_size(18.0)
                    .text(format!("Submitted · {submitted}"))
                    .text_trimming(TextTrimming::CharacterEllipsis),
            ));
            details.extend(page.results.iter().map(|delivery| {
                KeyedView::new(
                    format!("delivery-{}", delivery.target.pane),
                    TextBlock::new()
                        .text(delivery.description())
                        .text_wrapping(TextWrapping::Wrap),
                )
            }));
        }
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
            .rows([GridLength::Auto, GridLength::Auto, GridLength::STAR])
            .children((
                TextBlock::new().font_size(24.0).text(LABEL),
                StackPanel::new().grid_row(1).spacing(7.0).children((
                    entry,
                    send,
                    TextBlock::new()
                        .text(instructions)
                        .text_wrapping(TextWrapping::Wrap),
                )),
                ScrollViewer::new()
                    .grid_row(2)
                    .content(StackPanel::new().spacing(10.0).keyed_children(details)),
            ))
    }
}
