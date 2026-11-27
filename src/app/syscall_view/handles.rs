use super::*;

const ROWS: usize = 48;
const EVIDENCE_ROWS: usize = 12;

pub enum Action {
    Switch(TabId),
    Ready(TabId, Arc<()>, Arc<trace::DescriptorReport>),
    Rejected(TabId, Arc<()>),
    Query(TabId, String),
    Search(TabId),
    Related(TabId),
    Results(TabId, Arc<()>, Vec<usize>, Option<usize>),
    Select(TabId, usize),
    Page(TabId, bool),
    Evidence(TabId, bool),
}

#[derive(Default)]
pub(super) struct ViewState {
    pub(super) open: bool,
    report: Option<Arc<trace::DescriptorReport>>,
    visible: Vec<usize>,
    selection: Option<usize>,
    page: usize,
    evidence: usize,
    query: String,
}

fn message(action: Action) -> Message {
    send(super::Action::Handles(action))
}

impl TerminalApp {
    pub(super) fn syscall_handles_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        match action {
            Action::Switch(tab) => {
                let Some(page) = self.syscall_page(tab).filter(|page| page.pending.is_none())
                else {
                    return;
                };
                let Some(trace) = page.loaded.clone() else {
                    return;
                };
                page.descriptors.open = !page.descriptors.open;
                if page.descriptors.open && page.descriptors.report.is_none() {
                    let ticket = page.begin("Tracking descriptor lifetimes…");
                    let rejected = message(Action::Rejected(tab, ticket.clone()));
                    ctx.spawn_background_with_rejection(
                        move |_| {
                            message(Action::Ready(
                                tab,
                                ticket,
                                Arc::new(trace.descriptor_report()),
                            ))
                        },
                        rejected,
                    );
                }
            }
            Action::Ready(tab, ticket, report) => {
                if let Some(page) = self.syscall_page(tab)
                    && page.finish(&ticket)
                {
                    let state = &mut page.descriptors;
                    state.show_results((0..report.lifetimes().len()).collect(), None);
                    state.report = Some(report);
                }
            }
            Action::Rejected(tab, ticket) => {
                if let Some(page) = self.syscall_page(tab)
                    && page.finish(&ticket)
                {
                    page.status = Some("Descriptor analysis worker unavailable".into());
                }
            }
            Action::Query(tab, query) => {
                if let Some(state) = self.handles_input(tab) {
                    state.query = query.chars().take(256).collect();
                }
            }
            Action::Search(tab) => self.search_descriptor_lifetimes(tab, ctx, false),
            Action::Related(tab) => self.search_descriptor_lifetimes(tab, ctx, true),
            Action::Results(tab, ticket, results, previous) => {
                if let Some(page) = self.syscall_page(tab)
                    && page.finish(&ticket)
                {
                    page.descriptors.show_results(results, previous);
                }
            }
            Action::Select(tab, index) => {
                if let Some(state) = self.handles_input(tab)
                    && index < ROWS
                    && let Some(&id) = state.visible.get(state.page * ROWS + index)
                    && state.selection != Some(id)
                {
                    state.selection = Some(id);
                    state.evidence = 0;
                }
            }
            Action::Page(tab, next) => {
                if let Some(state) = self.handles_input(tab) {
                    let page = step(state.page, state.visible.len().div_ceil(ROWS), next);
                    if state.page != page {
                        state.page = page;
                        state.selection = state.visible.get(page * ROWS).copied();
                        state.evidence = 0;
                    }
                }
            }
            Action::Evidence(tab, next) => {
                if let Some(state) = self.handles_input(tab)
                    && let Some(lifetime) = state.current()
                {
                    state.evidence = step(
                        state.evidence,
                        lifetime.steps.len().div_ceil(EVIDENCE_ROWS),
                        next,
                    );
                }
            }
        }
    }

    fn handles_input(&mut self, tab: TabId) -> Option<&mut ViewState> {
        let page = self.syscall_page(tab)?;
        (page.descriptors.open && page.pending.is_none()).then_some(&mut page.descriptors)
    }

    fn search_descriptor_lifetimes(
        &mut self,
        tab: TabId,
        ctx: &ComponentContext<Self>,
        related: bool,
    ) {
        let Some(page) = self
            .syscall_page(tab)
            .filter(|page| page.descriptors.open && page.pending.is_none())
        else {
            return;
        };
        let Some(trace) = page.loaded.clone() else {
            return;
        };
        let Some(report) = page.descriptors.report.clone() else {
            return;
        };
        let previous = page.descriptors.selection;
        let query = if related {
            let Some(selected) = previous else { return };
            let query = format!("lifetime {}", selected + 1);
            page.descriptors.query = query.clone();
            query
        } else {
            page.descriptors.query.clone()
        };
        let ticket = page.begin("Filtering descriptor lifetimes…");
        let rejected = message(Action::Rejected(tab, ticket.clone()));
        ctx.spawn_background_with_rejection(
            move |_| {
                message(Action::Results(
                    tab,
                    ticket,
                    report.search(&trace, &query),
                    previous,
                ))
            },
            rejected,
        );
    }
}

impl ViewState {
    fn show_results(&mut self, results: Vec<usize>, previous: Option<usize>) {
        self.visible = results;
        let previous = previous.and_then(|id| self.visible.iter().position(|&item| item == id));
        let position = previous.unwrap_or(0);
        self.selection = self.visible.get(position).copied();
        self.page = position / ROWS;
        self.evidence = 0;
    }

    fn current(&self) -> Option<&trace::Lifetime> {
        self.report.as_ref()?.lifetimes().get(self.selection?)
    }

    pub(super) fn switch(
        &self,
        tab: TabId,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> KeyedView {
        KeyedView::new(
            "descriptor-mode",
            Button::new()
                .is_enabled(idle)
                .on_click(ctx.callback(move |_| message(Action::Switch(tab))))
                .content(if self.open {
                    "Back to syscall groups"
                } else {
                    "File descriptors"
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
        let Some(report) = &self.report else {
            return StackPanel::new()
                .spacing(8.0)
                .margin(12.0)
                .keyed_children(header);
        };
        if report.limited {
            header.push(KeyedView::new("descriptor-analysis-limit",
                TextBlock::new().text("Descriptor tracking reached its analysis limit. Later lifetimes may be missing.").text_wrapping(TextWrapping::Wrap)));
        }
        header.push(KeyedView::new(
            "descriptor-search",
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    TextBox::new()
                        .automation_name("Search descriptor lifetimes")
                        .placeholder_text("File path, PID 123, FD 4, or Lifetime 10")
                        .is_enabled(idle)
                        .text(&self.query)
                        .on_text_changed(
                            ctx.callback(move |text| message(Action::Query(tab, text))),
                        ),
                    Button::new()
                        .grid_column(1)
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| message(Action::Search(tab))))
                        .content("Filter descriptors"),
                )),
        ));
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(12.0)
            .margin(12.0)
            .children((
                StackPanel::new().spacing(8.0).keyed_children(header),
                Grid::new()
                    .grid_row(1)
                    .columns([GridLength::Star(1.0), GridLength::Star(1.1)])
                    .column_spacing(16.0)
                    .children((
                        self.list(tab, report, idle, ctx),
                        Border::new()
                            .grid_column(1)
                            .content(self.timeline(tab, trace, idle, ctx)),
                    )),
            ))
    }

    fn list(
        &self,
        tab: TabId,
        report: &trace::DescriptorReport,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let begin = self.page * ROWS;
        let window = &self.visible[begin..(begin + ROWS).min(self.visible.len())];
        let selected = window.iter().position(|&id| Some(id) == self.selection);
        let summary = format!(
            "{} descriptor lifetimes · Page {} of {}",
            self.visible.len(),
            self.page + 1,
            self.visible.len().div_ceil(ROWS).max(1)
        );
        let list = ListView::new()
            .grid_row(1)
            .automation_name("Descriptor lifetimes")
            .selected_index(selected)
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index.map_or(Message::ControlIdle, |index| {
                    message(Action::Select(tab, index))
                })
            }))
            .collection_slot(
                ListViewSlot::Items,
                window.iter().map(|&id| {
                    let life = &report.lifetimes()[id];
                    let path: String = life.resource.chars().take(160).collect();
                    let pid = life
                        .pid
                        .map_or_else(|| "PID unknown".into(), |pid| format!("PID {pid}"));
                    let label = format!(
                        "{} · {pid} · FD {} · Lifetime {} · {}",
                        safe_text(&path),
                        life.fd,
                        id + 1,
                        life.status()
                    );
                    KeyedView::new(
                        id.to_string(),
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
                        "descriptor page",
                        idle && self.page > 0,
                        idle && begin + ROWS < self.visible.len(),
                        ctx.callback(move |_| message(Action::Page(tab, false))),
                        ctx.callback(move |_| message(Action::Page(tab, true))),
                    ),
                )),
                list,
            ))
    }

    fn timeline(
        &self,
        tab: TabId,
        trace: &Trace,
        idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let Some(life) = self.current() else {
            return TextBlock::new()
                .text(if self.visible.is_empty() && self.query.is_empty() {
                    "No supported descriptor events were found in this trace."
                } else {
                    "No descriptor lifetimes match this search."
                })
                .into();
        };
        let begin = self.evidence * EVIDENCE_ROWS;
        let end = (begin + EVIDENCE_ROWS).min(life.steps.len());
        let mut original = String::new();
        for item in &life.steps[begin..end] {
            let event = &trace.events()[item.event];
            let actor = event
                .pid
                .map_or_else(|| "PID unknown".into(), |pid| format!("PID {pid}"));
            let _ = writeln!(original, "{} · {actor} · {}", item.label(), event.name);
            for raw in event.originals() {
                append_raw(&mut original, trace, raw);
            }
        }
        let summary = format!("Events {}–{} of {}", begin + 1, end, life.steps.len());
        let heading = format!(
            "Lifetime {} · FD {} · {}",
            self.selection.unwrap_or_default() + 1,
            life.fd,
            safe_text(&life.resource)
        );
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(6.0)
            .children((
                StackPanel::new().spacing(6.0).children((
                    TextBlock::new()
                        .text(heading)
                        .font_size(16.0)
                        .text_wrapping(TextWrapping::Wrap),
                    TextBlock::new().text(summary),
                    Button::new()
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| message(Action::Related(tab))))
                        .content("Show related lifetimes"),
                    navigation(
                        "descriptor events",
                        idle && begin > 0,
                        idle && end < life.steps.len(),
                        ctx.callback(move |_| message(Action::Evidence(tab, false))),
                        ctx.callback(move |_| message(Action::Evidence(tab, true))),
                    ),
                )),
                ScrollViewer::new().grid_row(1).content(
                    TextBlock::new()
                        .automation_id("descriptor-original-text")
                        .is_text_selection_enabled(true)
                        .text_wrapping(TextWrapping::Wrap)
                        .text(original),
                ),
            ))
    }
}
