use super::*;
use w11term::events::{NoticeTarget, SshAttemptOutcome};

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
    VisitGroup(NoticeTarget),
    ViewCommand { pane: u64, sequence: u64 },
    ViewLoginResults(NoticeTarget),
    BackFromLoginResults(NoticeTarget),
    VisitLoginPane { target: NoticeTarget, pane: u64 },
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
                    .find(|entry| matches!(entry.value.page, Page::Attention(_)))
                    .map(|entry| entry.id);
                if let Some(id) = existing {
                    if let Some(Page::Attention(target)) =
                        self.tabs.get_mut(id).map(|tab| &mut tab.page)
                    {
                        *target = None;
                    }
                    self.tabs.select(id);
                } else {
                    self.tabs.insert(AppTab {
                        label: "Attention inbox".into(),
                        page: Page::Attention(None),
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
            Action::VisitGroup(target) => match target {
                NoticeTarget::GroupRun { tab, submission }
                    if self.group_run_retained(tab, submission)
                        && self.inbox.mark_target_read(target) =>
                {
                    self.show_group_run(tab, submission);
                }
                NoticeTarget::GroupLogins { tab, .. }
                    if matches!(
                        self.tabs.get(tab).map(|entry| &entry.page),
                        Some(Page::HostGroup(_))
                    ) && self.inbox.mark_target_read(target) =>
                {
                    self.tabs.select(tab);
                }
                _ => return,
            },
            Action::ViewCommand { pane, sequence } => {
                if let Some(link) = self.inbox.command_link(pane, sequence)
                    && self.open_command_history(pane, Some(link), ctx)
                {
                    self.inbox.mark_read(pane, sequence);
                }
                return;
            }
            Action::ViewLoginResults(target) => {
                if self.inbox.group_login_notice(target).is_some()
                    && let Some(id) = self.tabs.active_id()
                    && let Some(Page::Attention(view)) =
                        self.tabs.get_mut(id).map(|tab| &mut tab.page)
                    && view.is_none()
                    && self.inbox.mark_target_read(target)
                {
                    *view = Some(target);
                }
                return;
            }
            Action::BackFromLoginResults(target) => {
                if let Some(id) = self.tabs.active_id()
                    && let Some(Page::Attention(view)) =
                        self.tabs.get_mut(id).map(|tab| &mut tab.page)
                    && *view == Some(target)
                {
                    *view = None;
                }
                return;
            }
            Action::VisitLoginPane { target, pane } => {
                if matches!(self.tabs.active().map(|tab| &tab.page), Some(Page::Attention(Some(open))) if *open == target)
                    && self
                        .inbox
                        .group_login_notice(target)
                        .and_then(|row| row.login_results)
                        .is_some_and(|results| results.iter().any(|result| result.pane == pane))
                    && let Some(id) = self.tab_for_pane(pane)
                {
                    self.tabs.select(id);
                } else {
                    return;
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

    pub(super) fn attention_view(
        &self,
        login_results: Option<NoticeTarget>,
        ctx: &ViewContext<Self>,
    ) -> View {
        if let Some(target) = login_results {
            return self.login_results_view(target, ctx);
        }
        let rows = self.inbox.rows();
        let content = if rows.is_empty() {
            TextBlock::new().text("No bells or notifications").into()
        } else {
            ScrollViewer::new().content(StackPanel::new().spacing(4.0).keyed_children(
                rows.iter().map(|row| {
                    let status = if row.unread { "Unread" } else { "Read" };
                    let (pane, sequence) = match row.target {
                        NoticeTarget::Pane { pane, sequence } => (pane, sequence),
                        target @ (NoticeTarget::GroupRun { tab, .. }
                        | NoticeTarget::GroupLogins { tab, .. }) => {
                            let (enabled, detail, key) = match target {
                                NoticeTarget::GroupRun { submission, .. } => {
                                    let enabled = self.group_run_retained(tab, submission);
                                    let expired = if enabled {
                                        ""
                                    } else {
                                        " (results unavailable)"
                                    };
                                    (
                                        enabled,
                                        format!(" · Run {submission}{expired}"),
                                        format!("group-{tab}/{submission}"),
                                    )
                                }
                                NoticeTarget::GroupLogins { sequence, .. } => {
                                    let enabled = matches!(
                                        self.tabs.get(tab).map(|entry| &entry.page),
                                        Some(Page::HostGroup(_))
                                    );
                                    let detail = if enabled { "" } else { " (closed)" };
                                    (
                                        enabled,
                                        detail.into(),
                                        format!("group-logins-{tab}/{sequence}"),
                                    )
                                }
                                NoticeTarget::Pane { .. } => unreachable!(),
                            };
                            let source = self
                                .tabs
                                .get(tab)
                                .map_or(row.source.as_ref(), |tab| tab.label.as_str());
                            let label = format!("{status} · {source}{detail} · {}", row.message);
                            let visit = Button::new()
                                    .automation_name(label.clone())
                                    .is_enabled(enabled)
                                    .on_click(ctx.callback(move |_| {
                                        Message::Attention(Action::VisitGroup(target))
                                    }))
                                    .content(
                                        TextBlock::new()
                                            .text(label)
                                            .text_wrapping(TextWrapping::Wrap),
                                    );
                            let content = if let NoticeTarget::GroupLogins { sequence, .. } = target {
                                Grid::new()
                                    .columns([GridLength::STAR, GridLength::Auto])
                                    .column_spacing(8.0)
                                    .children((
                                        visit,
                                        Button::new()
                                            .grid_column(1)
                                            .style(ButtonStyle::Subtle)
                                            .automation_name(format!("View SSH login results for {source}, notice {sequence}"))
                                            .on_click(ctx.callback(move |_| Message::Attention(Action::ViewLoginResults(target))))
                                            .content("View login results"),
                                    ))
                            } else {
                                visit
                            };
                            return KeyedView::new(key, content);
                        }
                    };
                    let tab = self.tab_for_pane(pane).and_then(|id| self.tabs.get(id));
                    let source = tab.map_or_else(
                        || format!("{} (closed)", row.source),
                        |tab| tab.label.clone(),
                    );
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

    fn login_results_view(&self, target: NoticeTarget, ctx: &ViewContext<Self>) -> View {
        let notice = self.inbox.group_login_notice(target);
        let content = notice
            .as_ref()
            .and_then(|notice| notice.login_results.as_deref())
            .map_or_else(
                || {
                    TextBlock::new()
                        .text("These login results have expired")
                        .into()
                },
                |results| {
                    ScrollViewer::new().content(StackPanel::new().spacing(8.0).keyed_children(
                        results.iter().map(|result| {
                            let pane = result.pane;
                            let (status, reason) = match &result.outcome {
                                SshAttemptOutcome::Connected => ("Connected", None),
                                SshAttemptOutcome::Failed(reason) => {
                                    ("Failed", Some(reason.as_ref()))
                                }
                                SshAttemptOutcome::Stopped => ("Stopped", None),
                                SshAttemptOutcome::Closed => ("Closed", None),
                            };
                            let diagnostic = reason
                                .filter(|reason| !reason.is_empty())
                                .map_or_else(View::empty, |reason| {
                                    TextBlock::new()
                                        .text(reason)
                                        .text_wrapping(TextWrapping::Wrap)
                                        .into()
                                });
                            KeyedView::new(
                                pane,
                                Grid::new()
                                    .columns([GridLength::STAR, GridLength::Auto])
                                    .column_spacing(8.0)
                                    .children((
                                        StackPanel::new().spacing(2.0).children((
                                            TextBlock::new()
                                                .text(format!("{} · {status}", result.host))
                                                .text_wrapping(TextWrapping::Wrap),
                                            diagnostic,
                                        )),
                                        Button::new()
                                            .grid_column(1)
                                            .style(ButtonStyle::Subtle)
                                            .is_enabled(self.tab_for_pane(pane).is_some())
                                            .automation_name(format!("Open SSH login pane {pane}"))
                                            .on_click(ctx.callback(move |_| {
                                                Message::Attention(Action::VisitLoginPane {
                                                    target,
                                                    pane,
                                                })
                                            }))
                                            .content("Open pane"),
                                    )),
                            )
                        }),
                    ))
                },
            );
        Grid::new()
            .margin(16.0)
            .row_spacing(12.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(8.0).children((
                    TextBlock::new().text("SSH login results").font_size(24.0),
                    notice.as_ref().map_or_else(View::empty, |notice| {
                        TextBlock::new().text(notice.source.as_ref()).into()
                    }),
                    Button::new()
                        .on_click(ctx.callback(move |_| {
                            Message::Attention(Action::BackFromLoginResults(target))
                        }))
                        .content("Back to inbox"),
                )),
                Border::new().grid_row(1).content(content),
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
