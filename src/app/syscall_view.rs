use super::*;
use std::fmt::Write as _;
use trace::{Filter, Group, Raw, Trace};

mod capture;
mod compare;
mod handles;
mod trace;

pub(super) const LABEL: &str = "Syscall trace viewer";
const GROUPS_PER_PAGE: usize = 64;
const CALLS_PER_PAGE: usize = 20;
const RAW_PREVIEW_BYTES: usize = 4096;

pub enum Action {
    Show,
    Resume(TabId),
    Open(TabId),
    Opened(TabId, Arc<()>, Result<Option<Arc<Trace>>, String>),
    Query(TabId, String),
    Filter(TabId, usize),
    Apply(TabId),
    Filtered(TabId, Arc<()>, Vec<usize>),
    Select(TabId, usize),
    Groups(TabId, bool),
    Originals(TabId, bool),
    Handles(handles::Action),
    Compare(compare::Action),
    Capture(capture::Action),
}

#[derive(Default)]
pub(super) struct PageState {
    loaded: Option<Arc<Trace>>,
    visible: Vec<usize>,
    pending: Option<Arc<()>>,
    status: Option<String>,
    query: String,
    filter: Filter,
    group_page: usize,
    raw_page: usize,
    selected: Option<usize>,
    descriptors: handles::ViewState,
    comparison: compare::ViewState,
    capture: capture::ViewState,
}

impl TerminalApp {
    pub(super) fn syscall_view_action(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if matches!(action, Action::Show) {
            let origin = self.profile_connection();
            self.command_reference.close();
            self.set_diagnostics_drawer(None);
            let current = self
                .tabs
                .entries()
                .iter()
                .find(|entry| matches!(entry.value.page, Page::SyscallTrace(_)))
                .map(|entry| entry.id);
            let tab = if let Some(current) = current {
                self.tabs.select(current);
                current
            } else {
                self.tabs.insert(AppTab {
                    label: LABEL.into(),
                    page: Page::SyscallTrace(Box::default()),
                })
            };
            if let Some(page) = self.syscall_page(tab) {
                page.capture.offer(origin);
            }
            self.sync_selection(ctx);
            self.activate(ctx);
            return;
        }

        match action {
            Action::Resume(id) => {
                if self.syscall_page(id).is_some() {
                    self.set_diagnostics_drawer(None);
                    self.tabs.select(id);
                    self.sync_selection(ctx);
                    self.activate(ctx);
                }
            }
            Action::Open(id) => self.open_syscall_trace(id, ctx),
            Action::Apply(id) => self.filter_syscall_trace(id, ctx),
            Action::Opened(id, ticket, result) => {
                if let Some(page) = self.syscall_page(id)
                    && page.finish(&ticket)
                {
                    match result {
                        Ok(Some(trace)) => {
                            page.capture.activity = None;
                            page.load(trace);
                        }
                        Ok(None) => (),
                        Err(error) => page.status = Some(error),
                    }
                }
            }
            Action::Filtered(id, ticket, visible) => {
                if let Some(page) = self.syscall_page(id)
                    && page.finish(&ticket)
                {
                    page.visible = visible;
                    page.reset_position();
                }
            }
            Action::Query(id, query) => {
                if let Some(page) = self.syscall_page(id).filter(|page| page.pending.is_none()) {
                    page.query = query.chars().take(256).collect();
                }
            }
            Action::Filter(id, index) => {
                if let Some(page) = self.syscall_page(id).filter(|page| page.pending.is_none())
                    && let Some((_, filter)) = Filter::CHOICES.get(index)
                {
                    page.filter = *filter;
                }
            }
            Action::Select(id, offset) => {
                if let Some(page) = self.syscall_page(id).filter(|page| page.pending.is_none())
                    && offset < GROUPS_PER_PAGE
                    && let Some(&group) =
                        page.visible.get(page.group_page * GROUPS_PER_PAGE + offset)
                    && page.selected != Some(group)
                {
                    page.selected = Some(group);
                    page.raw_page = 0;
                }
            }
            Action::Groups(id, next) => {
                if let Some(page) = self.syscall_page(id).filter(|page| page.pending.is_none()) {
                    let count = page.visible.len().div_ceil(GROUPS_PER_PAGE);
                    let new_page = step(page.group_page, count, next);
                    if new_page != page.group_page {
                        page.group_page = new_page;
                        page.selected = page.visible.get(new_page * GROUPS_PER_PAGE).copied();
                        page.raw_page = 0;
                    }
                }
            }
            Action::Originals(id, next) => {
                if let Some(page) = self.syscall_page(id)
                    && let Some(group) = page.selection()
                {
                    page.raw_page = step(
                        page.raw_page,
                        group.range.len().div_ceil(CALLS_PER_PAGE),
                        next,
                    );
                }
            }
            Action::Handles(action) => self.syscall_handles_action(action, ctx),
            Action::Compare(action) => self.syscall_compare_action(action, ctx),
            Action::Capture(action) => self.syscall_capture_action(action, ctx),
            Action::Show => (),
        }
    }

    fn syscall_page(&mut self, id: TabId) -> Option<&mut PageState> {
        match &mut self.tabs.get_mut(id)?.page {
            Page::SyscallTrace(page) => Some(page),
            _ => None,
        }
    }

    fn open_syscall_trace(&mut self, id: TabId, ctx: &ComponentContext<Self>) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(owner) = self.keyboard.hwnd().map(|window| window.0 as usize) else {
            return;
        };
        let initial = self.store.path().to_owned();
        let Some(page) = self.syscall_page(id).filter(|page| page.pending.is_none()) else {
            return;
        };
        let path = page
            .loaded
            .as_ref()
            .map_or(initial, |trace| trace.first_path().to_owned());
        let ticket = page.begin("Opening syscall trace…");
        let rejected = Message::SyscallView(Action::Opened(
            id,
            ticket.clone(),
            Err("Trace file worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| -> anyhow::Result<_> {
                    let files = file_picker::pick_paths(
                        owner,
                        file_picker::Purpose::OpenSyscallTrace(path),
                    )?;
                    if files.is_empty() || scope.is_cancelled() {
                        return Ok(None);
                    }
                    Ok(Some(Arc::new(Trace::open(files)?)))
                })()
                .map_err(|error| format!("{error:#}"));
                Message::SyscallView(Action::Opened(id, ticket, result))
            },
            rejected,
        );
    }

    fn filter_syscall_trace(&mut self, id: TabId, ctx: &ComponentContext<Self>) {
        let Some(page) = self.syscall_page(id).filter(|page| page.pending.is_none()) else {
            return;
        };
        let Some(trace) = page.loaded.clone() else {
            return;
        };
        let filter = page.filter;
        let query = page.query.clone();
        let ticket = page.begin("Filtering syscall trace…");
        let rejected = Message::SyscallView(Action::Opened(
            id,
            ticket.clone(),
            Err("Trace filter worker unavailable".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |_| {
                Message::SyscallView(Action::Filtered(id, ticket, trace.filtered(filter, &query)))
            },
            rejected,
        );
    }
}

fn step(current: usize, count: usize, next: bool) -> usize {
    if next {
        (current + 1).min(count.saturating_sub(1))
    } else {
        current.saturating_sub(1)
    }
}

fn send(action: Action) -> Message {
    Message::SyscallView(action)
}

impl PageState {
    pub(super) fn diagnostics_activity(&self) -> Option<&diagnostics::Activity> {
        self.capture.activity.as_ref()
    }

    pub(super) fn capture_open(&self) -> bool {
        self.capture.open
    }

    fn load(&mut self, trace: Arc<Trace>) {
        self.visible = (0..trace.groups.len()).collect();
        self.loaded = Some(trace);
        self.query.clear();
        self.filter = Filter::default();
        self.descriptors = Default::default();
        self.comparison = Default::default();
        self.capture.back();
        self.reset_position();
    }

    fn begin(&mut self, status: &str) -> Arc<()> {
        let ticket = Arc::new(());
        self.pending = Some(ticket.clone());
        self.status = Some(status.into());
        ticket
    }

    fn finish(&mut self, ticket: &Arc<()>) -> bool {
        let current = self
            .pending
            .take_if(|pending| Arc::ptr_eq(pending, ticket))
            .is_some();
        if current {
            self.status = None;
        }
        current
    }

    fn reset_position(&mut self) {
        self.group_page = 0;
        self.raw_page = 0;
        self.selected = self.visible.first().copied();
    }

    fn selection(&self) -> Option<&Group> {
        self.loaded.as_ref()?.groups.get(self.selected?)
    }

    pub(super) fn view(
        &self,
        id: TabId,
        targets: Vec<capture::Candidate>,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let idle = self.pending.is_none();
        let mut header = vec![
            KeyedView::new("title", TextBlock::new().text(LABEL).font_size(20.0)),
            KeyedView::new(
                "open",
                Button::new()
                    .is_enabled(idle)
                    .on_click(ctx.callback(move |_| send(Action::Open(id))))
                    .content("Open syscall trace…"),
            ),
        ];
        if let Some(status) = &self.status {
            header.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .text(status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        header.push(self.capture.switch(id, idle, ctx));
        if self.capture.open {
            return self.capture.view(id, targets, header, idle, ctx);
        }
        let Some(trace) = &self.loaded else {
            header.push(KeyedView::new(
                "empty",
                TextBlock::new()
                    .text("Open a strace log or select matching per-process trace files.")
                    .text_wrapping(TextWrapping::Wrap),
            ));
            return StackPanel::new()
                .spacing(8.0)
                .margin(12.0)
                .keyed_children(header);
        };
        let overview = format!(
            "{}  ·  {} files  ·  {} events  ·  {} errors or abnormal exits  ·  {} incomplete or unrecognized",
            trace
                .first_path()
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            trace.file_count(),
            trace.events.len(),
            trace.errors,
            trace.incomplete,
        );
        header.push(KeyedView::new(
            "summary",
            TextBlock::new()
                .text(overview)
                .text_wrapping(TextWrapping::Wrap),
        ));
        if trace.file_count() > 1 {
            let order = match trace.merge_order {
                trace::MergeOrder::Epoch => "Files merged by epoch timestamp",
                trace::MergeOrder::Clock { .. } => "Files merged by time of day",
                trace::MergeOrder::File => {
                    "Files shown separately because comparable timestamps are unavailable"
                }
            };
            header.push(KeyedView::new("order", TextBlock::new().text(order)));
        }
        if self.comparison.open {
            return self.comparison.view(id, header, idle, ctx);
        }
        header.push(self.comparison.launch(id, idle, ctx));
        header.push(self.descriptors.switch(id, idle, ctx));
        if self.descriptors.open {
            return self.descriptors.view(id, trace, header, idle, ctx);
        }
        let selected_filter = Filter::CHOICES
            .iter()
            .position(|(_, filter)| *filter == self.filter)
            .unwrap_or_default();
        header.push(KeyedView::new(
            "filters",
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    TextBox::new()
                        .automation_name("Search syscall trace")
                        .text(&self.query)
                        .placeholder_text("PID, syscall, resource or errno")
                        .is_enabled(idle)
                        .on_text_changed(ctx.callback(move |value| send(Action::Query(id, value)))),
                    ComboBox::new()
                        .grid_column(1)
                        .automation_name("Syscall filter")
                        .items_source(Filter::CHOICES.map(|(label, _)| label))
                        .selected_index(selected_filter)
                        .is_enabled(idle)
                        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                            index.map_or(Message::ControlIdle, |index| {
                                send(Action::Filter(id, index))
                            })
                        })),
                    Button::new()
                        .grid_column(2)
                        .is_enabled(idle)
                        .on_click(ctx.callback(move |_| send(Action::Apply(id))))
                        .content("Apply filters"),
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
                    .columns([GridLength::Star(1.1), GridLength::Star(1.0)])
                    .column_spacing(16.0)
                    .children((
                        self.groups(id, trace, ctx),
                        Border::new()
                            .grid_column(1)
                            .content(self.originals(id, trace, ctx)),
                    )),
            ))
    }

    fn groups(&self, id: TabId, trace: &Trace, ctx: &ViewContext<TerminalApp>) -> View {
        let idle = self.pending.is_none();
        let start = self.group_page * GROUPS_PER_PAGE;
        let groups = &self.visible[start..(start + GROUPS_PER_PAGE).min(self.visible.len())];
        let choice = groups
            .iter()
            .position(|&group| Some(group) == self.selected);
        let pager = format!(
            "{} groups · Page {} of {}",
            self.visible.len(),
            self.group_page + 1,
            self.visible.len().div_ceil(GROUPS_PER_PAGE).max(1)
        );
        let list = ListView::new()
            .grid_row(1)
            .automation_name("Syscall groups")
            .selected_index(choice)
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index.map_or(Message::ControlIdle, |index| {
                    send(Action::Select(id, index))
                })
            }))
            .collection_slot(
                ListViewSlot::Items,
                groups.iter().map(|&index| {
                    let label = group_label(trace, index);
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
                        .text(pager)
                        .text_wrapping(TextWrapping::Wrap),
                    navigation(
                        "group page",
                        idle && self.group_page > 0,
                        idle && start + GROUPS_PER_PAGE < self.visible.len(),
                        ctx.callback(move |_| send(Action::Groups(id, false))),
                        ctx.callback(move |_| send(Action::Groups(id, true))),
                    ),
                )),
                list,
            ))
    }

    fn originals(&self, id: TabId, trace: &Trace, ctx: &ViewContext<TerminalApp>) -> View {
        let Some(group) = self.selection() else {
            return TextBlock::new()
                .text("No groups match these filters.")
                .into();
        };
        let start = group.range.start + self.raw_page * CALLS_PER_PAGE;
        let end = (start + CALLS_PER_PAGE).min(group.range.end);
        let mut original = String::new();
        for raw in trace.events[start..end]
            .iter()
            .flat_map(|event| event.originals())
        {
            append_raw(&mut original, trace, raw);
        }
        let detail = format!(
            "Calls {}–{} of {}",
            start - group.range.start + 1,
            end - group.range.start,
            group.range.len()
        );
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(6.0)
            .children((
                StackPanel::new().spacing(6.0).children((
                    TextBlock::new()
                        .text("Original trace records")
                        .font_size(16.0),
                    TextBlock::new().text(detail),
                    navigation(
                        "original records",
                        start > group.range.start,
                        end < group.range.end,
                        ctx.callback(move |_| send(Action::Originals(id, false))),
                        ctx.callback(move |_| send(Action::Originals(id, true))),
                    ),
                )),
                ScrollViewer::new().grid_row(1).content(
                    TextBlock::new()
                        .automation_id("syscall-original-text")
                        .is_text_selection_enabled(true)
                        .text_wrapping(TextWrapping::Wrap)
                        .text(original),
                ),
            ))
    }
}

fn append_raw(text: &mut String, trace: &Trace, raw: Raw) {
    let (path, bytes) = trace.original(raw);
    let file = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let _ = writeln!(text, "{} · Line {}", safe_text(&file), raw.line);
    let fragment = &bytes[..bytes.len().min(RAW_PREVIEW_BYTES)];
    text.push_str(&safe_text(
        String::from_utf8_lossy(fragment).trim_end_matches(['\r', '\n']),
    ));
    if bytes.len() > fragment.len() {
        text.push_str(" [preview clipped]");
    }
    text.push_str("\n\n");
}

fn group_label(trace: &Trace, index: usize) -> String {
    let group = &trace.groups[index];
    let event = &trace.events[group.range.start];
    let pid = event
        .pid
        .map_or_else(|| "PID unknown".into(), |pid| format!("PID {pid}"));
    let mut label = format!("{pid} · {}", event.name);
    if !event.resource.is_empty() {
        let shortened: String = event.resource.chars().take(150).collect();
        let _ = write!(label, " · {}", safe_text(&shortened));
    }
    let outcome = event.error.as_deref().unwrap_or(event.outcome.label());
    let _ = write!(label, " · {outcome} · {}×", group.range.len());
    if group.timed > 0 {
        let _ = write!(
            label,
            " · {:.3} ms in calls",
            group.duration as f64 / 1_000_000.0
        );
    }
    if let Some(start) = trace.relative_start(group.range.start) {
        let _ = write!(label, " · +{:.3} ms", start as f64 / 1_000_000.0);
    }
    if event.later_success {
        label.push_str(" · Later success on same path");
    }
    label
}

fn navigation(
    what: &str,
    previous: bool,
    next: bool,
    go_back: Callback<()>,
    go_forward: Callback<()>,
) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0)
        .children((
            Button::new()
                .automation_name(format!("Previous {what}"))
                .is_enabled(previous)
                .on_click(go_back)
                .content("Previous"),
            Button::new()
                .automation_name(format!("Next {what}"))
                .is_enabled(next)
                .on_click(go_forward)
                .content("Next"),
        ))
}

fn safe_text(text: &str) -> String {
    let mut safe = String::with_capacity(text.len());
    for c in text.chars() {
        if (c.is_control() && c != '\t')
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            safe.extend(c.escape_default());
        } else {
            safe.push(c);
        }
    }
    safe
}
