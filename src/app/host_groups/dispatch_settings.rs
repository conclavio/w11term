use super::*;

impl TerminalApp {
    pub(super) fn dispatch_settings(
        &self,
        id: TabId,
        page: &HostGroup,
        offer: &CommandOffer,
        selected: &[Target],
        ctx: &ViewContext<Self>,
    ) -> View {
        let editable = !page.recorded_send.pending();
        let timing = StackPanel::new().spacing(4.0).children((
            parallel_limit::picker(
                "Parallel SSH commands",
                page.roster_preferences.command_limit,
                editable,
                move |limit| Message::HostGroup(Action::CommandLimit(id, limit)),
                ctx,
            ),
            command_queue::interval_picker(id, page, ctx),
            command_queue::expiry_picker(id, page, ctx),
            CheckBox::new()
                .automation_name("Stop queue on failure")
                .is_checked(page.roster_preferences.stop_command_queue_on_failure)
                .is_enabled(editable && page.orders_commands())
                .on_is_checked_changed(ctx.callback(move |stop| {
                    Message::HostGroup(Action::StopCommandQueueOnFailure(id, stop))
                }))
                .content("Stop queue on failure"),
        ));
        let recipients = StackPanel::new().spacing(4.0).children((
            CheckBox::new()
                .automation_name(REQUIRE_READY)
                .is_checked(page.roster_preferences.require_all_ready)
                .is_enabled(editable)
                .on_is_checked_changed(ctx.callback(move |require| {
                    Message::HostGroup(Action::RequireAllReady(id, require))
                }))
                .content(REQUIRE_READY),
            command_queue::canary_picker(id, page, selected, ctx),
        ));
        let settings = if self.width >= 760.0 {
            Grid::new()
                .columns([GridLength::STAR, GridLength::STAR])
                .column_spacing(12.0)
                .children((timing, Border::new().grid_column(1).content(recipients)))
        } else {
            StackPanel::new()
                .spacing(8.0)
                .children((timing, recipients))
        };
        let active = summary(page, offer);
        let header = StackPanel::new().spacing(2.0).children((
            TextBlock::new().text("Dispatch settings"),
            if active.is_empty() {
                View::empty()
            } else {
                TextBlock::new()
                    .text(active)
                    .text_wrapping(TextWrapping::Wrap)
                    .font_size(12.0)
                    .into()
            },
        ));
        let expander =
            Expander::new()
                .automation_name("Dispatch settings")
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .is_expanded(page.dispatch_settings_open)
                .on_is_expanded_changed(ctx.callback(move |open| {
                    Message::HostGroup(Action::ShowDispatchSettings(id, open))
                }))
                .slots([
                    SlotView::new(ExpanderSlot::Header, header),
                    SlotView::new(
                        ExpanderSlot::Content,
                        ScrollViewer::new()
                            .automation_name("Dispatch controls")
                            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                            .max_height((self.window_height * 0.32).clamp(96.0, 240.0))
                            .content(settings),
                    ),
                ]);
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(6.0)
            .children((
                expander,
                Border::new()
                    .grid_column(1)
                    .vertical_alignment(VerticalAlignment::Top)
                    .content(command_order::button(
                        id,
                        page.preview_command_order,
                        !offer.targets.is_empty(),
                        ctx,
                    )),
            ))
    }
}

fn summary(page: &HostGroup, offer: &CommandOffer) -> String {
    let mut details = Vec::new();
    if let Some(limit) = offer.parallel_limit {
        details.push(format!("{limit} at a time"));
    }
    if !offer.interval.is_zero() {
        details.push(format!("{:?} between sends", offer.interval));
    }
    if let Some(expiry) = offer.queue_expiry {
        details.push(format!("Expires after {expiry:?}"));
    }
    if page.orders_commands() && offer.stop_queue_on_failure {
        details.push("Stop on failure".into());
    }
    if page.roster_preferences.require_all_ready {
        details.push("All recipients must be ready".into());
    }
    if offer.canary.is_some() {
        details.push("Canary first".into());
    }
    details.join(" · ")
}
