use super::*;

pub(super) fn recipients<'a>(
    page: &HostGroup,
    shown: impl Iterator<Item = &'a Target>,
) -> Vec<&'a Target> {
    let mut targets: Vec<_> = shown
        .filter(|target| page.selected.contains(&target.pane))
        .collect();
    page.sort_shown(&mut targets);
    if let Some(index) = targets
        .iter()
        .position(|target| Some(target.pane) == page.canary)
    {
        targets[..=index].rotate_right(1);
    }
    targets
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

pub(super) fn preview(offer: &CommandOffer, require_all_ready: bool) -> View {
    let limit = offer.limit.concurrency();
    let canary = offer.canary.is_some();
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
    } else if unavailable && require_all_ready {
        Some("Waiting for all checked hosts to be ready.")
    } else if unavailable && limit.is_some() && offer.stop_queue_on_failure {
        Some("An unavailable host would stop this queue before it starts.")
    } else {
        None
    };
    let mut position = 0;
    let rows = offer.targets.iter().map(|target| {
        let name = &target.name;
        let text = if !target.state.is_ready() {
            format!("Unavailable · {name} · {}", target.state.label())
        } else if Some(target.pane) == offer.canary {
            format!("Canary first · {name}")
        } else if limit.is_some() {
            position += 1;
            format!("{position}. {name}")
        } else {
            format!("Ready · {name}")
        };
        KeyedView::new(
            target.pane.to_string(),
            TextBlock::new()
                .text(text)
                .text_wrapping(TextWrapping::Wrap),
        )
    });
    StackPanel::new().spacing(6.0).children((
        TextBlock::new().font_size(18.0).text("Send order"),
        TextBlock::new()
            .text(summary)
            .text_wrapping(TextWrapping::Wrap),
        hold.map_or_else(View::empty, |text| {
            TextBlock::new()
                .text(text)
                .text_wrapping(TextWrapping::Wrap)
                .into()
        }),
        StackPanel::new().spacing(4.0).keyed_children(rows),
    ))
}
