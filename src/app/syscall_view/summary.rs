use super::*;
use trace::{CallStats, Summary, SummarySort};

const ROWS: usize = 48;

pub enum Action {
    Switch(TabId),
    Pid(TabId, String),
    Apply(TabId),
    Sort(TabId, usize),
    Page(TabId, bool),
    Select(TabId, usize),
    Ready(TabId, Arc<()>, Result<Ranked, &'static str>),
}

pub struct Ranked {
    summary: Arc<Summary>,
    order: Vec<usize>,
    sort: SummarySort,
}

#[derive(Default)]
pub(super) struct ViewState {
    pub(super) open: bool,
    pid: String,
    ranked: Option<Ranked>,
    page: usize,
    selected: usize,
}

fn message(action: Action) -> Message {
    send(super::Action::Summary(action))
}

impl TerminalApp {
    pub(super) fn syscall_summary_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Switch(tab) => {
                if let Some(page) = self.syscall_page(tab).filter(|page| page.pending.is_none()) {
                    page.timing.open = !page.timing.open;
                    if page.timing.open && page.timing.ranked.is_none() {
                        self.rank_syscalls(tab, None, SummarySort::Total, ctx);
                    }
                }
            }
            Action::Pid(tab, value) => {
                if let Some(page) = self.summary_input(tab) {
                    page.timing.pid = value.chars().take(32).collect();
                }
            }
            Action::Apply(tab) => {
                let Some(page) = self.summary_input(tab) else {
                    return;
                };
                let input = page.timing.pid.trim();
                let pid = if input.is_empty() {
                    None
                } else if let Some(pid) = input.parse::<u32>().ok().filter(|&pid| pid > 0) {
                    Some(pid)
                } else {
                    page.status =
                        Some("Enter a positive process ID or leave the field empty.".into());
                    return;
                };
                let sort = page
                    .timing
                    .ranked
                    .as_ref()
                    .map_or(SummarySort::Total, |ranked| ranked.sort);
                self.rank_syscalls(tab, pid, sort, ctx);
            }
            Action::Sort(tab, index) => {
                if let Some((_, _, sort)) = SummarySort::CHOICES.get(index)
                    && let Some(page) = self.summary_input(tab)
                    && let Some(ranked) = &page.timing.ranked
                    && ranked.sort != *sort
                {
                    let pid = ranked.summary.pid;
                    self.rank_syscalls(tab, pid, *sort, ctx);
                }
            }
            Action::Ready(tab, ticket, result) => {
                if let Some(page) = self.syscall_page(tab)
                    && page.finish(&ticket)
                {
                    match result {
                        Ok(ranked) => {
                            page.timing.ranked = Some(ranked);
                            page.timing.page = 0;
                            page.timing.selected = 0;
                        }
                        Err(error) => page.status = Some(error.into()),
                    }
                }
            }
            Action::Page(tab, next) => {
                if let Some(page) = self.summary_input(tab)
                    && let Some(ranked) = &page.timing.ranked
                {
                    let target = step(page.timing.page, ranked.order.len().div_ceil(ROWS), next);
                    if target != page.timing.page {
                        page.timing.page = target;
                        page.timing.selected = 0;
                    }
                }
            }
            Action::Select(tab, index) => {
                if let Some(page) = self.summary_input(tab)
                    && let Some(ranked) = &page.timing.ranked
                    && index < ROWS
                    && page.timing.page * ROWS + index < ranked.order.len()
                {
                    page.timing.selected = index;
                }
            }
        }
    }

    fn summary_input(&mut self, tab: TabId) -> Option<&mut PageState> {
        self.syscall_page(tab)
            .filter(|page| page.timing.open && page.pending.is_none())
    }

    fn rank_syscalls(
        &mut self,
        tab: TabId,
        pid: Option<u32>,
        sort: SummarySort,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(page) = self.summary_input(tab) else {
            return;
        };
        let Some(trace) = page.loaded.clone() else {
            return;
        };
        let cached = page
            .timing
            .ranked
            .as_ref()
            .filter(|ranked| ranked.summary.pid == pid)
            .map(|ranked| ranked.summary.clone());
        let ticket = page.begin("Summarizing syscall timings…");
        let rejected = message(Action::Ready(
            tab,
            ticket.clone(),
            Err("Syscall timing worker unavailable"),
        ));
        ctx.spawn_background_with_rejection(
            move |_| {
                let summary = cached.unwrap_or_else(|| Arc::new(trace.summarize(pid)));
                let order = summary.order(sort);
                message(Action::Ready(
                    tab,
                    ticket,
                    Ok(Ranked {
                        summary,
                        order,
                        sort,
                    }),
                ))
            },
            rejected,
        );
    }
}

impl ViewState {
    pub(super) fn switch(
        &self,
        tab: TabId,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> KeyedView {
        KeyedView::new(
            "timing-mode",
            Button::new()
                .is_enabled(idle)
                .on_click(ctx.callback(move |_| message(Action::Switch(tab))))
                .content(if self.open {
                    "Back to syscall groups"
                } else {
                    "Syscall timing summary"
                }),
        )
    }

    pub(super) fn view(
        &self,
        tab: TabId,
        trace: &Trace,
        mut header: Vec<KeyedView>,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let sort = self
            .ranked
            .as_ref()
            .map_or(SummarySort::Total, |ranked| ranked.sort);
        let sort_index = SummarySort::CHOICES
            .iter()
            .position(|(_, _, choice)| *choice == sort)
            .unwrap_or_default();
        header.push(KeyedView::new(
            "timing-filters",
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    TextBox::new()
                        .automation_name("Summary PID")
                        .placeholder_text("All processes")
                        .is_enabled(idle)
                        .text(&self.pid)
                        .on_text_changed(
                            ctx.callback(move |value| message(Action::Pid(tab, value))),
                        ),
                    Button::new()
                        .grid_column(1)
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| message(Action::Apply(tab))))
                        .content("Apply summary PID"),
                    ComboBox::new()
                        .grid_column(2)
                        .automation_name("Syscall summary sort")
                        .items_source(SummarySort::CHOICES.map(|(label, _, _)| label))
                        .selected_index(sort_index)
                        .is_enabled(idle && self.ranked.is_some())
                        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                            index.map_or(Message::ControlIdle, |index| {
                                message(Action::Sort(tab, index))
                            })
                        })),
                )),
        ));
        let Some(ranked) = &self.ranked else {
            return StackPanel::new()
                .spacing(8.0)
                .margin(12.0)
                .keyed_children(header);
        };
        let summary = &ranked.summary;
        let scope = summary
            .pid
            .map_or_else(|| "All processes".into(), |pid| format!("PID {pid}"));
        let time = if summary.timed_calls == 0 {
            "No durations recorded. Capture with strace -T to include timings.".into()
        } else {
            format!(
                "{} ms total recorded time. Call durations may overlap.",
                millis(summary.total_ns)
            )
        };
        header.push(KeyedView::new(
            "timing-counts",
            TextBlock::new()
                .text(format!(
                    "{scope} · {} calls · {} timed · {} errors · {} incomplete · {} other events",
                    summary.calls,
                    summary.timed_calls,
                    summary.errors,
                    summary.incomplete_calls,
                    summary.other_events
                ))
                .text_wrapping(TextWrapping::Wrap),
        ));
        header.push(KeyedView::new(
            "timing-coverage",
            TextBlock::new()
                .text(time)
                .text_wrapping(TextWrapping::Wrap),
        ));
        let start = self.page * ROWS;
        let rows = &ranked.order[start..(start + ROWS).min(ranked.order.len())];
        let selected = rows.get(self.selected).map(|&index| &summary.rows[index]);
        let pages = ranked.order.len().div_ceil(ROWS).max(1);
        let list = ListView::new()
            .grid_row(1)
            .automation_name("Syscall timing rows")
            .selected_index(selected.map(|_| self.selected))
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index.map_or(Message::ControlIdle, |index| {
                    message(Action::Select(tab, index))
                })
            }))
            .collection_slot(
                ListViewSlot::Items,
                rows.iter().map(|&index| {
                    let row = &summary.rows[index];
                    let timing = row.timing.as_ref().map_or_else(
                        || "timing unavailable".into(),
                        |time| {
                            format!(
                                "{} timed · total {} ms · max {} ms",
                                time.calls,
                                millis(time.total_ns),
                                millis(u128::from(time.max_ns))
                            )
                        },
                    );
                    let label = format!(
                        "{} · {} calls · {} errors · {} incomplete · {timing}",
                        row.syscall, row.calls, row.errors, row.incomplete_calls
                    );
                    KeyedView::new(
                        row.syscall.to_string(),
                        ListViewItem::new().automation_name(&label).content(
                            TextBlock::new()
                                .text(label)
                                .text_wrapping(TextWrapping::Wrap),
                        ),
                    )
                }),
            );
        let contents = Grid::new()
            .grid_row(1)
            .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
            .column_spacing(16.0)
            .children((
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Star(1.0)])
                    .row_spacing(6.0)
                    .children((
                        StackPanel::new().spacing(6.0).children((
                            TextBlock::new().text(format!(
                                "{} syscalls · Page {} of {pages}",
                                ranked.order.len(),
                                self.page + 1
                            )),
                            navigation(
                                "timing page",
                                idle && self.page > 0,
                                idle && self.page + 1 < pages,
                                ctx.callback(move |_| message(Action::Page(tab, false))),
                                ctx.callback(move |_| message(Action::Page(tab, true))),
                            ),
                        )),
                        list,
                    )),
                Border::new()
                    .grid_column(1)
                    .content(slowest(trace, selected)),
            ));
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .margin(12.0)
            .row_spacing(12.0)
            .children((
                StackPanel::new().spacing(8.0).keyed_children(header),
                contents,
            ))
    }
}

fn slowest(trace: &Trace, row: Option<&CallStats>) -> View {
    let Some(row) = row else {
        return TextBlock::new()
            .text("No syscalls match this scope.")
            .into();
    };
    let Some(timing) = &row.timing else {
        return TextBlock::new()
            .text(format!("No sampled durations for {}.", row.syscall))
            .into();
    };
    let event = &trace.events()[timing.slowest_event];
    let actor = event
        .pid
        .map_or_else(|| "PID unknown".into(), |pid| format!("PID {pid}"));
    let mut original = String::new();
    for raw in event.originals() {
        append_raw(&mut original, trace, raw);
    }
    Grid::new()
        .rows([GridLength::Auto, GridLength::Star(1.0)])
        .row_spacing(6.0)
        .children((
            TextBlock::new()
                .text(format!(
                    "Slowest recorded call · {} · {actor} · {} ms",
                    row.syscall,
                    millis(u128::from(timing.max_ns))
                ))
                .text_wrapping(TextWrapping::Wrap),
            ScrollViewer::new().grid_row(1).content(
                TextBlock::new()
                    .automation_id("syscall-summary-original-text")
                    .is_text_selection_enabled(true)
                    .text_wrapping(TextWrapping::Wrap)
                    .text(original),
            ),
        ))
}

fn millis(ns: u128) -> String {
    let whole = ns / 1_000_000;
    let fraction = ns % 1_000_000;
    if fraction.is_multiple_of(1000) {
        format!("{whole}.{:03}", fraction / 1000)
    } else {
        format!("{whole}.{fraction:06}")
    }
}
