use super::*;
use trace::{Comparison, ComparisonKind, ComparisonMatch, ComparisonRow};

mod report;

const ROWS: usize = 40;
const COMPARISON_LIMIT: &str =
    "Comparison reached its descriptor or change limit. Some evidence may be missing.";

pub enum Action {
    Open(TabId),
    Back(TabId),
    Ready(TabId, Arc<()>, Result<Option<Arc<Comparison>>, String>),
    Select(TabId, Arc<Comparison>, usize),
    Page(TabId, bool),
    Evidence(TabId, bool),
    Timing(TabId, bool),
    Query(TabId, String),
    Filter(TabId),
    Filtered(TabId, Arc<()>, Result<AppliedFilter, String>),
    Export(TabId),
}

pub struct AppliedFilter {
    query: String,
    matches: Vec<ComparisonMatch>,
}

#[derive(Default)]
pub(super) struct ViewState {
    pub(super) open: bool,
    compare: Option<Arc<Comparison>>,
    timing: bool,
    query: String,
    applied: Option<AppliedFilter>,
    visible: Vec<usize>,
    page: usize,
    selected: Option<usize>,
    sample: usize,
}

fn message(action: Action) -> Message {
    send(super::Action::Compare(action))
}

enum Input {
    Picker {
        working: Arc<Trace>,
        owner: usize,
    },
    Saved {
        before: Vec<PathBuf>,
        after: Vec<PathBuf>,
    },
}

impl TerminalApp {
    pub(in crate::app) fn compare_recent_syscall_traces(
        &mut self,
        before: Vec<PathBuf>,
        after: Vec<PathBuf>,
        ctx: &ComponentContext<Self>,
    ) {
        let tab = self.new_saved_syscall_viewer(ctx);
        self.read_trace_comparison(tab, Input::Saved { before, after }, ctx);
    }

    pub(super) fn syscall_compare_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Open(tab) => self.open_trace_comparison(tab, ctx),
            Action::Ready(tab, ticket, result) => {
                if let Some(page) = self.syscall_page(tab)
                    && page.finish(&ticket)
                {
                    match result {
                        Ok(Some(comparison)) => {
                            if page.loaded.is_none() {
                                page.load(comparison.traces[0].clone());
                            }
                            let paths = comparison.traces[1].paths();
                            let state = &mut page.comparison;
                            *state = ViewState {
                                open: true,
                                compare: Some(comparison),
                                ..Default::default()
                            };
                            state.filter(false);
                            self.remember_diagnostic(
                                w11term::diagnostic_history::Kind::SyscallTrace,
                                paths,
                                ctx,
                            );
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
            Action::Select(tab, comparison, index) => {
                if let Some(state) = self.comparison_input(tab)
                    && state
                        .compare
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &comparison))
                    && state.page_rows().contains(&index)
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
                    state.filter(false);
                }
            }
            Action::Query(tab, text) => {
                if let Some(state) = self.comparison_input(tab) {
                    state.query = text.chars().take(256).collect();
                }
            }
            Action::Filter(tab) => self.filter_trace_comparison(tab, ctx),
            Action::Filtered(tab, ticket, result) => {
                if let Some(page) = self.syscall_page(tab)
                    && page.finish(&ticket)
                {
                    match result {
                        Ok(filter) => {
                            page.comparison.applied = Some(filter);
                            page.comparison.filter(true);
                        }
                        Err(error) => page.status = Some(error),
                    }
                }
            }
            Action::Export(tab) => {
                if self.tabs.active_id() == Some(tab)
                    && self.export.pending.is_none()
                    && let Some(snapshot) = self
                        .comparison_input(tab)
                        .and_then(|state| report::Snapshot::capture(state))
                {
                    self.begin_text_export(
                        text_export::Subject::SyscallComparison,
                        move |file| snapshot.write(file),
                        ctx,
                    );
                }
            }
        }
    }

    fn comparison_input(&mut self, tab: TabId) -> Option<&mut ViewState> {
        let page = self.syscall_page(tab)?;
        (page.comparison.open && page.pending.is_none()).then_some(&mut page.comparison)
    }

    fn filter_trace_comparison(&mut self, tab: TabId, ctx: &ComponentContext<Self>) {
        if self.tabs.active_id() != Some(tab) {
            return;
        }
        let Some(page) = self
            .syscall_page(tab)
            .filter(|page| page.comparison.open && page.pending.is_none())
        else {
            return;
        };
        let state = &mut page.comparison;
        let query = state.query.trim();
        if query.is_empty() {
            state.applied = None;
            state.filter(false);
            page.status = None;
            return;
        }
        let Some(compare) = state.compare.clone() else {
            return;
        };
        let query = query.to_owned();
        let ticket = page.begin("Filtering syscall changes…");
        let rejected = message(Action::Filtered(
            tab,
            ticket.clone(),
            Err("Syscall comparison filter worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |_| {
                let matches = compare.search(&query);
                message(Action::Filtered(
                    tab,
                    ticket,
                    Ok(AppliedFilter { query, matches }),
                ))
            },
            rejected,
        );
    }

    fn open_trace_comparison(&mut self, tab: TabId, ctx: &ComponentContext<Self>) {
        if self.tabs.active_id() != Some(tab) {
            return;
        }
        let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
            return;
        };
        let Some(working) = self.syscall_page(tab).and_then(|page| page.loaded.clone()) else {
            return;
        };
        self.read_trace_comparison(tab, Input::Picker { working, owner }, ctx);
    }

    fn read_trace_comparison(&mut self, tab: TabId, input: Input, ctx: &ComponentContext<Self>) {
        let Some(page) = self.syscall_page(tab).filter(|page| page.pending.is_none()) else {
            return;
        };
        let status = match &input {
            Input::Picker { .. } => "Opening the failing trace and comparing syscall sequences…",
            Input::Saved { .. } => "Comparing saved syscall traces…",
        };
        let ticket = page.begin(status);
        let rejected = message(Action::Ready(
            tab,
            ticket.clone(),
            Err("Trace comparison worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| -> anyhow::Result<_> {
                    if scope.is_cancelled() {
                        return Ok(None);
                    }
                    let (working, files) = match input {
                        Input::Picker { working, owner } => {
                            let initial = working.first_path().to_owned();
                            let files = file_picker::pick_paths(
                                owner,
                                file_picker::Purpose::OpenSyscallTrace(initial),
                            )?;
                            (working, files)
                        }
                        Input::Saved { before, after } => (
                            Arc::new(Trace::open(before).context("Opening the baseline trace")?),
                            after,
                        ),
                    };
                    if files.is_empty() || scope.is_cancelled() {
                        return Ok(None);
                    }
                    let failing =
                        Arc::new(Trace::open(files).context("Opening the comparison trace")?);
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
    fn filter(&mut self, prefer_match: bool) {
        let Some(compare) = &self.compare else { return };
        let show =
            |&index: &usize| self.timing || compare.rows[index].kind != ComparisonKind::Timing;
        self.visible = if let Some(filter) = &self.applied {
            filter
                .matches
                .iter()
                .map(|matched| matched.index)
                .filter(show)
                .collect()
        } else {
            (0..compare.rows.len()).filter(show).collect()
        };
        let selected = self
            .selected
            .filter(|id| self.visible.contains(id))
            .or_else(|| {
                compare
                    .first_divergence()
                    .filter(|id| self.visible.contains(id))
            })
            .or_else(|| self.visible.first().copied());
        self.page = selected
            .and_then(|id| self.visible.iter().position(|&item| item == id))
            .unwrap_or_default()
            / ROWS;
        if self.selected != selected {
            self.select(selected);
        } else if prefer_match
            && let Some(sample) = selected.and_then(|index| self.match_sample(index))
        {
            self.sample = sample;
        }
    }

    fn match_sample(&self, index: usize) -> Option<usize> {
        let matches = &self.applied.as_ref()?.matches;
        let position = matches
            .binary_search_by_key(&index, |matched| matched.index)
            .ok()?;
        matches[position].sample
    }

    fn select(&mut self, index: Option<usize>) {
        self.selected = index;
        self.sample = index
            .and_then(|index| self.match_sample(index))
            .unwrap_or_else(|| self.current().map_or(0, |(_, row)| row.sample));
    }

    fn page_rows(&self) -> &[usize] {
        let first = self.page * ROWS;
        &self.visible[first..(first + ROWS).min(self.visible.len())]
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
        header.push(KeyedView::new(
            "compare-filter",
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    TextBox::new()
                        .automation_name("Filter syscall changes")
                        .placeholder_text("Syscall, path, executable, or error")
                        .is_enabled(idle)
                        .text(&self.query)
                        .on_text_changed(
                            ctx.callback(move |text| message(Action::Query(tab, text))),
                        ),
                    Button::new()
                        .grid_column(1)
                        .is_enabled(
                            idle && (self.applied.is_some() || !self.query.trim().is_empty()),
                        )
                        .on_click(ctx.callback(move |_| message(Action::Filter(tab))))
                        .content("Apply change filter"),
                    Button::new()
                        .grid_column(2)
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| message(Action::Export(tab))))
                        .content("Export comparison…"),
                )),
        ));
        if comparison.limited {
            header.push(KeyedView::new(
                "comparison-limit",
                TextBlock::new()
                    .text(COMPARISON_LIMIT)
                    .text_wrapping(TextWrapping::Wrap),
            ));
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
        compare: &Arc<Comparison>,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let first = self.page * ROWS;
        let window = self.page_rows();
        let row_ids: Arc<[usize]> = window.into();
        let comparison = compare.clone();
        let selected = window
            .iter()
            .position(|&index| Some(index) == self.selected);
        let first_divergence = compare.first_divergence();
        let count = self.visible.len();
        let prefix = if self.applied.is_some() {
            "matching "
        } else {
            ""
        };
        let label = if count == 1 {
            "change row"
        } else {
            "change rows"
        };
        let summary = format!(
            "{count} {prefix}{label} · Page {} of {}",
            self.page + 1,
            count.div_ceil(ROWS).max(1)
        );
        let list = ListView::new()
            .grid_row(1)
            .automation_name("Trace differences")
            .selected_index(selected)
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index
                    .and_then(|offset| row_ids.get(offset))
                    .map_or(Message::ControlIdle, |&id| {
                        message(Action::Select(tab, comparison.clone(), id))
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
            } else if self.applied.is_some() {
                "No changes match the filter."
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
        let evidence = row.map_or_else(String::new, |row| {
            evidence_text(compare, row, side, self.sample)
        });
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

fn evidence_text(compare: &Comparison, row: &ComparisonRow, side: usize, sample: usize) -> String {
    let Some(event) = compare.event(row, side, sample) else {
        return "No corresponding call in this run.".into();
    };
    let trace = &compare.traces[side];
    let mut evidence = String::new();
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
    evidence
}

fn delta(before: u64, after: u64) -> f64 {
    (i128::from(after) - i128::from(before)) as f64 / 1_000_000.0
}
