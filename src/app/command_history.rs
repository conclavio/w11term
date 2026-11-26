use super::*;
use regex::{Regex, RegexBuilder};
use std::{cmp::Reverse, collections::HashMap, ops::Range, sync::Weak};
use w11term::control::CommandStatus;
use w11term::events::CommandLink;
use w11term::saved_commands::{Entry, valid_command_text};
use w11term::session::history_text_allowed;
use w11term::terminal::Snapshot;
use wezterm_term::CompletedCommand;

mod compare;
pub use compare::Request as CompareRequest;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputCapture {
    pane: u64,
    link: CommandLink,
}

pub(super) struct HistoryPage {
    pane: u64,
    selected: Option<(u64, u64)>, // Connection generation and command id.
    search: HistorySearch,
    output_hit: Option<(CommandLink, Range<usize>)>,
    window_search: Option<Box<pane_picker::WindowHistoryReturn>>,
    status: CommandStatus,
    order: HistoryOrder,
    feedback: String,
    save: Option<SaveDraft>,
    showing: Option<Arc<()>>,
    comparison: Option<compare::Panel>,
}

struct SaveDraft {
    target: (u64, u64),
    name: String,
}

pub enum Action {
    Open,
    InPane(u64, HistoryAction),
    OutputShown {
        page: TabId,
        pane: u64,
        target: (u64, u64),
        request: Arc<()>,
        result: Result<(), &'static str>,
    },
    ComparisonReady {
        page: TabId,
        request: Arc<()>,
        result: Result<text_diff::Difference, &'static str>,
    },
}

pub enum HistoryAction {
    Search(String),
    IncludeOutput(bool),
    Status(CommandStatus),
    Order(HistoryOrder),
    Refresh,
    Select(u64, u64),
    Copy(u64, u64, CopyPart),
    ExportOutput((u64, u64)),
    ShowOutput((u64, u64)),
    OpenDirectory((u64, u64)),
    Compare(CompareRequest),
    Insert((u64, u64)),
    BeginSave((u64, u64)),
    SaveName((u64, u64), String),
    Save((u64, u64)),
    CancelSave((u64, u64)),
    Return,
    ReturnToWindowSearch,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HistoryOrder {
    #[default]
    Newest,
    Slowest,
}

enum DirectoryNavigation {
    Sftp(TabId, String),
    Explorer(explorer::Location),
}

#[derive(Clone, Copy)]
pub enum CopyPart {
    Command,
    Output,
}

impl CopyPart {
    fn text(self, entry: &CompletedCommand) -> Option<&str> {
        match self {
            Self::Command => entry.command.as_deref(),
            Self::Output => entry.output.as_ref().ok().map(String::as_str),
        }
    }
}

pub(super) const STATUS_OPTIONS: [(CommandStatus, &str); 4] = [
    (CommandStatus::All, "All commands"),
    (CommandStatus::Failed, "Failed"),
    (CommandStatus::Succeeded, "Succeeded"),
    (CommandStatus::Unknown, "Status unknown"),
];

pub(super) const ORDER_OPTIONS: [(HistoryOrder, &str); 2] = [
    (HistoryOrder::Newest, "Newest first"),
    (HistoryOrder::Slowest, "Slowest first"),
];

#[derive(Default)]
struct HistorySearch {
    query: String,
    needle: String,
    include_output: bool,
    matcher: Option<Regex>,
    cache: RefCell<OutputMatches>,
}

#[derive(Default)]
struct OutputMatches {
    // Previewing command history should not keep evicted output in memory.
    history: Weak<VecDeque<Arc<CompletedCommand>>>,
    ranges: HashMap<u64, Range<usize>>,
}

impl HistorySearch {
    fn update_matcher(&mut self) {
        self.matcher = if self.include_output && !self.query.is_empty() {
            output_matcher(&self.query)
        } else {
            None
        };
        *self.cache.get_mut() = OutputMatches::default();
    }

    fn output_match(&self, frame: &Snapshot, id: u64) -> Option<Range<usize>> {
        let matcher = self.matcher.as_ref()?;
        let mut cache = self.cache.borrow_mut();
        if cache.history.as_ptr() != Arc::as_ptr(&frame.command_history) {
            cache.ranges = frame
                .command_history
                .iter()
                .filter_map(|entry| {
                    let text = entry.output.as_ref().ok()?;
                    let hit = matcher.find(text)?;
                    Some((entry.id, hit.range()))
                })
                .collect();
            cache.history = Arc::downgrade(&frame.command_history);
        }
        cache.ranges.get(&id).cloned()
    }

    fn includes(&self, frame: &Snapshot, entry: &CompletedCommand, status: CommandStatus) -> bool {
        status.matches(entry.exit_code)
            && (command_text_matches(entry, &self.needle)
                || self.output_match(frame, entry.id).is_some())
    }
}

fn output_matcher(query: &str) -> Option<Regex> {
    RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
        .ok()
}

pub(super) struct WindowCapture {
    pane: u64,
    generation: u64,
    source: Arc<str>,
    command: Arc<CompletedCommand>,
    fields_match: bool,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub(super) struct WindowHistoryOptions {
    pub(super) status: CommandStatus,
    pub(super) order: HistoryOrder,
    pub(super) include_output: bool,
}

pub(super) fn window_history_rows(
    captures: Vec<WindowCapture>,
    output_query: Option<&str>,
    order: HistoryOrder,
    cancelled: impl Fn() -> bool,
) -> Vec<command_palette::Row> {
    let matcher = output_query.and_then(output_matcher);
    let mut rows = Vec::new();
    for capture in captures {
        if cancelled() {
            return Vec::new();
        }
        let command = &capture.command;
        let output_match = matcher.as_ref().and_then(|matcher| {
            let output = command.output.as_deref().ok()?;
            Some((output, matcher.find(output)?.range()))
        });
        if !capture.fields_match && output_match.is_none() {
            continue;
        }
        let pane = capture.pane;
        let link = CommandLink {
            generation: capture.generation,
            id: command.id,
        };
        let mut label = format!(
            "{} · {} · {}",
            capture.source,
            command_status(command),
            command_name(command),
        );
        if let Some((output, hit)) = &output_match {
            label = format!("{} · {label}", output_excerpt(output, hit));
        }
        let (duration, elapsed) = match order {
            HistoryOrder::Newest => (None, String::new()),
            HistoryOrder::Slowest => (
                command.timing.duration_ms,
                command
                    .timing
                    .duration_ms
                    .map_or_else(|| "Duration unavailable".into(), elapsed_label),
            ),
        };
        rows.push((
            Reverse((duration, command.finished_at, pane, command.id)),
            command_palette::Row {
                id: command_palette::CommandId::OpenWindowCommand(
                    pane,
                    link,
                    output_match.map(|(_, hit)| hit),
                ),
                label,
                chord: elapsed,
                enabled: true,
            },
        ));
    }
    rows.sort_unstable_by_key(|(order, _)| *order);
    rows.into_iter().map(|(_, row)| row).collect()
}

fn output_excerpt(output: &str, hit: &Range<usize>) -> String {
    let start = output.ceil_char_boundary(hit.start.saturating_sub(24));
    let end = output.floor_char_boundary(output.len().min(hit.start + 64));
    let excerpt = output[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "{}{excerpt}{}",
        if start > 0 { "…" } else { "" },
        if end < output.len() { "…" } else { "" }
    )
}

fn command_text_matches(entry: &CompletedCommand, needle: &str) -> bool {
    needle.is_empty()
        || entry
            .command
            .as_ref()
            .is_some_and(|text| text.to_lowercase().contains(needle))
}

fn command_name(entry: &CompletedCommand) -> String {
    entry.command.as_ref().map_or_else(
        || format!("Command {}", entry.id),
        |text| {
            text.chars()
                .take(120)
                .map(|ch| if ch.is_whitespace() { ' ' } else { ch })
                .collect()
        },
    )
}

fn command_status(entry: &CompletedCommand) -> String {
    entry
        .exit_code
        .map_or_else(|| "status unknown".into(), |code| format!("exit {code}"))
}

fn elapsed_label(ms: u64) -> String {
    match ms {
        0 => "<1 ms".into(),
        1..=999 => format!("{ms} ms"),
        1000..=59_999 => format!("{}.{:03} s", ms / 1000, ms % 1000),
        _ => {
            let seconds = ms / 1000;
            format!("{}m {:02}s", seconds / 60, seconds % 60)
        }
    }
}

pub(super) fn timestamp_label(ms: u64) -> Option<String> {
    let time = chrono::DateTime::from_timestamp_millis(i64::try_from(ms).ok()?)?;
    Some(time.format("%Y-%m-%d %H:%M:%S UTC").to_string())
}

fn timing_label(entry: &CompletedCommand) -> String {
    let timing = entry.timing;
    let mut fields = Vec::new();
    if let Some(start) = timing.started_unix_ms.and_then(timestamp_label) {
        fields.push(format!("Started {start}"));
    }
    if let Some(finish) = timestamp_label(timing.finished_unix_ms) {
        fields.push(format!("Finished {finish}"));
    }
    if let Some(elapsed) = timing.duration_ms {
        fields.push(format!("Elapsed {}", elapsed_label(elapsed)));
    }
    fields.join(" · ")
}

fn command_directory(
    source: &Source,
    entry: &CompletedCommand,
) -> Option<directory_split::Directory> {
    directory_split::Directory::reported(source, entry.reported_directory.as_deref()?)
}

fn output_preview(text: &str, hit: Option<Range<usize>>) -> String {
    const PREVIEW_BYTES: usize = 8192;
    const CONTEXT_BYTES: usize = 384;
    let start = hit.map_or(0, |hit| {
        text.ceil_char_boundary(hit.start.saturating_sub(CONTEXT_BYTES))
    });
    let end = text.floor_char_boundary(text.len().min(start + PREVIEW_BYTES));
    let mut preview = String::new();
    if start > 0 {
        preview.push_str("…\r\n");
    }
    preview.push_str(&text[start..end]);
    if start > 0 || end < text.len() {
        preview.push_str("\r\n… Copy output for the full text.");
    }
    preview
}

fn selected_command(frame: &Snapshot, selected: Option<(u64, u64)>) -> Option<&CompletedCommand> {
    let (generation, id) = selected?;
    frame
        .command_for_link(w11term::events::CommandLink { generation, id })
        .map(AsRef::as_ref)
}

fn saveable_command(frame: &Snapshot, target: (u64, u64)) -> Option<&str> {
    selected_command(frame, Some(target))?
        .command
        .as_deref()
        .filter(|text| valid_command_text(text))
}

fn history_matches<'a>(
    frame: &'a Snapshot,
    search: &HistorySearch,
    status: CommandStatus,
    order: HistoryOrder,
) -> Vec<&'a CompletedCommand> {
    let mut matches: Vec<_> = frame
        .command_history
        .iter()
        .rev()
        .map(AsRef::as_ref)
        .filter(|entry| search.includes(frame, entry, status))
        .collect();
    if order == HistoryOrder::Slowest {
        matches.sort_unstable_by_key(|entry| Reverse((entry.timing.duration_ms, entry.id)));
    }
    matches
}

fn first_matching(
    frame: &Snapshot,
    search: &HistorySearch,
    status: CommandStatus,
    order: HistoryOrder,
) -> Option<(u64, u64)> {
    history_matches(frame, search, status, order)
        .first()
        .map(|entry| (frame.connection_generation, entry.id))
}

impl HistoryPage {
    fn keep_matching_selection(&mut self, frame: &Snapshot) {
        if !selected_command(frame, self.selected)
            .is_some_and(|entry| self.search.includes(frame, entry, self.status))
        {
            self.selected = first_matching(frame, &self.search, self.status, self.order);
        }
    }
}

impl TerminalApp {
    #[cfg(feature = "ui-smoke")]
    pub(super) fn history_smoke_select_older(&mut self, ctx: &ComponentContext<Self>) {
        let Page::CommandHistory(page) = &self.tabs.active().unwrap().page else {
            panic!("expected history page")
        };
        let frame = self.history_frame(page.pane).unwrap();
        assert_eq!(
            selected_command(&frame, page.selected)
                .unwrap()
                .command
                .as_deref(),
            Some("status")
        );
        let first = frame.command_history.front().unwrap().id;
        self.update(
            Message::History(Action::InPane(
                page.pane,
                HistoryAction::Select(frame.connection_generation, first),
            )),
            ctx,
        );
    }

    #[cfg(feature = "ui-smoke")]
    pub(super) fn history_smoke_verify_older(&self) {
        let Page::CommandHistory(page) = &self.tabs.active().unwrap().page else {
            panic!("expected history page")
        };
        let frame = self.history_frame(page.pane).unwrap();
        let selected = selected_command(&frame, page.selected).unwrap();
        assert_eq!(selected.command.as_deref(), Some("first"));
        assert_eq!(selected.output.as_deref(), Ok("first result\r\n"));
        assert_eq!(selected.exit_code, Some(0));
    }

    pub(super) fn history_frame(&self, pane: u64) -> Option<Arc<Snapshot>> {
        let tab = self.tabs.get(self.tab_for_pane(pane)?)?;
        match &tab.page {
            Page::Terminal(live) => Some(live.session.frames.load()),
            _ => None,
        }
    }

    pub(super) fn window_command_history_rows(
        &self,
        query: &str,
        options: WindowHistoryOptions,
    ) -> Vec<command_palette::Row> {
        window_history_rows(
            self.window_command_captures(query, options),
            None,
            options.order,
            || false,
        )
    }

    pub(super) fn window_command_captures(
        &self,
        query: &str,
        options: WindowHistoryOptions,
    ) -> Vec<WindowCapture> {
        let needle = query.trim().to_lowercase();
        let mut captures = Vec::new();
        for tab in self.tabs.entries().iter().map(|entry| &entry.value) {
            let Page::Terminal(live) = &tab.page else {
                continue;
            };
            let pane = live.session.pane_id;
            let label = tab
                .pane_name()
                .map_or(tab.label.as_str(), |name| name.as_str());
            let source = match &live.source {
                Source::Ssh(ssh) => format!(
                    "{label} · {}@{}:{} · Pane {pane}",
                    ssh.user, ssh.host, ssh.port
                ),
                Source::Local(_) => format!("{label} · Pane {pane}"),
                _ => continue,
            };
            let source_matches = source.to_lowercase().contains(&needle)
                || tab
                    .profile_name()
                    .is_some_and(|name| name.to_lowercase().contains(&needle));
            let source: Arc<str> = source.into();
            let frame = live.session.frames.load();
            for command in frame
                .command_history
                .iter()
                .filter(|entry| options.status.matches(entry.exit_code))
            {
                let fields_match = source_matches
                    || command_text_matches(command, &needle)
                    || command_directory(&live.source, command)
                        .is_some_and(|path| path.display().to_lowercase().contains(&needle));
                if fields_match || (options.include_output && command.output.is_ok()) {
                    captures.push(WindowCapture {
                        pane,
                        generation: frame.connection_generation,
                        source: Arc::clone(&source),
                        command: Arc::clone(command),
                        fields_match,
                    });
                }
            }
        }
        captures
    }

    fn history_directory(
        &self,
        pane: u64,
        generation: u64,
        entry: &CompletedCommand,
    ) -> Option<(directory_split::Directory, Option<DirectoryNavigation>)> {
        let tab_id = self.tab_for_pane(pane)?;
        let Page::Terminal(live) = &self.tabs.get(tab_id)?.page else {
            return None;
        };
        let directory = command_directory(&live.source, entry)?;
        let navigation = match &directory {
            directory_split::Directory::Ssh(path) => (live.session.ssh_connection_generation()
                == Some(generation)
                && live.files.can_reveal_path())
            .then(|| DirectoryNavigation::Sftp(tab_id, path.clone())),
            _ => directory
                .explorer_location(&live.source)
                .map(DirectoryNavigation::Explorer),
        };
        Some((directory, navigation))
    }

    fn history_insert_target(
        &self,
        pane: u64,
        frame: &Snapshot,
    ) -> Option<(TabId, clipboard_paste::Origin)> {
        let id = self.tab_for_pane(pane)?;
        let Page::Terminal(live) = &self.tabs.get(id)?.page else {
            return None;
        };
        if frame.alternate
            || frame.prompt.is_some()
            || live.read_only.get()
            || !live.session.events.idle_shell(frame.connection_generation)
        {
            return None;
        }
        Some((
            id,
            clipboard_paste::Origin {
                pane,
                group: self.tabs.group_id(id)?,
                connection: frame.connection_generation,
            },
        ))
    }

    fn show_history_output(
        &mut self,
        page_id: TabId,
        pane: u64,
        target: (u64, u64),
        ctx: &ComponentContext<Self>,
    ) {
        let Some(Page::Terminal(live)) = self
            .tab_for_pane(pane)
            .and_then(|id| self.tabs.get(id))
            .map(|tab| &tab.page)
        else {
            return;
        };
        let input = live.session.input.clone();
        let Some(Page::CommandHistory(page)) = self.tabs.get_mut(page_id).map(|tab| &mut tab.page)
        else {
            return;
        };
        let (reply, received) = tokio::sync::oneshot::channel();
        let request = Arc::new(());
        let complete = {
            let request = request.clone();
            move |result| {
                Message::History(Action::OutputShown {
                    page: page_id,
                    pane,
                    target,
                    request: request.clone(),
                    result,
                })
            }
        };
        page.showing = Some(request);
        let rejected = complete(Err("Could not locate the command output."));
        let task = ctx.spawn_background_with_rejection(
            move |_| {
                complete(
                    received
                        .blocking_recv()
                        .unwrap_or(Err("The source pane has closed.")),
                )
            },
            rejected,
        );
        if task.status() != ComponentTaskStatus::Rejected {
            let _ = input.send(Command::RevealCommand {
                connection: target.0,
                command: target.1,
                reply,
            });
        }
    }

    pub(super) fn open_command_history(
        &mut self,
        pane: u64,
        target: Option<w11term::events::CommandLink>,
        ctx: &ComponentContext<Self>,
    ) -> bool {
        self.open_command_history_preview(pane, target, None, None, ctx)
    }

    pub(super) fn open_command_history_preview(
        &mut self,
        pane: u64,
        target: Option<CommandLink>,
        output_hit: Option<Range<usize>>,
        window_search: Option<pane_picker::WindowHistoryReturn>,
        ctx: &ComponentContext<Self>,
    ) -> bool {
        let Some(source) = self.tab_for_pane(pane).and_then(|id| self.tabs.get(id)) else {
            return false;
        };
        let Some(frame) = self.history_frame(pane) else {
            return false;
        };
        let label = format!("History · {}", source.label);
        let search = HistorySearch::default();
        let selected = match target {
            Some(link) if frame.has_command(link) => Some((link.generation, link.id)),
            Some(_) => return false,
            None => first_matching(&frame, &search, CommandStatus::All, HistoryOrder::Newest),
        };
        let existing = self
            .tabs
            .entries()
            .iter()
            .find(|tab| matches!(&tab.value.page, Page::CommandHistory(page) if page.pane == pane))
            .map(|tab| tab.id);
        let page = Page::CommandHistory(HistoryPage {
            pane,
            selected,
            search,
            output_hit: target.zip(output_hit),
            window_search: window_search.map(Box::new),
            status: CommandStatus::All,
            order: HistoryOrder::Newest,
            feedback: String::new(),
            save: None,
            showing: None,
            comparison: None,
        });
        if let Some(id) = existing {
            self.tabs.get_mut(id).unwrap().page = page;
            self.tabs.select(id);
        } else {
            self.tabs.insert(AppTab { label, page });
        }
        self.sync_selection(ctx);
        self.activate(ctx);
        true
    }

    pub(super) fn handle_history(&mut self, action: Action, ctx: &ComponentContext<Self>) {
        if let Action::ComparisonReady {
            page,
            request,
            result,
        } = action
        {
            self.finish_history_comparison(page, &request, result);
            return;
        }
        if let Action::OutputShown {
            page,
            pane,
            target,
            request,
            result,
        } = action
        {
            let active = self.tabs.active_id() == Some(page);
            let frame = self.history_frame(pane);
            let Some(Page::CommandHistory(view)) = self.tabs.get_mut(page).map(|tab| &mut tab.page)
            else {
                return;
            };
            if view.pane != pane
                || !view
                    .showing
                    .as_ref()
                    .is_some_and(|pending| Arc::ptr_eq(pending, &request))
            {
                return;
            }
            view.showing = None;
            if view.selected == Some(target) {
                match result {
                    Ok(())
                        if frame
                            .as_ref()
                            .is_some_and(|frame| frame.connection_generation == target.0) =>
                    {
                        if active && let Some(source) = self.tab_for_pane(pane) {
                            self.tabs.select(source);
                            self.sync_selection(ctx);
                            self.activate(ctx);
                        }
                    }
                    Ok(()) => view.feedback = "The source pane changed.".into(),
                    Err(error) => view.feedback = error.into(),
                }
            }
            return;
        }
        if matches!(action, Action::Open) {
            let Some(source) = self.tabs.active() else {
                return;
            };
            let Some(pane) = source.pane_id() else { return };
            self.open_command_history(pane, None, ctx);
            return;
        }
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let Some(AppTab {
            page: Page::CommandHistory(history),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let pane = history.pane;
        let Action::InPane(source, action) = action else {
            return;
        };
        if source != pane {
            return;
        }
        if let HistoryAction::Compare(request) = action {
            self.history_compare(id, pane, request, ctx);
            return;
        }
        if matches!(action, HistoryAction::ReturnToWindowSearch) {
            if let Some(saved) = history.window_search.as_deref().cloned() {
                self.restore_window_history(saved, ctx);
            }
            return;
        }
        let Some(frame) = self.history_frame(pane) else {
            return;
        };
        let Page::CommandHistory(page) = &mut self.tabs.get_mut(id).unwrap().page else {
            return;
        };
        if matches!(&action, HistoryAction::Status(status) if *status == page.status)
            || matches!(&action, HistoryAction::Order(order) if *order == page.order)
            || matches!(&action, HistoryAction::IncludeOutput(include) if *include == page.search.include_output)
            || matches!(&action, HistoryAction::Search(query) if *query == page.search.query)
        {
            return;
        }
        page.feedback.clear();
        if matches!(
            action,
            HistoryAction::Search(_)
                | HistoryAction::IncludeOutput(_)
                | HistoryAction::Refresh
                | HistoryAction::Select(..)
        ) {
            page.output_hit = None;
        }
        match action {
            HistoryAction::Compare(_) | HistoryAction::ReturnToWindowSearch => unreachable!(),
            HistoryAction::Search(query) => {
                page.search.needle = query.to_lowercase();
                page.search.query = query;
                page.search.update_matcher();
                page.keep_matching_selection(&frame);
            }
            HistoryAction::IncludeOutput(include) => {
                page.search.include_output = include;
                page.search.update_matcher();
                page.keep_matching_selection(&frame);
            }
            HistoryAction::Status(status) => {
                page.status = status;
                page.keep_matching_selection(&frame);
            }
            HistoryAction::Order(order) => {
                page.order = order;
                page.keep_matching_selection(&frame);
            }
            HistoryAction::Refresh => {
                page.selected = first_matching(&frame, &page.search, page.status, page.order);
            }
            HistoryAction::Select(generation, command) => {
                let key = Some((generation, command));
                if selected_command(&frame, key)
                    .is_some_and(|entry| page.search.includes(&frame, entry, page.status))
                {
                    page.selected = key;
                }
            }
            HistoryAction::Copy(generation, command, part) => {
                let key = Some((generation, command));
                if page.selected == key
                    && let Some(entry) = selected_command(&frame, key)
                    && let Some(text) = part.text(entry)
                {
                    page.feedback = match w11term::clipboard::write(text) {
                        Ok(()) => match part {
                            CopyPart::Command => "Copied command".into(),
                            CopyPart::Output => "Copied output".into(),
                        },
                        Err(error) => format!("Could not copy to clipboard · {error}"),
                    };
                }
            }
            HistoryAction::ExportOutput(target) => {
                if page.selected == Some(target)
                    && let Some(output) = selected_command(&frame, Some(target))
                        .and_then(|entry| CopyPart::Output.text(entry))
                {
                    self.export_command_output(pane, target.1, output, ctx);
                }
                return;
            }
            HistoryAction::ShowOutput(target) => {
                if page.showing.is_none()
                    && page.selected == Some(target)
                    && selected_command(&frame, Some(target)).is_some()
                    && !frame.alternate
                {
                    self.show_history_output(id, pane, target, ctx);
                }
                return;
            }
            HistoryAction::Insert(target) => {
                if page.selected != Some(target) {
                    return;
                }
                let Some(text) = selected_command(&frame, Some(target))
                    .and_then(|entry| entry.command.as_ref())
                    .filter(|text| history_text_allowed(text, frame.bracketed_paste))
                else {
                    return;
                };
                if let Some((source, origin)) = self.history_insert_target(pane, &frame) {
                    self.tabs.select(source);
                    self.sync_selection(ctx);
                    self.activate(ctx);
                    self.paste_history_text(origin, target.1, text.clone(), ctx);
                }
                return;
            }
            HistoryAction::BeginSave(target) => {
                if page.selected == Some(target) && saveable_command(&frame, target).is_some() {
                    page.save = Some(SaveDraft {
                        target,
                        name: String::new(),
                    });
                }
            }
            HistoryAction::OpenDirectory(target) => {
                if page.selected != Some(target) {
                    return;
                }
                let navigation = selected_command(&frame, Some(target))
                    .and_then(|entry| self.history_directory(pane, target.0, entry))
                    .and_then(|(_, navigation)| navigation);
                match navigation {
                    Some(DirectoryNavigation::Explorer(location)) => {
                        explorer::open_location(location, ctx);
                    }
                    Some(DirectoryNavigation::Sftp(tab, path)) if self.tabs.select(tab) => {
                        self.file_action(FileAction::Reveal(pane, target.0, path, None), ctx);
                        self.sync_selection(ctx);
                        self.activate(ctx);
                    }
                    _ => {}
                }
                return;
            }
            HistoryAction::SaveName(target, name) => {
                if let Some(draft) = &mut page.save
                    && draft.target == target
                {
                    draft.name = name;
                }
            }
            HistoryAction::Save(target) => {
                if let Some(draft) = &page.save
                    && draft.target == target
                    && page.selected == Some(target)
                    && let Some(text) = saveable_command(&frame, target)
                {
                    let entry = Entry {
                        name: draft.name.clone(),
                        text: text.into(),
                    };
                    match saved_commands::create(entry) {
                        Ok(_) => {
                            page.save = None;
                            page.feedback = "Saved command".into();
                        }
                        Err(error) => page.feedback = format!("{error:#}"),
                    }
                }
            }
            HistoryAction::CancelSave(target) => {
                if page
                    .save
                    .as_ref()
                    .is_some_and(|draft| draft.target == target)
                {
                    page.save = None;
                }
            }
            HistoryAction::Return => {
                if let Some(source) = self.tab_for_pane(pane) {
                    self.tabs.select(source);
                    self.sync_selection(ctx);
                    self.activate(ctx);
                }
                return;
            }
        }
        if page
            .save
            .as_ref()
            .is_some_and(|draft| Some(draft.target) != page.selected)
        {
            page.save = None;
        }
    }

    pub(super) fn history_view(&self, page: &HistoryPage, ctx: &ViewContext<Self>) -> View {
        if let Some(comparison) = &page.comparison {
            return self.history_comparison_view(page.pane, comparison, ctx);
        }
        let pane = page.pane;
        let action = move |request| Message::History(Action::InPane(pane, request));
        let page_heading = Grid::new()
            .grid_column_span(2)
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                TextBlock::new()
                    .font_size(24.0)
                    .text_wrapping(TextWrapping::Wrap)
                    .text(format!("Command history · Pane {pane}")),
                if page.window_search.is_some() {
                    Button::new()
                        .grid_column(1)
                        .on_click(
                            ctx.callback(move |_| action(HistoryAction::ReturnToWindowSearch)),
                        )
                        .content("Back to search results")
                } else {
                    View::empty()
                },
            ));
        let Some(frame) = self.history_frame(page.pane) else {
            return StackPanel::new().margin(16.0).spacing(8.0).children((
                page_heading,
                TextBlock::new().text("The source pane has closed"),
            ));
        };
        let generation = frame.connection_generation;
        let selected = selected_command(&frame, page.selected)
            .filter(|entry| page.search.includes(&frame, entry, page.status));
        let matches = history_matches(&frame, &page.search, page.status, page.order);
        let has_matches = !matches.is_empty();
        let rows = matches.into_iter().map(|entry| {
            let id = entry.id;
            let name = command_name(entry);
            let status = command_status(entry);
            let label = format!("{name} · {status}");
            let active = page.selected == Some((generation, id));
            KeyedView::new(
                format!("{generation}/{id}"),
                Button::new()
                    .style(if active {
                        ButtonStyle::Accent
                    } else {
                        ButtonStyle::Subtle
                    })
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .horizontal_content_alignment(HorizontalAlignment::Stretch)
                    .automation_name(if active {
                        format!("Selected, {label}")
                    } else {
                        label.clone()
                    })
                    .on_click(ctx.callback(move |_| action(HistoryAction::Select(generation, id))))
                    .content(
                        Grid::new()
                            .columns([GridLength::STAR, GridLength::Auto])
                            .column_spacing(8.0)
                            .children((
                                TextBlock::new()
                                    .text(label)
                                    .text_trimming(TextTrimming::CharacterEllipsis),
                                TextBlock::new().grid_column(1).text(
                                    entry
                                        .timing
                                        .duration_ms
                                        .map_or_else(String::new, elapsed_label),
                                ),
                            )),
                    ),
            )
        });
        let preview_hit = selected.and_then(|entry| {
            page.search.output_match(&frame, entry.id).or_else(|| {
                page.output_hit.as_ref().and_then(|(link, range)| {
                    (link.generation == generation && link.id == entry.id).then(|| range.clone())
                })
            })
        });
        // Mount a fresh native scroll surface when the matching context moves.
        let preview_key = selected.zip(preview_hit.as_ref()).map_or_else(
            || "output".to_owned(),
            |(entry, hit)| format!("{generation}/{}/{}", entry.id, hit.start),
        );
        let (heading, preview) = match selected {
            Some(entry) => {
                let heading = entry
                    .command
                    .clone()
                    .unwrap_or_else(|| format!("Command {}", entry.id));
                let preview = match &entry.output {
                    Err(why) => (*why).to_owned(),
                    Ok(text) if text.is_empty() => "No output".to_owned(),
                    Ok(text) => output_preview(text, preview_hit),
                };
                (heading, preview)
            }
            None if frame.command_history.is_empty() => (
                "Waiting for a completed command".into(),
                "This pane needs a shell that sends OSC 133 command markers.".into(),
            ),
            None if !has_matches => ("No matching commands".into(), String::new()),
            None => ("Select a command".into(), String::new()),
        };
        let copy_button = |label: &'static str, part: CopyPart| {
            let id = selected
                .filter(|entry| part.text(entry).is_some())
                .map(|entry| entry.id);
            Button::new()
                .is_enabled(id.is_some())
                .on_click(ctx.callback(move |_| {
                    action(HistoryAction::Copy(
                        generation,
                        id.unwrap_or_default(),
                        part,
                    ))
                }))
                .content(label)
        };
        let save_target = page
            .selected
            .filter(|&target| saveable_command(&frame, target).is_some());
        let insert_target = page.selected.filter(|_| {
            selected
                .and_then(|entry| entry.command.as_deref())
                .is_some_and(|text| history_text_allowed(text, frame.bracketed_paste))
                && self.history_insert_target(pane, &frame).is_some()
        });
        let export_target = page.selected.filter(|_| {
            selected.is_some_and(|entry| entry.output.is_ok()) && self.export.pending.is_none()
        });
        let show_target = page
            .selected
            .filter(|_| selected.is_some() && !frame.alternate && page.showing.is_none());
        let directory = selected
            .and_then(|entry| {
                self.history_directory(pane, generation, entry)
                    .map(|(path, navigation)| (entry.id, path, navigation.is_some()))
            })
            .map_or_else(View::empty, |(id, path, can_open)| {
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new()
                        .text(format!("Working directory · {}", path.display()))
                        .text_wrapping(TextWrapping::Wrap)
                        .is_text_selection_enabled(true),
                    Button::new()
                        .is_enabled(can_open)
                        .on_click(ctx.callback(move |_| {
                            action(HistoryAction::OpenDirectory((generation, id)))
                        }))
                        .content("Open command directory"),
                ))
            });
        let timing = selected.map_or_else(View::empty, |entry| {
            TextBlock::new()
                .automation_id("command-timing")
                .text(timing_label(entry))
                .text_wrapping(TextWrapping::Wrap)
                .into()
        });
        let comparison = self.history_reference_view(pane, generation, selected, ctx);
        let save = match (&page.save, save_target) {
            (Some(draft), Some(target)) if draft.target == target => saved_commands::name_form(
                &draft.name,
                ctx.callback(move |name| action(HistoryAction::SaveName(target, name))),
                ctx.callback(move |_| action(HistoryAction::Save(target))),
                ctx.callback(move |_| action(HistoryAction::CancelSave(target))),
            ),
            (_, target) => Button::new()
                .is_enabled(target.is_some())
                .on_click(ctx.callback(move |_| {
                    target.map_or(Message::ControlIdle, |target| {
                        action(HistoryAction::BeginSave(target))
                    })
                }))
                .content("Save as command…"),
        };
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .column_spacing(12.0)
            .rows([
                GridLength::Auto,
                GridLength::Auto,
                GridLength::Auto,
                GridLength::STAR,
            ])
            .columns([GridLength::Star(1.0), GridLength::Star(2.0)])
            .children((
                page_heading,
                StackPanel::new()
                    .grid_row(1)
                    .grid_column_span(2)
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .on_click(ctx.callback(move |_| action(HistoryAction::Refresh)))
                            .content("Refresh history"),
                        copy_button("Copy command", CopyPart::Command),
                        copy_button("Copy output", CopyPart::Output),
                        Button::new()
                            .on_click(ctx.callback(move |_| action(HistoryAction::Return)))
                            .content("Return to pane"),
                        TextBlock::new().text(&page.feedback),
                    )),
                StackPanel::new().grid_row(2).spacing(4.0).children((
                    TextBox::new()
                        .automation_name("Search command history")
                        .placeholder_text("Search command history")
                        .text(&page.search.query)
                        .on_text_changed(
                            ctx.callback(move |text| action(HistoryAction::Search(text))),
                        ),
                    CheckBox::new()
                        .automation_name("Include command output")
                        .is_checked(page.search.include_output)
                        .on_is_checked_changed(
                            ctx.callback(move |checked| {
                                action(HistoryAction::IncludeOutput(checked))
                            }),
                        )
                        .content("Include command output"),
                    ComboBox::new()
                        .automation_name("Command status")
                        .items_source(STATUS_OPTIONS.map(|(_, label)| label))
                        .selected_index(
                            STATUS_OPTIONS
                                .iter()
                                .position(|(status, _)| *status == page.status),
                        )
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                            index
                                .and_then(|index| STATUS_OPTIONS.get(index))
                                .map_or(Message::ControlIdle, |&(status, _)| {
                                    action(HistoryAction::Status(status))
                                })
                        })),
                    ComboBox::new()
                        .automation_name("Command order")
                        .items_source(ORDER_OPTIONS.map(|(_, label)| label))
                        .selected_index(
                            ORDER_OPTIONS
                                .iter()
                                .position(|(order, _)| *order == page.order),
                        )
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                            index
                                .and_then(|index| ORDER_OPTIONS.get(index))
                                .map_or(Message::ControlIdle, |&(order, _)| {
                                    action(HistoryAction::Order(order))
                                })
                        })),
                )),
                ScrollViewer::new()
                    .grid_row(3)
                    .content(StackPanel::new().spacing(4.0).keyed_children(rows)),
                Grid::new()
                    .grid_row(2)
                    .grid_row_span(2)
                    .grid_column(1)
                    .keyed_children([KeyedView::new(
                        preview_key,
                        ScrollViewer::new().content(
                            StackPanel::new().spacing(8.0).children((
                                TextBlock::new()
                                    .font_size(18.0)
                                    .text(heading)
                                    .text_wrapping(TextWrapping::Wrap),
                                timing,
                                directory,
                                comparison,
                                Button::new()
                                    .is_enabled(insert_target.is_some())
                                    .on_click(ctx.callback(move |_| {
                                        insert_target.map_or(Message::ControlIdle, |target| {
                                            action(HistoryAction::Insert(target))
                                        })
                                    }))
                                    .content("Insert command"),
                                save,
                                Button::new()
                                    .is_enabled(export_target.is_some())
                                    .on_click(ctx.callback(move |_| {
                                        export_target.map_or(Message::ControlIdle, |target| {
                                            action(HistoryAction::ExportOutput(target))
                                        })
                                    }))
                                    .content(text_export::COMMAND_OUTPUT_LABEL),
                                Button::new()
                                    .is_enabled(show_target.is_some())
                                    .on_click(ctx.callback(move |_| {
                                        show_target.map_or(Message::ControlIdle, |target| {
                                            action(HistoryAction::ShowOutput(target))
                                        })
                                    }))
                                    .content("Show output in pane"),
                                TextBlock::new()
                                    .text(preview)
                                    .text_wrapping(TextWrapping::Wrap),
                            )),
                        ),
                    )]),
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slowest_order_keeps_the_status_filter_and_places_unknown_durations_last() {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let terminal = w11term::terminal::create(TerminalSize::default(), tx);
        let mut frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        frame.command_history = Arc::new(VecDeque::from(
            [
                (1, Some(45), Some(1)),
                (2, Some(0), Some(1)),
                (3, Some(45), Some(1)),
                (4, Some(900), Some(0)),
                (5, None, Some(1)),
            ]
            .map(|(id, duration_ms, exit_code)| {
                Arc::new(CompletedCommand {
                    id,
                    finished_at: std::time::Instant::now(),
                    timing: wezterm_term::CommandTiming {
                        duration_ms,
                        ..Default::default()
                    },
                    command: Some("timed command".into()),
                    reported_directory: None,
                    exit_code,
                    output: Ok(String::new()),
                })
            }),
        ));
        let search = HistorySearch::default();
        let ordered = |order| {
            history_matches(&frame, &search, CommandStatus::Failed, order)
                .into_iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ordered(HistoryOrder::Newest), [5, 3, 2, 1]);
        assert_eq!(ordered(HistoryOrder::Slowest), [3, 1, 2, 5]);
    }

    #[test]
    fn output_search_uses_literal_unicode_text_and_keeps_the_status_filter() {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let terminal = w11term::terminal::create(TerminalSize::default(), tx);
        let mut frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        let message = "Error [Ångström.*]";
        let long = format!("{}\r\n{message}\r\nafter", "界".repeat(4097));
        let entry = |id, output, exit_code| {
            Arc::new(CompletedCommand {
                id,
                finished_at: std::time::Instant::now(),
                timing: Default::default(),
                command: Some(format!("run {id}")),
                reported_directory: None,
                output,
                exit_code,
            })
        };
        frame.command_history = Arc::new(VecDeque::from([
            entry(1, Ok(long.clone()), Some(4)),
            entry(2, Ok("error ÅngströmZZ".into()), Some(0)),
            entry(3, Err("Error [Ångström.*]"), Some(8)),
        ]));
        let mut search = HistorySearch {
            query: "[ångstrÖm.*]".into(),
            needle: "[ångström.*]".into(),
            ..Default::default()
        };
        assert_eq!(
            first_matching(&frame, &search, CommandStatus::All, HistoryOrder::Newest),
            None
        );

        search.include_output = true;
        search.update_matcher();
        assert_eq!(
            first_matching(&frame, &search, CommandStatus::Failed, HistoryOrder::Newest),
            Some((0, 1))
        );
        assert_eq!(
            first_matching(
                &frame,
                &search,
                CommandStatus::Succeeded,
                HistoryOrder::Newest
            ),
            None
        );
        assert!(search.output_match(&frame, 3).is_none());
        let hit = search.output_match(&frame, 1).unwrap();
        assert_eq!(&long[hit.clone()], "[Ångström.*]");
        assert!(!output_preview(&long, None).contains(message));
        assert!(output_preview(&long, Some(hit)).contains(message));

        // A new snapshot may reuse numeric IDs after a connection change.
        let old_history = Arc::downgrade(&frame.command_history);
        frame.command_history = Arc::new(VecDeque::from([entry(1, Ok("new".into()), Some(4))]));
        assert!(old_history.upgrade().is_none());
        assert!(search.output_match(&frame, 1).is_none());
    }

    #[test]
    fn command_search_uses_full_captured_input_and_keeps_output_separate() {
        let command = format!("{} [Ångström]\r\n  --final", "x".repeat(130));
        let mut entry = CompletedCommand {
            id: 9,
            finished_at: std::time::Instant::now(),
            timing: Default::default(),
            command: Some(command.clone()),
            reported_directory: None,
            output: Ok("archived result\r\n".into()),
            exit_code: Some(17),
        };
        assert!(command_text_matches(&entry, "[ångström]"));
        assert!(!command_text_matches(&entry, "archived"));
        assert!(!command_text_matches(&entry, ".*"));
        assert_eq!(CopyPart::Command.text(&entry), Some(command.as_str()));
        assert_eq!(CopyPart::Output.text(&entry), Some("archived result\r\n"));
        entry.command = None;
        assert!(command_text_matches(&entry, ""));
        assert!(!command_text_matches(&entry, "9"));
        assert_eq!(CopyPart::Command.text(&entry), None);
        entry.output = Err("Unavailable");
        assert_eq!(CopyPart::Output.text(&entry), None);
    }
}
