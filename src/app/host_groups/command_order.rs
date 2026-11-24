use super::*;

pub enum Edit {
    Swap(u64, u64),
    Reset,
}

fn sort_targets(page: &HostGroup, targets: &mut [&Target]) {
    page.sort_shown(targets);
    if page.manual_command_order.is_empty() || !page.orders_commands() {
        return;
    }
    let ranks: BTreeMap<_, _> = page
        .manual_command_order
        .iter()
        .copied()
        .enumerate()
        .map(|(rank, pane)| (pane, rank))
        .collect();
    targets.sort_by_key(|target| ranks.get(&target.pane).copied().unwrap_or(usize::MAX));
}

pub(super) fn recipients<'a>(
    page: &HostGroup,
    shown: impl Iterator<Item = &'a Target>,
) -> Vec<&'a Target> {
    let mut targets: Vec<_> = shown
        .filter(|target| page.selected.contains(&target.pane))
        .collect();
    sort_targets(page, &mut targets);
    if let Some(index) = targets
        .iter()
        .position(|target| Some(target.pane) == page.canary)
    {
        targets[..=index].rotate_right(1);
    }
    targets
}

fn swap_recipients(page: &mut HostGroup, roster: &[Target], earlier: u64, later: u64) {
    if !page.orders_commands() {
        return;
    }
    let visible: Vec<_> = recipients(page, page.shown(roster))
        .into_iter()
        .filter(|target| movable(target, page.canary))
        .map(|target| target.pane)
        .collect();
    if !visible.windows(2).any(|pair| pair == [earlier, later]) {
        return;
    }

    // Swap positions across the whole roster so filters and temporary prompt
    // changes cannot erase the ordering of other panes.
    let mut scoped: Vec<_> = page.scoped_targets(roster).collect();
    sort_targets(page, &mut scoped);
    let mut order: Vec<_> = scoped.iter().map(|target| target.pane).collect();
    let first = order.iter().position(|&pane| pane == earlier);
    let second = order.iter().position(|&pane| pane == later);
    if let (Some(first), Some(second)) = (first, second) {
        order.swap(first, second);
        page.manual_command_order = order;
    }
}

fn movable(target: &Target, canary: Option<u64>) -> bool {
    target.state.is_ready() && Some(target.pane) != canary
}

impl TerminalApp {
    pub(super) fn edit_command_order(&mut self, id: TabId, edit: Edit) {
        if self.host_group_command_target() != Some(id) {
            return;
        }
        let roster = self.host_group_targets();
        let Some(page) = self
            .host_group_mut(id)
            .filter(|page| page.preview_command_order && !page.recorded_send.pending())
        else {
            return;
        };
        match edit {
            Edit::Swap(earlier, later) => swap_recipients(page, &roster, earlier, later),
            Edit::Reset => page.manual_command_order.clear(),
        }
    }
}

pub(super) fn button(
    id: TabId,
    open: bool,
    has_recipients: bool,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    Button::new()
        .is_enabled(open || has_recipients)
        .on_click(ctx.callback(move |_| Message::HostGroup(Action::PreviewCommandOrder(id, !open))))
        .content(if open {
            "Back to roster and results"
        } else {
            "Preview send order"
        })
}

pub(super) fn preview(
    id: TabId,
    page: &HostGroup,
    offer: &CommandOffer,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let limit = offer.limit.concurrency();
    let canary = offer.canary.is_some();
    let ordered = page.orders_commands();
    let summary = match (canary, limit) {
        (true, Some(1)) => "After approval, send to one host at a time.".into(),
        (true, Some(limit)) => format!("After approval, send to up to {limit} hosts at a time."),
        (true, None) => "After approval, send to all remaining ready hosts.".into(),
        (false, Some(1)) => "Send to one host at a time.".into(),
        (false, Some(limit)) => format!("Send to up to {limit} hosts at a time."),
        (false, None) => "Send to all ready hosts without a parallel limit.".into(),
    };
    let unavailable = offer.targets.iter().any(|target| !target.state.is_ready());
    let hold = if offer.targets.is_empty() {
        Some("Check the hosts to include in the send order.")
    } else if unavailable && page.roster_preferences.require_all_ready {
        Some("Waiting for all checked hosts to be ready.")
    } else if unavailable && ordered && offer.stop_queue_on_failure {
        Some("An unavailable host would stop this queue before it starts.")
    } else {
        None
    };
    let movable_panes: Vec<_> = offer
        .targets
        .iter()
        .filter(|target| movable(target, offer.canary))
        .map(|target| target.pane)
        .collect();
    let can_edit = !page.recorded_send.pending();
    let mut position: usize = 0;
    let rows = offer.targets.iter().map(|target| {
        let name = &target.name;
        let text = if !target.state.is_ready() {
            format!("Unavailable · {name} · {}", target.state.label())
        } else if Some(target.pane) == offer.canary {
            format!("Canary first · {name}")
        } else if ordered {
            position += 1;
            format!("{position}. {name}")
        } else {
            format!("Ready · {name}")
        };
        let title = TextBlock::new()
            .vertical_alignment(VerticalAlignment::Center)
            .text(text)
            .text_wrapping(TextWrapping::Wrap);
        let row = if ordered && movable_panes.len() > 1 && movable(target, offer.canary) {
            let pane = target.pane;
            let previous = position
                .checked_sub(2)
                .and_then(|index| movable_panes.get(index))
                .copied();
            let following = movable_panes.get(position).copied();
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto, GridLength::Auto])
                .column_spacing(6.0)
                .children((
                    title,
                    Button::new()
                        .grid_column(1)
                        .automation_name(format!("Move {name} earlier"))
                        .is_enabled(can_edit && previous.is_some())
                        .on_click(ctx.callback(move |_| match previous {
                            Some(previous) => Message::HostGroup(super::Action::EditCommandOrder(
                                id,
                                Edit::Swap(previous, pane),
                            )),
                            None => Message::ControlIdle,
                        }))
                        .content("Earlier"),
                    Button::new()
                        .grid_column(2)
                        .automation_name(format!("Move {name} later"))
                        .is_enabled(can_edit && following.is_some())
                        .on_click(ctx.callback(move |_| match following {
                            Some(following) => Message::HostGroup(super::Action::EditCommandOrder(
                                id,
                                Edit::Swap(pane, following),
                            )),
                            None => Message::ControlIdle,
                        }))
                        .content("Later"),
                ))
        } else {
            title.into()
        };
        KeyedView::new(target.pane.to_string(), row)
    });
    let reset = if page.manual_command_order.is_empty() {
        View::empty()
    } else {
        Button::new()
            .is_enabled(can_edit)
            .on_click(ctx.callback(move |_| {
                Message::HostGroup(super::Action::EditCommandOrder(id, Edit::Reset))
            }))
            .content("Use roster order")
    };
    StackPanel::new().spacing(6.0).children((
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                TextBlock::new()
                    .vertical_alignment(VerticalAlignment::Center)
                    .font_size(18.0)
                    .text("Send order"),
                reset,
            )),
        TextBlock::new()
            .text(summary)
            .text_wrapping(TextWrapping::Wrap),
        if offer.interval.is_zero() {
            View::empty()
        } else {
            TextBlock::new()
                .text(format!("Dispatch interval · {:?}", offer.interval))
                .into()
        },
        hold.map_or_else(View::empty, |text| {
            TextBlock::new()
                .text(text)
                .text_wrapping(TextWrapping::Wrap)
                .into()
        }),
        StackPanel::new().spacing(4.0).keyed_children(rows),
    ))
}
