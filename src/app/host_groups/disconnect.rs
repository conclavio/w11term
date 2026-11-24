use super::*;

const LABEL: &str = "Disconnect selected…";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipient {
    pane: u64,
    connection: u64,
    name: String,
}

pub enum Action {
    Review(Vec<Recipient>),
    Confirm(Arc<[Recipient]>),
    Cancel(Arc<[Recipient]>),
}

#[derive(Default)]
pub(super) struct State {
    offered: Option<Arc<[Recipient]>>,
    changed: bool,
    results: Vec<(Recipient, DeliveryStatus)>,
}

impl State {
    pub(super) fn is_reviewing(&self) -> bool {
        self.offered.is_some()
    }

    fn pending(&self) -> bool {
        self.results
            .iter()
            .any(|(_, status)| matches!(status, DeliveryStatus::Pending(..)))
    }

    pub(super) fn can_review(&self) -> bool {
        !self.is_reviewing() && !self.pending()
    }

    pub(super) fn poll(&mut self) {
        for (_, status) in &mut self.results {
            status.poll();
        }
    }

    pub(super) fn controls(
        &self,
        id: TabId,
        recipients: Vec<Recipient>,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        if recipients.is_empty() && self.results.is_empty() && !self.changed {
            return View::empty();
        }
        let offered = recipients.len();
        let button = Button::new()
            .automation_name(LABEL)
            .is_enabled(offered > 0 && self.can_review())
            .on_click(ctx.callback(move |_| {
                Message::HostGroup(super::Action::Disconnect(
                    id,
                    Action::Review(recipients.clone()),
                ))
            }))
            .content(if offered == 0 {
                LABEL.into()
            } else {
                format!("Disconnect selected ({offered})…")
            });
        let mut feedback = Vec::new();
        if self.changed {
            feedback.push("The selected SSH shells changed. Review the current selection.".into());
        }
        let accepted = self
            .results
            .iter()
            .filter(|(_, state)| matches!(state, DeliveryStatus::Sent))
            .count();
        if accepted > 0 {
            let hosts = if accepted == 1 { "host" } else { "hosts" };
            feedback.push(format!("Disconnect requested for {accepted} {hosts}."));
        }
        if self.pending() {
            feedback.push("Waiting for SSH shells to accept disconnect.".into());
        }
        for (target, status) in &self.results {
            match status {
                DeliveryStatus::Rejected(reason) => {
                    feedback.push(format!("{} · {reason}", target.name));
                }
                DeliveryStatus::Unconfirmed => {
                    feedback.push(format!("{} · Disconnect unconfirmed.", target.name));
                }
                _ => {}
            }
        }
        StackPanel::new().spacing(4.0).children((
            button,
            if feedback.is_empty() {
                View::empty()
            } else {
                TextBlock::new()
                    .text(feedback.join("\n"))
                    .text_wrapping(TextWrapping::Wrap)
                    .into()
            },
        ))
    }

    pub(super) fn review(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> Option<View> {
        let offered = self.offered.as_ref()?;
        let confirm = offered.clone();
        let cancel = offered.clone();
        let count = offered.len();
        let shells = if count == 1 { "shell" } else { "shells" };
        Some(
            Grid::new()
                .margin(16.0)
                .row_spacing(8.0)
                .rows([GridLength::Auto, GridLength::STAR])
                .children((
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new().font_size(24.0).text("Disconnect SSH shells"),
                        TextBlock::new()
                            .text("Ending these shells may stop remote programs and file transfers. The panes and scrollback stay open.")
                            .text_wrapping(TextWrapping::Wrap),
                        StackPanel::new()
                            .spacing(8.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                Button::new()
                                    .on_click(ctx.callback(move |_| {
                                        Message::HostGroup(super::Action::Disconnect(
                                            id,
                                            Action::Confirm(confirm.clone()),
                                        ))
                                    }))
                                    .content(format!("Disconnect {count} SSH {shells}")),
                                Button::new()
                                    .on_click(ctx.callback(move |_| {
                                        Message::HostGroup(super::Action::Disconnect(
                                            id,
                                            Action::Cancel(cancel.clone()),
                                        ))
                                    }))
                                    .content("Cancel"),
                            )),
                    )),
                    ScrollViewer::new().grid_row(1).content(
                        StackPanel::new().keyed_children(offered.iter().map(|target| {
                            KeyedView::new(
                                target.pane.to_string(),
                                TextBlock::new().text(&target.name)
                                    .text_wrapping(TextWrapping::Wrap),
                            )
                        })),
                    ),
                )),
        )
    }
}

impl TerminalApp {
    pub(super) fn direct_ssh_session(&self, pane: u64) -> Option<&Session> {
        match self.tab_for_pane(pane).and_then(|id| self.tabs.get(id)) {
            Some(AppTab {
                page: Page::Terminal(live),
                ..
            }) if live.session.is_direct_ssh_shell() => Some(&live.session),
            _ => None,
        }
    }

    pub(super) fn disconnect_recipients<'a>(
        &self,
        selected: impl Iterator<Item = &'a Target>,
    ) -> Vec<Recipient> {
        selected
            .filter(|target| target.connected && !target.state.is_authenticating())
            .filter_map(|target| {
                Some(Recipient {
                    pane: target.pane,
                    connection: self
                        .direct_ssh_session(target.pane)?
                        .ssh_connection_generation()?,
                    name: target.name.clone(),
                })
            })
            .collect()
    }

    pub(super) fn handle_group_disconnect(&mut self, id: TabId, action: Action) {
        if self.tabs.active_id() != Some(id)
            || (matches!(&action, Action::Review(_))
                && self.host_group_command_target() != Some(id))
        {
            return;
        }
        let current = || {
            let Some(AppTab {
                page: Page::HostGroup(page),
                ..
            }) = self.tabs.get(id)
            else {
                return Vec::new();
            };
            self.disconnect_recipients(
                page.shown(&self.host_group_targets())
                    .filter(|target| page.selected.contains(&target.pane)),
            )
        };
        match action {
            Action::Review(recipients) => {
                let unchanged = !recipients.is_empty() && current() == recipients;
                let Some(page) = self.host_group_mut(id) else {
                    return;
                };
                let state = &mut page.disconnect;
                if state.can_review() {
                    state.changed = !unchanged;
                    if unchanged {
                        state.offered = Some(recipients.into());
                    }
                }
            }
            Action::Cancel(offer) => {
                let Some(page) = self.host_group_mut(id) else {
                    return;
                };
                let state = &mut page.disconnect;
                if state
                    .offered
                    .as_ref()
                    .is_some_and(|shown| Arc::ptr_eq(shown, &offer))
                {
                    state.offered = None;
                }
            }
            Action::Confirm(offer) => {
                let unchanged = current() == offer.as_ref();
                let Some(page) = self.host_group_mut(id) else {
                    return;
                };
                let state = &mut page.disconnect;
                if !state
                    .offered
                    .as_ref()
                    .is_some_and(|shown| Arc::ptr_eq(shown, &offer))
                {
                    return;
                }
                state.offered = None;
                state.changed = !unchanged;
                if !unchanged {
                    return;
                }
                let now = Instant::now();
                let results = offer
                    .iter()
                    .map(|target| {
                        let (reply, received) = oneshot::channel();
                        let status =
                            if self.direct_ssh_session(target.pane).is_some_and(|session| {
                                session
                                    .input
                                    .send(Command::DisconnectSshShell {
                                        connection: target.connection,
                                        reply,
                                    })
                                    .is_ok()
                            }) {
                                DeliveryStatus::Pending(now, received)
                            } else {
                                DeliveryStatus::Rejected("The SSH pane already closed.".into())
                            };
                        (target.clone(), status)
                    })
                    .collect();
                self.host_group_mut(id).unwrap().disconnect.results = results;
            }
        }
    }
}
