use super::*;
use trace::{Comparison, ComparisonKind, ComparisonRow};

const ROWS: usize = 40;

pub enum Action {
    Open(TabId),
    Back(TabId),
    Ready(TabId, Arc<()>, Result<Option<Arc<Comparison>>, String>),
    Select(TabId, usize),
    Page(TabId, bool),
    Evidence(TabId, bool),
    Timing(TabId, bool),
}

#[derive(Default)]
pub(super) struct ViewState {
    pub(super) open: bool,
    compare: Option<Arc<Comparison>>,
    timing: bool,
    visible: Vec<usize>,
    page: usize,
    selected: Option<usize>,
    sample: usize,
}

fn message(action: Action) -> Message {
    send(super::Action::Compare(action))
}

impl TerminalApp {
    pub(super) fn syscall_compare_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Open(tab) => self.open_trace_comparison(tab, ctx),
            Action::Ready(tab, ticket, result) => {
                if let Some(page) = self.syscall_page(tab)
                    && page.finish(&ticket)
                {
                    match result {
                        Ok(Some(comparison)) => {
                            let state = &mut page.comparison;
                            *state = ViewState {
                                open: true,
                                compare: Some(comparison),
                                ..Default::default()
                            };
                            state.filter();
                        }
                        Ok(None) => (),
                        Err(error) => page.status = Some(error),
                    }
                }
            }
            Action::Back(tab) => {
                if let Some(state) = self.comparison_input(tab) {
                    state.open = false;
                }
            }
            Action::Select(tab, offset) => {
                if let Some(state) = self.comparison_input(tab)
                    && offset < ROWS
                    && let Some(&index) = state.visible.get(state.page * ROWS + offset)
                    && state.selected != Some(index)
                {
                    state.select(Some(index));
                }
            }
            Action::Page(tab, next) => {
                if let Some(state) = self.comparison_input(tab) {
                    let page = step(state.page, state.visible.len().div_ceil(ROWS), next);
                    if page != state.page {
                        state.page = page;
                        state.select(state.visible.get(page * ROWS).copied());
                    }
                }
            }
            Action::Evidence(tab, next) => {
                if let Some(state) = self.comparison_input(tab)
                    && let Some((_, row)) = state.current()
                {
                    state.sample = step(state.sample, row.evidence_count(), next);
                }
            }
            Action::Timing(tab, enabled) => {
                if let Some(state) = self.comparison_input(tab)
                    && state.timing != enabled
                {
                    state.timing = enabled;
                    state.filter();
                }
            }
        }
    }

    fn comparison_input(&mut self, tab: TabId) -> Option<&mut ViewState> {
        let page = self.syscall_page(tab)?;
        (page.comparison.open && page.pending.is_none()).then_some(&mut page.comparison)
    }

    fn open_trace_comparison(&mut self, tab: TabId, ctx: &ComponentContext<Self>) {
        if self.tabs.active_id() != Some(tab) {
            return;
        }
        let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
            return;
        };
        let Some(page) = self.syscall_page(tab).filter(|page| page.pending.is_none()) else {
            return;
        };
        let Some(working) = page.loaded.clone() else {
            return;
        };
        let initial = working.first_path().to_owned();
        let ticket = page.begin("Opening the failing trace and comparing syscall sequences…");
        let rejected = message(Action::Ready(
            tab,
            ticket.clone(),
            Err("Trace comparison worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| -> anyhow::Result<_> {
                    let files = file_picker::pick_paths(
                        owner,
                        file_picker::Purpose::OpenSyscallTrace(initial),
                    )?;
                    if files.is_empty() || scope.is_cancelled() {
                        return Ok(None);
                    }
                    let failing = Arc::new(Trace::open(files)?);
                    Ok(Some(Arc::new(Comparison::build(working, failing))))
                })()
                .map_err(|error| format!("{error:#}"));
                message(Action::Ready(tab, ticket, result))
            },
            rejected,
        );
    }
}

impl ViewState {
    fn filter(&mut self) {
        let Some(compare) = &self.compare else { return };
        self.visible = compare
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                (self.timing || row.kind != ComparisonKind::Timing).then_some(index)
            })
            .collect();
        let selected = self
            .selected
            .filter(|id| self.visible.contains(id))
            .or_else(|| compare.first_divergence())
            .or_else(|| self.visible.first().copied());
        self.page = selected
            .and_then(|id| self.visible.iter().position(|&item| item == id))
            .unwrap_or_default()
            / ROWS;
        self.select(selected);
    }

    fn select(&mut self, index: Option<usize>) {
        self.selected = index;
        self.sample = self.current().map_or(0, |(_, row)| row.sample);
    }

    fn current(&self) -> Option<(&Comparison, &ComparisonRow)> {
        let compare = self.compare.as_deref()?;
        Some((compare, compare.rows.get(self.selected?)?))
    }

    pub(super) fn launch(
        &self,
        tab: TabId,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> KeyedView {
        KeyedView::new(
            "compare-traces",
            Button::new()
                .is_enabled(idle)
                .on_click(ctx.callback(move |_| message(Action::Open(tab))))
                .content("Compare with failing trace…"),
        )
    }

    pub(super) fn view(
        &self,
        tab: TabId,
        mut header: Vec<KeyedView>,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let Some(comparison) = &self.compare else {
            return StackPanel::new()
                .spacing(8.0)
                .margin(12.0)
                .keyed_children(header);
        };
        header.push(KeyedView::new(
            "compare-actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| message(Action::Back(tab))))
                        .content("Back to working trace"),
                    Button::new()
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| message(Action::Open(tab))))
                        .content("Choose another failing trace…"),
                    CheckBox::new()
                        .is_enabled(idle)
                        .is_checked(self.timing)
                        .on_is_checked_changed(
                            ctx.callback(move |checked| message(Action::Timing(tab, checked))),
                        )
                        .content("Include timing-only changes"),
                )),
        ));
        if comparison.limited {
            header.push(KeyedView::new("comparison-limit", TextBlock::new()
                .text("Comparison reached its descriptor or change limit. Some evidence may be missing.")
                .text_wrapping(TextWrapping::Wrap)));
        }
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(12.0)
            .margin(12.0)
            .children((
                StackPanel::new().spacing(8.0).keyed_children(header),
                Grid::new()
                    .grid_row(1)
                    .columns([GridLength::Star(0.85), GridLength::Star(1.65)])
                    .column_spacing(16.0)
                    .children((
                        self.differences(tab, comparison, idle, ctx),
                        Border::new()
                            .grid_column(1)
                            .content(self.evidence(tab, comparison, idle, ctx)),
                    )),
            ))
    }

    fn differences(
        &self,
        tab: TabId,
        compare: &Comparison,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let first = self.page * ROWS;
        let window = &self.visible[first..(first + ROWS).min(self.visible.len())];
        let selected = window
            .iter()
            .position(|&index| Some(index) == self.selected);
        let first_divergence = compare.first_divergence();
        let summary = format!(
            "{} change rows · Page {} of {}",
            self.visible.len(),
            self.page + 1,
            self.visible.len().div_ceil(ROWS).max(1)
        );
        let list = ListView::new()
            .grid_row(1)
            .automation_name("Trace differences")
            .selected_index(selected)
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index.map_or(Message::ControlIdle, |index| {
                    message(Action::Select(tab, index))
                })
            }))
            .collection_slot(
                ListViewSlot::Items,
                window.iter().map(|&index| {
                    let row = &compare.rows[index];
                    let resource = row.resource.chars().take(126).collect::<String>();
                    let lead = if Some(index) == first_divergence {
                        "First divergence · "
                    } else {
                        ""
                    };
                    let uncertain = if row.uncertain && row.kind != ComparisonKind::Unaligned {
                        " · Match uncertain"
                    } else {
                        ""
                    };
                    let label = format!(
                        "{lead}{} · {} · {}{uncertain}",
                        row.kind.label(),
                        row.call,
                        safe_text(&resource)
                    );
                    KeyedView::new(
                        index.to_string(),
                        ListViewItem::new().automation_name(&label).content(
                            TextBlock::new()
                                .text(label)
                                .text_wrapping(TextWrapping::Wrap),
                        ),
                    )
                }),
            );
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(6.0)
            .children((
                StackPanel::new().spacing(6.0).children((
                    TextBlock::new()
                        .text(summary)
                        .text_wrapping(TextWrapping::Wrap),
                    navigation(
                        "difference page",
                        idle && self.page > 0,
                        idle && first + ROWS < self.visible.len(),
                        ctx.callback(move |_| message(Action::Page(tab, false))),
                        ctx.callback(move |_| message(Action::Page(tab, true))),
                    ),
                )),
                list,
            ))
    }

    fn evidence(
        &self,
        tab: TabId,
        compare: &Comparison,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let current = self.current().map(|(_, row)| row);
        let mut heading = Vec::new();
        if let Some(row) = current {
            let [left, right] = row.counts();
            heading.push(KeyedView::new(
                "summary",
                TextBlock::new()
                    .font_size(16.0)
                    .text(format!(
                        "{} · {}",
                        row.kind.label(),
                        safe_text(&row.process)
                    ))
                    .text_wrapping(TextWrapping::Wrap),
            ));
            heading.push(KeyedView::new(
                "counts",
                TextBlock::new().text(format!(
                    "Working {left} · Failing {right} · Count delta {:+}",
                    right as i64 - left as i64
                )),
            ));
            if row.result_changes != 0 {
                let [before, after] = row
                    .result
                    .each_ref()
                    .map(|value| value.as_deref().unwrap_or("Unavailable"));
                heading.push(KeyedView::new(
                    "results",
                    TextBlock::new()
                        .text(format!(
                            "Result {} → {} · {} changed",
                            safe_text(before),
                            safe_text(after),
                            row.result_changes
                        ))
                        .text_wrapping(TextWrapping::Wrap),
                ));
            }
            if let [Some(before), Some(after)] = row.durations {
                heading.push(KeyedView::new(
                    "duration",
                    TextBlock::new().text(format!(
                        "Total call duration delta {:+.3} ms",
                        delta(before, after)
                    )),
                ));
            }
            if let [Some(before), Some(after)] = [0, 1].map(|side| {
                compare
                    .event(row, side, self.sample)
                    .and_then(|event| compare.traces[side].events[event].duration)
            }) {
                heading.push(KeyedView::new(
                    "sample-duration",
                    TextBlock::new().text(format!(
                        "Selected call duration delta {:+.3} ms",
                        delta(before, after)
                    )),
                ));
            }
            heading.push(KeyedView::new(
                "evidence-position",
                TextBlock::new().text(format!(
                    "Evidence {} of {}",
                    self.sample + 1,
                    row.evidence_count()
                )),
            ));
            heading.push(KeyedView::new(
                "evidence-pages",
                navigation(
                    "comparison evidence",
                    idle && self.sample > 0,
                    idle && self.sample + 1 < row.evidence_count(),
                    ctx.callback(move |_| message(Action::Evidence(tab, false))),
                    ctx.callback(move |_| message(Action::Evidence(tab, true))),
                ),
            ));
        } else {
            let text = if compare.rows.is_empty() {
                "No differences found in the available records."
            } else {
                "Timing-only changes are available."
            };
            heading.push(KeyedView::new(
                "equal",
                TextBlock::new()
                    .text(text)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(6.0)
            .children((
                StackPanel::new().spacing(4.0).keyed_children(heading),
                Grid::new()
                    .grid_row(1)
                    .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
                    .column_spacing(12.0)
                    .children((
                        self.evidence_column(compare, current, 0),
                        Border::new()
                            .grid_column(1)
                            .content(self.evidence_column(compare, current, 1)),
                    )),
            ))
    }

    fn evidence_column(
        &self,
        compare: &Comparison,
        row: Option<&ComparisonRow>,
        side: usize,
    ) -> View {
        let trace = &compare.traces[side];
        let first = trace.first_path();
        let name = first
            .file_name()
            .unwrap_or(first.as_os_str())
            .to_string_lossy();
        let label = if side == 0 { "Working" } else { "Failing" };
        let automation_id = if side == 0 {
            "comparison-working-original"
        } else {
            "comparison-failing-original"
        };
        let mut evidence = String::new();
        if let Some(event) = row.and_then(|row| compare.event(row, side, self.sample)) {
            if let Some(elapsed) = trace.relative_start(event) {
                let _ = writeln!(
                    evidence,
                    "+{:.3} ms from run start",
                    elapsed as f64 / 1_000_000.0
                );
            }
            if let Some(duration) = trace.events[event].duration {
                let _ = writeln!(
                    evidence,
                    "Call duration {:.3} ms",
                    duration as f64 / 1_000_000.0
                );
            }
            if let Some(pid) = trace.events[event].pid {
                let _ = writeln!(evidence, "PID {pid}");
            }
            for raw in trace.events[event].originals() {
                append_raw(&mut evidence, trace, raw);
            }
        } else if row.is_some() {
            evidence.push_str("No corresponding call in this run.");
        }
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(6.0)
            .children((
                TextBlock::new()
                    .font_size(16.0)
                    .text(format!("{label} · {}", safe_text(&name)))
                    .text_wrapping(TextWrapping::Wrap),
                ScrollViewer::new().grid_row(1).content(
                    TextBlock::new()
                        .automation_id(automation_id)
                        .is_text_selection_enabled(true)
                        .text_wrapping(TextWrapping::Wrap)
                        .text(evidence),
                ),
            ))
    }
}

fn delta(before: u64, after: u64) -> f64 {
    (i128::from(after) - i128::from(before)) as f64 / 1_000_000.0
}
