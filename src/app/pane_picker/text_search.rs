use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};
use w11term::copy_mode::ScreenId;
use w11term::search::{MAX_QUERY_BYTES, PaneSearchAnchor, PaneSearchPreview};

// Limit the number of terminal workers scanning history at the same time.
const PARALLEL_PANES: usize = 4;
const PANE_TIMEOUT: Duration = Duration::from_secs(5);
static NEXT_QUERY: AtomicU64 = AtomicU64::new(1);

pub enum Action {
    Edit(u64, String),
    Start(u64),
    Ready(u64, Report),
    Choose(u64, u64),
    Revealed(u64, u64, ScreenId, bool),
    Unavailable(u64),
    Refresh(u64),
}

struct Target {
    pane: u64,
    order: usize,
    caption: String,
    input: mpsc::UnboundedSender<Command>,
}

pub(super) struct Row {
    pane: u64,
    order: usize,
    caption: String,
    anchor: PaneSearchAnchor,
}

#[derive(Default)]
pub struct Report {
    rows: Vec<Row>,
    updating: usize,
    closed: usize,
}

impl Report {
    fn add(&mut self, target: Target, preview: PaneSearchPreview) {
        self.updating += usize::from(preview.updating);
        if let Some((excerpt, anchor)) = preview.first {
            let plus = if preview.truncated { "+" } else { "" };
            let unit = if preview.total == 1 {
                "match"
            } else {
                "matches"
            };
            let caption = format!(
                "Pane {} · {} · {}{plus} {unit} · {excerpt}",
                target.pane, target.caption, preview.total
            );
            self.rows.push(Row {
                pane: target.pane,
                order: target.order,
                caption,
                anchor,
            });
        }
    }
}

async fn scan(targets: Vec<Target>, query: &str) -> Report {
    let mut report = Report::default();
    let mut remaining = targets.into_iter();
    let mut pending = tokio::task::JoinSet::new();
    loop {
        while pending.len() < PARALLEL_PANES {
            let Some(target) = remaining.next() else {
                break;
            };
            let (reply, result) = oneshot::channel();
            if target
                .input
                .send(Command::SearchPreview {
                    query: query.into(),
                    reply,
                })
                .is_ok()
            {
                pending.spawn(async move {
                    let preview = tokio::time::timeout(PANE_TIMEOUT, result)
                        .await
                        .ok()
                        .and_then(Result::ok);
                    (target, preview)
                });
            } else {
                report.closed += 1;
            }
        }
        match pending.join_next().await {
            Some(Ok((target, Some(preview)))) => report.add(target, preview),
            Some(_) => report.closed += 1,
            None => break,
        }
    }
    // Keyboard order follows the tab strip and split layout even when a later pane replies first.
    report.rows.sort_by_key(|row| row.order);
    report
}

fn wait_for_query<T>(work: impl Future<Output = T>, stop: oneshot::Receiver<()>) -> Option<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .ok()?
        .block_on(async {
            tokio::select! {
                biased;
                _ = stop => None,
                result = work => Some(result),
            }
        })
}

pub(super) struct Results {
    instance: u64,
    epoch: u64,
    pub(super) rows: Vec<Row>,
    status: String,
    task: Option<ComponentTask>,
    stop: Option<oneshot::Sender<()>>,
    pub(super) revealing: bool,
}

impl Default for Results {
    fn default() -> Self {
        let instance = NEXT_QUERY.fetch_add(1, Ordering::Relaxed);
        Self {
            instance,
            epoch: instance,
            rows: vec![],
            status: String::new(),
            task: None,
            stop: None,
            revealing: false,
        }
    }
}

impl Results {
    fn cancel(&mut self) {
        self.stop.take();
        self.revealing = false;
        if let Some(task) = self.task.take() {
            task.cancel();
        }
    }

    fn set_report(&mut self, report: Report) {
        self.rows = report.rows;
        self.status = match self.rows.len() {
            0 => "No matching panes".into(),
            1 => "1 matching pane".into(),
            count => format!("{count} matching panes"),
        };
        if report.updating != 0 {
            self.status.push_str(&format!(
                " · {} updating, refresh to search again",
                report.updating
            ));
        }
        if report.closed != 0 {
            self.status
                .push_str(&format!(" · {} unavailable", report.closed));
        }
    }
}

impl Drop for Results {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Picker {
    pub(super) fn text_choice(&self, index: usize) -> Option<Action> {
        let row = self.text.rows.get(index)?;
        Some(Action::Choose(self.text.epoch, row.pane))
    }

    pub(super) fn text_view(&self, app: &TerminalApp, ctx: &ViewContext<TerminalApp>) -> View {
        let epoch = self.text.epoch;
        let instance = self.text.instance;
        let query = Grid::new()
            .columns([GridLength::STAR, GridLength::Auto, GridLength::Auto])
            .children((
                TextBox::new()
                    .element_ref(&self.input)
                    .text(&self.query)
                    .is_enabled(!self.text.revealing)
                    .placeholder_text("Find text in this window’s panes")
                    .automation_name("Find in all panes")
                    .on_text_changed(
                        ctx.callback(move |text| Message::PaneText(Action::Edit(instance, text))),
                    ),
                Button::new()
                    .grid_column(1)
                    .style(ButtonStyle::Subtle)
                    .is_enabled(
                        !self.text.revealing
                            && !self.query.is_empty()
                            && self.query.len() <= MAX_QUERY_BYTES,
                    )
                    .automation_name("Refresh pane search")
                    .on_click(ctx.callback(move |_| Message::PaneText(Action::Refresh(epoch))))
                    .content("Refresh"),
                Button::new()
                    .grid_column(2)
                    .style(ButtonStyle::Subtle)
                    .is_enabled(!self.text.revealing)
                    .automation_name("Close Find in all panes")
                    .on_click(ctx.callback(|_| Message::Shortcut(Shortcut::PickerCancel)))
                    .content("×"),
            ));
        let page = ((app.window_height - 165.0) / 40.0).clamp(1.0, 8.0) as usize;
        let first = self.selected / page * page;
        let mut results = vec![];
        for (i, row) in self.text.rows.iter().enumerate().skip(first).take(page) {
            let pane = row.pane;
            let button = Button::new()
                .automation_name(&row.caption)
                .is_enabled(!self.text.revealing)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .style(if i == self.selected {
                    ButtonStyle::Accent
                } else {
                    ButtonStyle::Subtle
                })
                .on_click(ctx.callback(move |_| Message::PaneText(Action::Choose(epoch, pane))))
                .content(TextBlock::new().text(&row.caption));
            results.push(KeyedView::new(format!("text-pane-{pane}"), button));
        }
        if self.text.rows.len() > page {
            results.push(KeyedView::new(
                "pages",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(first > 0)
                            .automation_name("Previous matching panes")
                            .on_click(ctx.callback(move |_| {
                                Message::Shortcut(Shortcut::PickerMove(-(page as isize)))
                            }))
                            .content("Previous"),
                        Button::new()
                            .is_enabled(first + page < self.text.rows.len())
                            .automation_name("Next matching panes")
                            .on_click(ctx.callback(move |_| {
                                Message::Shortcut(Shortcut::PickerMove(page as isize))
                            }))
                            .content("Next"),
                    )),
            ));
        }
        let list = StackPanel::new().spacing(4.0).keyed_children(results);
        let status = TextBlock::new()
            .text(&self.text.status)
            .automation_name(&self.text.status);
        Self::overlay(
            StackPanel::new()
                .spacing(6.0)
                .children((query, status, list)),
            (app.width - 32.0).clamp(1.0, 780.0),
        )
    }
}

impl TerminalApp {
    pub(in crate::app) fn announce_text_selection(&self) {
        let Some(picker) = self
            .picker
            .as_ref()
            .filter(|picker| picker.mode == Mode::Text)
        else {
            return;
        };
        let announcement = match picker.text.rows.get(picker.selected) {
            Some(row) => format!(
                "{}. {} of {}. {}",
                picker.text.status,
                picker.selected + 1,
                picker.text.rows.len(),
                row.caption
            ),
            None => picker.text.status.clone(),
        };
        use windows::Win32::UI::Accessibility::{
            NotificationKind_Other, NotificationProcessing_MostRecent, UiaClientsAreListening,
            UiaHostProviderFromHwnd, UiaRaiseNotificationEvent,
        };
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        use windows::core::BSTR;
        if let Some(hwnd) = self.keyboard.hwnd() {
            unsafe {
                if GetForegroundWindow() == hwnd
                    && UiaClientsAreListening().as_bool()
                    && let Ok(provider) = UiaHostProviderFromHwnd(hwnd)
                {
                    let _ = UiaRaiseNotificationEvent(
                        &provider,
                        NotificationKind_Other,
                        NotificationProcessing_MostRecent,
                        &BSTR::from(announcement.as_str()),
                        &BSTR::from("w11term.find-all-panes"),
                    );
                }
            }
        }
    }

    pub(super) fn edit_text_query(&mut self, mut query: String, ctx: &ComponentContext<Self>) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        if picker.text.revealing {
            return;
        }
        picker.text.cancel();
        picker.text.rows.clear();
        picker.selected = 0;
        // Preserve the overflow indication, including when the boundary splits a UTF-8 scalar.
        Find::limit_query(&mut query, true);
        picker.query = query;
        let epoch = NEXT_QUERY.fetch_add(1, Ordering::Relaxed);
        picker.text.epoch = epoch;
        if picker.query.is_empty() {
            picker.text.status.clear();
            return;
        }
        if picker.query.len() > MAX_QUERY_BYTES {
            picker.text.status = format!("Search text exceeds {MAX_QUERY_BYTES} UTF-8 bytes");
            return;
        }
        picker.text.status = "Searching…".into();
        picker.text.task = Some(ctx.spawn_background_with_rejection(
            move |_| {
                std::thread::sleep(Duration::from_millis(120));
                Message::PaneText(Action::Start(epoch))
            },
            Message::PaneText(Action::Unavailable(epoch)),
        ));
    }

    pub(in crate::app) fn pane_text_action(
        &mut self,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(picker) = self.picker.as_mut().filter(|p| p.mode == Mode::Text) else {
            return;
        };
        let epoch = picker.text.epoch;
        let current = match &action {
            Action::Edit(instance, _) => *instance == picker.text.instance,
            Action::Start(e)
            | Action::Refresh(e)
            | Action::Unavailable(e)
            | Action::Ready(e, _)
            | Action::Choose(e, _)
            | Action::Revealed(e, ..) => *e == epoch,
        };
        if !current {
            return;
        }
        match action {
            Action::Edit(_, query) => {
                if query != picker.query {
                    self.edit_text_query(query, ctx);
                }
            }
            Action::Refresh(_) => {
                let query = picker.query.clone();
                self.edit_text_query(query, ctx);
            }
            Action::Start(_) => {
                let query = picker.query.clone();
                let targets = self
                    .picker_choices(Mode::Text)
                    .into_iter()
                    .enumerate()
                    .filter_map(|(order, choice)| {
                        let Page::Terminal(live) =
                            &self.tabs.get(self.tab_for_pane(choice.pane)?)?.page
                        else {
                            return None;
                        };
                        Some(Target {
                            pane: choice.pane,
                            order,
                            caption: choice.caption(),
                            input: live.session.input.clone(),
                        })
                    })
                    .collect();
                let (stop, cancelled) = oneshot::channel();
                let state = &mut self.picker.as_mut().unwrap().text;
                state.stop = Some(stop);
                state.task = Some(ctx.spawn_background_with_rejection(
                    move |_| {
                        Message::PaneText(match wait_for_query(scan(targets, &query), cancelled) {
                            Some(report) => Action::Ready(epoch, report),
                            None => Action::Unavailable(epoch),
                        })
                    },
                    Message::PaneText(Action::Unavailable(epoch)),
                ));
            }
            Action::Ready(_, report) => {
                picker.text.task.take();
                picker.text.stop.take();
                picker.text.set_report(report);
                self.announce_text_selection();
            }
            Action::Unavailable(_) => {
                picker.text.cancel();
                picker.text.status = "Search is unavailable. Refresh to try again.".into();
                self.announce_text_selection();
                self.focus_picker_input(0, ctx);
            }
            Action::Choose(_, pane) => {
                if picker.text.revealing {
                    return;
                }
                let Some(row) = picker.text.rows.iter().position(|r| r.pane == pane) else {
                    return;
                };
                picker.selected = row;
                let query = picker.query.clone();
                let anchor = picker.text.rows[row].anchor.clone();
                let Some((input, screen)) = self.search_target(pane).map(|(_, live)| {
                    (
                        live.session.input.clone(),
                        ScreenId::from(live.session.frames.load().as_ref()),
                    )
                }) else {
                    self.stale_text_result(pane);
                    self.focus_picker_input(0, ctx);
                    return;
                };
                let (reply, mut receiver) = oneshot::channel();
                let (stop, cancelled) = oneshot::channel();
                let task = ctx.spawn_background_with_rejection(
                    move |_| {
                        let accepted = wait_for_query(
                            async {
                                match tokio::time::timeout(PANE_TIMEOUT, &mut receiver).await {
                                    Ok(reply) => reply.unwrap_or(false),
                                    Err(_) => {
                                        // A simultaneous successful send has committed local Find.
                                        receiver.close();
                                        receiver.try_recv().unwrap_or(false)
                                    }
                                }
                            },
                            cancelled,
                        )
                        .unwrap_or(false);
                        Message::PaneText(Action::Revealed(epoch, pane, screen, accepted))
                    },
                    Message::PaneText(Action::Unavailable(epoch)),
                );
                if task.status() != ComponentTaskStatus::Rejected {
                    let _ = input.send(Command::SearchReveal {
                        query,
                        anchor,
                        reply,
                    });
                }
                let state = &mut self.picker.as_mut().unwrap().text;
                state.stop = Some(stop);
                state.revealing = true;
                state.status = "Opening match…".into();
                state.task = Some(task);
            }
            Action::Revealed(_, pane, screen, accepted) => {
                picker.text.revealing = false;
                picker.text.stop.take();
                picker.text.task.take();
                let query = picker.query.clone();
                let Some(id) = self
                    .search_target(pane)
                    .filter(|(_, live)| {
                        ScreenId::from(live.session.frames.load().as_ref()) == screen
                    })
                    .map(|(id, _)| id)
                    .filter(|_| accepted)
                else {
                    self.stale_text_result(pane);
                    self.focus_picker_input(0, ctx);
                    return;
                };
                let Some(AppTab {
                    page: Page::Terminal(live),
                    ..
                }) = self.tabs.get_mut(id)
                else {
                    return;
                };
                live.find.epoch += 1;
                live.find.open = true;
                live.find.query = query;
                live.find.sensitive = false;
                live.find.regex = false;
                live.find.pending = false;
                self.tabs.select(id);
                self.sync_selection(ctx);
                self.close_picker(ctx);
            }
        }
    }

    fn search_target(&self, pane: u64) -> Option<(TabId, &LiveTab)> {
        let id = self.tab_for_pane(pane)?;
        let Page::Terminal(live) = &self.tabs.get(id)?.page else {
            return None;
        };
        let transferring = self
            .outgoing
            .as_ref()
            .is_some_and(|t| self.tabs.group_id(id) == Some(t.group));
        (!transferring
            && !live.device_busy
            && !live.browsing_sessions()
            && live.session.prompts.current().is_none())
        .then_some((id, live))
    }

    fn stale_text_result(&mut self, pane: u64) {
        let picker = self.picker.as_mut().unwrap();
        picker.text.rows.retain(|row| row.pane != pane);
        picker.move_by(0);
        picker.text.status =
            "The match changed or the pane is unavailable. Refresh to search again.".into();
        self.announce_text_selection();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn stalled_panes_release_their_requests_and_allow_the_remaining_pane_to_report() {
        let (input, mut commands) = mpsc::unbounded_channel();
        let targets = (0..=PARALLEL_PANES)
            .map(|order| Target {
                pane: order as u64,
                order,
                caption: String::new(),
                input: input.clone(),
            })
            .collect();
        let scanning = tokio::spawn(async move { scan(targets, "needle").await });
        let mut stalled = vec![];
        for _ in 0..PARALLEL_PANES {
            let Some(Command::SearchPreview { reply, .. }) = commands.recv().await else {
                panic!("expected a scrollback preview");
            };
            stalled.push(reply);
        }
        assert!(commands.try_recv().is_err());
        tokio::time::advance(PANE_TIMEOUT).await;
        let Some(Command::SearchPreview { reply, .. }) = commands.recv().await else {
            panic!("the next pane should be searched after a worker times out");
        };
        assert!(stalled.iter().all(oneshot::Sender::is_closed));
        reply
            .send(PaneSearchPreview {
                updating: true,
                ..Default::default()
            })
            .unwrap();
        let report = scanning.await.unwrap();
        assert_eq!(report.closed, PARALLEL_PANES);
        assert_eq!(report.updating, 1);
    }
}
