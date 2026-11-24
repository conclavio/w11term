use super::*;

const LABEL: &str = "Close ended SSH panes…";
const CHANGED: &str = "The ended SSH panes changed. Review the current selection.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipient {
    exit: ExitedPane,
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
    feedback: String,
}

impl State {
    pub(super) fn is_reviewing(&self) -> bool {
        self.offered.is_some()
    }

    fn take_offer(&mut self, offer: &Arc<[Recipient]>) -> bool {
        if self
            .offered
            .as_ref()
            .is_some_and(|shown| Arc::ptr_eq(shown, offer))
        {
            self.offered = None;
            true
        } else {
            false
        }
    }

    pub(super) fn controls(
        &self,
        id: TabId,
        recipients: Vec<Recipient>,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let button = if recipients.is_empty() {
            View::empty()
        } else {
            let count = recipients.len();
            Button::new()
                .automation_name(LABEL)
                .on_click(ctx.callback(move |_| {
                    Message::HostGroup(super::Action::CloseEnded(
                        id,
                        Action::Review(recipients.clone()),
                    ))
                }))
                .content(format!("Close ended SSH panes ({count})…"))
        };
        if self.feedback.is_empty() {
            button
        } else {
            StackPanel::new().spacing(4.0).children((
                button,
                TextBlock::new()
                    .text(&self.feedback)
                    .text_wrapping(TextWrapping::Wrap),
            ))
        }
    }

    pub(super) fn review(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> Option<View> {
        let offered = self.offered.as_ref()?;
        let confirm = offered.clone();
        let cancel = offered.clone();
        let count = offered.len();
        let panes = if count == 1 { "pane" } else { "panes" };
        Some(
            Grid::new()
                .margin(16.0)
                .row_spacing(8.0)
                .rows([GridLength::Auto, GridLength::STAR])
                .children((
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .font_size(24.0)
                            .text("Review ended SSH panes"),
                        TextBlock::new()
                            .text("Closing these panes discards their scrollback.")
                            .text_wrapping(TextWrapping::Wrap),
                        StackPanel::new()
                            .spacing(8.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                Button::new()
                                    .on_click(ctx.callback(move |_| {
                                        Message::HostGroup(super::Action::CloseEnded(
                                            id,
                                            Action::Confirm(confirm.clone()),
                                        ))
                                    }))
                                    .content(format!("Close {count} ended SSH {panes}")),
                                Button::new()
                                    .on_click(ctx.callback(move |_| {
                                        Message::HostGroup(super::Action::CloseEnded(
                                            id,
                                            Action::Cancel(cancel.clone()),
                                        ))
                                    }))
                                    .content("Cancel"),
                            )),
                    )),
                    ScrollViewer::new()
                        .grid_row(1)
                        .content(
                            StackPanel::new().keyed_children(offered.iter().map(|target| {
                                KeyedView::new(
                                    target.exit.pane.to_string(),
                                    TextBlock::new()
                                        .text(&target.name)
                                        .text_wrapping(TextWrapping::Wrap),
                                )
                            })),
                        ),
                )),
        )
    }
}

impl TerminalApp {
    fn close_ended_location(&self, pane: u64) -> Option<(TabId, TabId)> {
        let entry = self.tab_for_pane(pane)?;
        let group = self.tabs.group_id(entry)?;
        (!self.tabs.group_is_pinned(group)).then_some((entry, group))
    }

    pub(super) fn close_ended_recipients<'a>(
        &self,
        selected: impl Iterator<Item = &'a Target>,
    ) -> Vec<Recipient> {
        selected
            .filter_map(|target| {
                let exit = target.state.exit()?;
                self.close_ended_location(exit.pane)?;
                Some(Recipient {
                    exit,
                    name: target.name.clone(),
                })
            })
            .collect()
    }

    fn current_close_ended_recipients(&self, id: TabId) -> Vec<Recipient> {
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page) else {
            return Vec::new();
        };
        self.close_ended_recipients(
            page.shown(&self.host_group_targets())
                .filter(|target| page.selected.contains(&target.pane)),
        )
    }

    pub(super) fn handle_close_ended(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        match action {
            Action::Review(recipients) => {
                if recipients.is_empty() || self.host_group_command_target() != Some(id) {
                    return;
                }
                let unchanged = self.current_close_ended_recipients(id) == recipients;
                if let Some(page) = self.host_group_mut(id) {
                    let state = &mut page.close_ended;
                    if unchanged {
                        state.offered = Some(recipients.into());
                        state.feedback.clear();
                    } else {
                        state.feedback = CHANGED.into();
                    }
                }
            }
            Action::Cancel(offer) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.close_ended.take_offer(&offer);
                }
            }
            Action::Confirm(offer) => {
                let unchanged = self.current_close_ended_recipients(id) == offer.as_ref();
                let Some(page) = self.host_group_mut(id) else {
                    return;
                };
                if !page.close_ended.take_offer(&offer) {
                    return;
                }
                if !unchanged {
                    page.close_ended.feedback = CHANGED.into();
                    return;
                }
                let closed = offer
                    .iter()
                    .filter(|target| self.close_ended_pane(target.exit))
                    .count();
                if let Some(page) = self.host_group_mut(id) {
                    let panes = if closed == 1 { "pane" } else { "panes" };
                    page.close_ended.feedback = if closed == offer.len() {
                        format!("Closed {closed} ended SSH {panes}.")
                    } else if closed == 0 {
                        CHANGED.into()
                    } else {
                        format!("Closed {closed} ended SSH {panes}. {CHANGED}")
                    };
                }
                if closed > 0 {
                    self.sync_selection(ctx);
                    self.activate(ctx);
                }
            }
        }
    }

    fn close_ended_pane(&mut self, exit: ExitedPane) -> bool {
        let Some((entry, group)) = self.close_ended_location(exit.pane) else {
            return false;
        };
        if !self
            .direct_ssh_session(exit.pane)
            .is_some_and(|session| session.claim_exited_ssh_for_close(exit.token))
        {
            return false;
        }
        if matches!(
            self.tabs.layout_for(entry),
            Some(w11term::pane_layout::Layout::Split { .. })
        ) {
            self.remember_closed_pane(entry);
        } else {
            self.remember_closed_tab(group);
            self.mru_groups.retain(|recent| *recent != group);
        }
        self.tabs.remove(entry);
        true
    }
}
