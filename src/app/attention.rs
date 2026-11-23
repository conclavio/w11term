use super::*;

pub(super) const LONG_COMMANDS: &str = "Notify for long commands";

pub(super) fn long_command_toggle(label: &str) -> Option<bool> {
    match label.strip_prefix(LONG_COMMANDS)? {
        ": Off" => Some(true),
        ": On" => Some(false),
        _ => None,
    }
}

pub(super) fn set_long_commands(origin: Option<clipboard_paste::Origin>, enabled: bool) -> Message {
    origin.map_or(Message::ControlIdle, |origin| {
        Message::Attention(Action::LongCommands(origin, enabled))
    })
}

pub enum Action {
    Open,
    WatchCommand(bool),
    LongCommands(clipboard_paste::Origin, bool),
    Visit { pane: u64, sequence: u64 },
    ViewCommand { pane: u64, sequence: u64 },
    MarkAllRead,
    Clear,
}

impl TerminalApp {
    pub(super) fn handle_attention(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::LongCommands(origin, enabled) => {
                if self.paste_origin() == Some(origin)
                    && let Some(AppTab {
                        page: Page::Terminal(live),
                        ..
                    }) = self.tabs.active()
                {
                    live.session
                        .events
                        .set_long_command_notifications(origin.connection, enabled);
                }
                return;
            }
            Action::WatchCommand(arm) => {
                if let Some(AppTab {
                    page: Page::Terminal(live),
                    ..
                }) = self.tabs.active()
                {
                    live.session.events.set_command_watch(arm);
                }
                return;
            }
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
            Action::ViewCommand { pane, sequence } => {
                if let Some(link) = self.inbox.command_link(pane, sequence)
                    && self.open_command_history(pane, Some(link), ctx)
                {
                    self.inbox.mark_read(pane, sequence);
                }
                return;
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
                    let visit = Button::new()
                        .automation_name(label.clone())
                        .is_enabled(tab.is_some())
                        .on_click(ctx.callback(move |_| {
                            Message::Attention(Action::Visit { pane, sequence })
                        }))
                        .content(
                            TextBlock::new()
                                .text(label)
                                .text_wrapping(TextWrapping::Wrap),
                        );
                    let command = row.command.filter(|&link| {
                        self.history_frame(pane)
                            .is_some_and(|frame| frame.has_command(link))
                    });
                    let content: View = if let Some(command) = command {
                        Grid::new()
                            .columns([GridLength::STAR, GridLength::Auto])
                            .column_spacing(8.0)
                            .children((
                                visit,
                                Button::new()
                                    .grid_column(1)
                                    .style(ButtonStyle::Subtle)
                                    .automation_name(format!(
                                        "View command {} for {source}, pane {pane}",
                                        command.id
                                    ))
                                    .on_click(ctx.callback(move |_| {
                                        Message::Attention(Action::ViewCommand { pane, sequence })
                                    }))
                                    .content("View command"),
                            ))
                    } else {
                        visit
                    };
                    KeyedView::new(format!("{pane}/{sequence}"), content)
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

pub(super) fn flash_background_window(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GA_ROOTOWNER, GetAncestor, GetForegroundWindow,
    };
    unsafe {
        let foreground = GetForegroundWindow();
        if !foreground.is_invalid()
            && foreground != hwnd
            && GetAncestor(foreground, GA_ROOTOWNER) != hwnd
        {
            let _ = FlashWindowEx(&FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd,
                dwFlags: FLASHW_TRAY,
                uCount: 3,
                dwTimeout: 0,
            });
        }
    }
}
