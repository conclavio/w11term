use super::*;

pub enum Action {
    Open,
    Visit { pane: u64, sequence: u64 },
    MarkAllRead,
    Clear,
}

impl TerminalApp {
    pub(super) fn handle_attention(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Open => {
                let existing = self
                    .tabs
                    .entries()
                    .iter()
                    .find(|entry| matches!(entry.value.page, Page::Attention))
                    .map(|entry| entry.id);
                if let Some(id) = existing {
                    self.tabs.select(id);
                } else {
                    self.tabs.insert(AppTab {
                        label: "Attention inbox".into(),
                        page: Page::Attention,
                    });
                }
            }
            Action::Visit { pane, sequence } => {
                let Some(id) = self.tab_for_pane(pane) else {
                    return;
                };
                if !self.inbox.mark_read(pane, sequence) {
                    return;
                }
                self.tabs.select(id);
                if matches!(self.tabs.get(id), Some(AppTab { page: Page::Terminal(live), .. }) if live.find.open)
                {
                    self.find(true, None, ctx);
                }
            }
            Action::MarkAllRead => {
                self.inbox.mark_all_read();
                return;
            }
            Action::Clear => {
                self.inbox.clear();
                return;
            }
        }
        self.sync_selection(ctx);
        self.activate(ctx);
    }

    pub(super) fn inbox_button(&self, ctx: &ViewContext<Self>) -> View {
        let unread = self.inbox.unread_count();
        let text = if unread == 0 {
            "Inbox".into()
        } else {
            format!("Inbox ({unread})")
        };
        Button::new()
            .style(ButtonStyle::Subtle)
            .height(36.0)
            .automation_name(format!("Attention inbox, {unread} unread"))
            .on_click(ctx.callback(|_| Message::Attention(Action::Open)))
            .content(text)
    }

    pub(super) fn attention_view(&self, ctx: &ViewContext<Self>) -> View {
        let rows = self.inbox.rows();
        let content = if rows.is_empty() {
            TextBlock::new().text("No bells or notifications").into()
        } else {
            ScrollViewer::new().content(StackPanel::new().spacing(4.0).keyed_children(
                rows.iter().map(|row| {
                    let pane = row.pane;
                    let sequence = row.sequence;
                    let tab = self.tab_for_pane(pane).and_then(|id| self.tabs.get(id));
                    let source = tab.map_or_else(
                        || format!("{} (closed)", row.source),
                        |tab| tab.label.clone(),
                    );
                    let status = if row.unread { "Unread" } else { "Read" };
                    let label = format!("{status} · {source} · Pane {pane} · {}", row.message);
                    KeyedView::new(
                        format!("{pane}/{sequence}"),
                        Button::new()
                            .automation_name(label.clone())
                            .is_enabled(tab.is_some())
                            .on_click(ctx.callback(move |_| {
                                Message::Attention(Action::Visit { pane, sequence })
                            }))
                            .content(
                                TextBlock::new()
                                    .text(label)
                                    .text_wrapping(TextWrapping::Wrap),
                            ),
                    )
                }),
            ))
        };
        Grid::new()
            .margin(16.0)
            .row_spacing(12.0)
            .rows([GridLength::Auto, GridLength::Auto, GridLength::STAR])
            .children((
                TextBlock::new().text("Attention inbox").font_size(24.0),
                StackPanel::new()
                    .grid_row(1)
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(rows.iter().any(|row| row.unread))
                            .on_click(ctx.callback(|_| Message::Attention(Action::MarkAllRead)))
                            .content("Mark all read"),
                        Button::new()
                            .is_enabled(!rows.is_empty())
                            .on_click(ctx.callback(|_| Message::Attention(Action::Clear)))
                            .content("Clear inbox"),
                    )),
                Border::new().grid_row(2).content(content),
            ))
    }
}
