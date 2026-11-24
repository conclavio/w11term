use super::{host_groups::FailedLogin, *};
use std::{fmt::Write as _, io::Write as _};
use w11term::events::{NoticeTarget, SshAttemptOutcome, SshLoginResult};

mod group_output;

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
    Visit {
        pane: u64,
        sequence: u64,
    },
    VisitGroup(NoticeTarget),
    ViewGroupOutput(NoticeTarget),
    ViewCommand {
        pane: u64,
        sequence: u64,
    },
    ViewLoginResults(NoticeTarget),
    BackFromLoginResults(NoticeTarget),
    VisitLoginPane {
        target: NoticeTarget,
        pane: u64,
    },
    ExportLoginReport(NoticeTarget, LoginReportAction),
    ReviewLoginReconnect(NoticeTarget),
    FinishLoginReconnect {
        target: NoticeTarget,
        offered: Arc<[FailedLogin]>,
        reconnect: bool,
    },
    MarkAllRead,
    Clear,
}

pub enum LoginReportAction {
    Copy,
    Save,
}

pub(super) enum AttentionPage {
    Inbox,
    GroupOutput(group_output::MatchView),
    Logins {
        target: NoticeTarget,
        changed: bool,
    },
    Reconnect {
        target: NoticeTarget,
        offered: Arc<[FailedLogin]>,
    },
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
                    if let Some(Page::Attention(page)) =
                        self.tabs.get_mut(id).map(|tab| &mut tab.page)
                    {
                        *page = AttentionPage::Inbox;
                    }
                    self.tabs.select(id);
                } else {
                    self.tabs.insert(AppTab {
                        label: "Attention inbox".into(),
                        page: Page::Attention(AttentionPage::Inbox),
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
                | NoticeTarget::GroupCanary { tab, submission }
                | NoticeTarget::GroupOutputMissing { tab, submission }
                | NoticeTarget::GroupOutput {
                    tab, submission, ..
                } if self.group_run_retained(tab, submission)
                    && (matches!(self.active_attention(), Some(AttentionPage::GroupOutput(output))
                        if output.target == target)
                        || self.inbox.mark_target_read(target)) =>
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
            Action::ViewGroupOutput(target) => self.open_group_output_match(target),
            Action::ViewCommand { pane, sequence } => {
                if let Some(link) = self.inbox.command_link(pane, sequence)
                    && self.open_command_history(pane, Some(link), ctx)
                {
                    self.inbox.mark_read(pane, sequence);
                }
                return;
            }
            Action::ViewLoginResults(target) => {
                if matches!(self.active_attention(), Some(AttentionPage::Inbox))
                    && self.inbox.group_login_notice(target).is_some()
                    && self.inbox.mark_target_read(target)
                {
                    self.set_attention(AttentionPage::Logins {
                        target,
                        changed: false,
                    });
                }
                return;
            }
            Action::BackFromLoginResults(target) => {
                if self.viewing_login_results(target) {
                    self.set_attention(AttentionPage::Inbox);
                }
                return;
            }
            Action::VisitLoginPane { target, pane } => {
                if self.viewing_login_results(target)
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
            Action::ExportLoginReport(target, action) => {
                self.export_login_report(target, action, ctx);
                return;
            }
            Action::ReviewLoginReconnect(target) => {
                if self.viewing_login_results(target) {
                    let offered = self.eligible_login_reconnects(target);
                    if offered.is_empty() {
                        self.set_attention(AttentionPage::Logins {
                            target,
                            changed: true,
                        });
                    } else {
                        self.set_attention(AttentionPage::Reconnect {
                            target,
                            offered: offered.into(),
                        });
                    }
                }
                return;
            }
            Action::FinishLoginReconnect {
                target,
                offered,
                reconnect,
            } => {
                if !self.finish_login_reconnect(target, &offered, reconnect) {
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

    pub(super) fn attention_view(&self, page: &AttentionPage, ctx: &ViewContext<Self>) -> View {
        match page {
            AttentionPage::Inbox => {}
            AttentionPage::GroupOutput(output) => return self.group_output_match_view(output, ctx),
            AttentionPage::Logins { target, changed } => {
                return self.login_results_view(*target, *changed, ctx);
            }
            AttentionPage::Reconnect { target, offered } => {
                return self.login_reconnect_view(*target, offered, ctx);
            }
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
                        | NoticeTarget::GroupCanary { tab, .. }
                        | NoticeTarget::GroupOutput { tab, .. }
                        | NoticeTarget::GroupOutputMissing { tab, .. }
                        | NoticeTarget::GroupLogins { tab, .. }) => {
                            let (enabled, detail, key) = match target {
                                NoticeTarget::GroupRun { submission, .. }
                                | NoticeTarget::GroupCanary { submission, .. }
                                | NoticeTarget::GroupOutputMissing { submission, .. }
                                | NoticeTarget::GroupOutput { submission, .. } => {
                                    let key = match target {
                                        NoticeTarget::GroupCanary { .. } => format!("group-canary-{tab}/{submission}"),
                                        NoticeTarget::GroupOutputMissing { .. } => format!("group-output-missing-{tab}/{submission}"),
                                        NoticeTarget::GroupOutput { line_id, .. } => {
                                            format!("group-output-{tab}/{submission}/{line_id}")
                                        }
                                        _ => format!("group-run-{tab}/{submission}"),
                                    };
                                    let enabled = self.group_run_retained(tab, submission);
                                    let expired = if enabled {
                                        ""
                                    } else {
                                        " (results unavailable)"
                                    };
                                    (
                                        enabled,
                                        format!(" · Run {submission}{expired}"),
                                        key,
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
                            let details = match target {
                                NoticeTarget::GroupLogins { sequence, .. } => Some((
                                    format!("View SSH login results for {source}, notice {sequence}"),
                                    "View login results",
                                    Action::ViewLoginResults as fn(NoticeTarget) -> Action,
                                )),
                                NoticeTarget::GroupOutput { submission, line_id, .. } => Some((
                                    format!("View matching output for {source}, run {submission}, line {line_id}"),
                                    "View matching output",
                                    Action::ViewGroupOutput as fn(NoticeTarget) -> Action,
                                )),
                                _ => None,
                            };
                            let content = if let Some((name, label, action)) = details {
                                Grid::new()
                                    .columns([GridLength::STAR, GridLength::Auto])
                                    .column_spacing(8.0)
                                    .children((
                                        visit,
                                        Button::new()
                                            .grid_column(1)
                                            .style(ButtonStyle::Subtle)
                                            .automation_name(name)
                                            .on_click(ctx.callback(move |_| Message::Attention(action(target))))
                                            .content(label),
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

    fn viewing_login_results(&self, target: NoticeTarget) -> bool {
        matches!(self.active_attention(), Some(AttentionPage::Logins { target: open, .. }) if *open == target)
    }

    fn active_attention(&self) -> Option<&AttentionPage> {
        match &self.tabs.active()?.page {
            Page::Attention(page) => Some(page),
            _ => None,
        }
    }

    fn set_attention(&mut self, next: AttentionPage) {
        if let Some(id) = self.tabs.active_id()
            && let Some(Page::Attention(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page)
        {
            *page = next;
        }
    }

    fn eligible_login_reconnects(&self, target: NoticeTarget) -> Vec<FailedLogin> {
        let NoticeTarget::GroupLogins { tab, .. } = target else {
            return Vec::new();
        };
        let Some(results) = self
            .inbox
            .group_login_notice(target)
            .and_then(|row| row.login_results)
        else {
            return Vec::new();
        };
        self.report_failed_logins(tab, &results)
    }

    fn finish_login_reconnect(
        &mut self,
        target: NoticeTarget,
        offered: &Arc<[FailedLogin]>,
        reconnect: bool,
    ) -> bool {
        if !matches!(self.active_attention(), Some(AttentionPage::Reconnect { target: open, offered: visible })
            if *open == target && Arc::ptr_eq(offered, visible))
        {
            return false;
        }
        if !reconnect {
            self.set_attention(AttentionPage::Logins {
                target,
                changed: false,
            });
            return false;
        }
        let NoticeTarget::GroupLogins { tab, .. } = target else {
            return false;
        };
        let changed = self.eligible_login_reconnects(target) != offered.as_ref();
        self.set_attention(AttentionPage::Logins { target, changed });
        if changed {
            return false;
        }
        self.restart_report_failed_logins(tab, offered);
        true
    }

    fn export_login_report(
        &mut self,
        target: NoticeTarget,
        action: LoginReportAction,
        ctx: &ComponentContext<Self>,
    ) {
        if !self.viewing_login_results(target) {
            return;
        }
        let NoticeTarget::GroupLogins { sequence, .. } = target else {
            return;
        };
        let Some(notice) = self.inbox.group_login_notice(target) else {
            return;
        };
        let Some(results) = notice.login_results.as_deref() else {
            return;
        };
        let text = login_report_text(&notice.source, &notice.message, results);
        match action {
            LoginReportAction::Copy => {
                self.export.status = Some(match w11term::clipboard::write(&text) {
                    Ok(()) => "Copied SSH login report".into(),
                    Err(error) => format!("Could not copy SSH login report. {error}"),
                });
            }
            LoginReportAction::Save => self.begin_text_export(
                text_export::Subject::SshLoginReport(sequence),
                move |file| file.write_all(text.as_bytes()),
                ctx,
            ),
        }
    }

    fn login_results_view(
        &self,
        target: NoticeTarget,
        changed: bool,
        ctx: &ViewContext<Self>,
    ) -> View {
        let notice = self.inbox.group_login_notice(target);
        let available = notice.is_some();
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
                            let (status, reason) = login_outcome(&result.outcome);
                            let diagnostic = reason.map_or_else(View::empty, |reason| {
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
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .is_enabled(available)
                                .on_click(ctx.callback(move |_| {
                                    Message::Attention(Action::ExportLoginReport(
                                        target,
                                        LoginReportAction::Copy,
                                    ))
                                }))
                                .content("Copy login report"),
                            Button::new()
                                .is_enabled(available && self.export.pending.is_none())
                                .on_click(ctx.callback(move |_| {
                                    Message::Attention(Action::ExportLoginReport(
                                        target,
                                        LoginReportAction::Save,
                                    ))
                                }))
                                .content("Save login report…"),
                            Button::new()
                                .on_click(ctx.callback(move |_| {
                                    Message::Attention(Action::BackFromLoginResults(target))
                                }))
                                .content("Back to inbox"),
                        )),
                    if self.eligible_login_reconnects(target).is_empty() {
                        View::empty()
                    } else {
                        Button::new()
                            .on_click(ctx.callback(move |_| {
                                Message::Attention(Action::ReviewLoginReconnect(target))
                            }))
                            .content("Review failed logins")
                    },
                    if changed {
                        TextBlock::new()
                            .text("The failed logins changed. Review the remaining hosts again.")
                            .text_wrapping(TextWrapping::Wrap)
                            .into()
                    } else {
                        View::empty()
                    },
                )),
                Border::new().grid_row(1).content(content),
            ))
    }

    fn login_reconnect_view(
        &self,
        target: NoticeTarget,
        offered: &Arc<[FailedLogin]>,
        ctx: &ViewContext<Self>,
    ) -> View {
        let confirm = offered.clone();
        let cancel = offered.clone();
        let count = offered.len();
        let hosts = if count == 1 { "host" } else { "hosts" };
        Grid::new()
            .margin(16.0)
            .row_spacing(12.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(8.0).children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text("Review SSH reconnects"),
                    TextBlock::new().text("Start a fresh SSH login in each listed pane."),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .automation_name("Reconnect reviewed logins")
                                .on_click(ctx.callback(move |_| {
                                    Message::Attention(Action::FinishLoginReconnect {
                                        target,
                                        offered: confirm.clone(),
                                        reconnect: true,
                                    })
                                }))
                                .content(format!("Reconnect {count} {hosts}")),
                            Button::new()
                                .on_click(ctx.callback(move |_| {
                                    Message::Attention(Action::FinishLoginReconnect {
                                        target,
                                        offered: cancel.clone(),
                                        reconnect: false,
                                    })
                                }))
                                .content("Cancel reconnect review"),
                        )),
                )),
                ScrollViewer::new().grid_row(1).content(
                    StackPanel::new()
                        .spacing(6.0)
                        .keyed_children(offered.iter().enumerate().map(|(index, result)| {
                            KeyedView::new(
                                index,
                                TextBlock::new()
                                    .text(&result.host)
                                    .text_wrapping(TextWrapping::Wrap),
                            )
                        })),
                ),
            ))
    }
}

fn login_outcome(outcome: &SshAttemptOutcome) -> (&'static str, Option<&str>) {
    match outcome {
        SshAttemptOutcome::Connected => ("Connected", None),
        SshAttemptOutcome::Failed(reason) => (
            "Failed",
            Some(reason.as_ref()).filter(|reason| !reason.is_empty()),
        ),
        SshAttemptOutcome::Stopped => ("Stopped", None),
        SshAttemptOutcome::Closed => ("Closed", None),
    }
}

fn login_report_text(source: &str, summary: &str, results: &[SshLoginResult]) -> String {
    let mut text = format!("SSH login results\n{source}\n{summary}\n\n");
    for result in results {
        let (status, reason) = login_outcome(&result.outcome);
        writeln!(text, "{} · {status}", result.host).expect("String writes cannot fail");
        if let Some(reason) = reason {
            writeln!(text, "  {reason}").expect("String writes cannot fail");
        }
    }
    text
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
