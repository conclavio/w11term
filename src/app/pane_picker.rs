use super::*;
use w11term::terminal::{BookmarkEntry, BookmarkScreen, FrameStore, Snapshot};
mod text_search;
pub use text_search::Action as TextAction;
pub(super) const FIND_TEXT_LABEL: &str = "Find in all panes…";

struct Preview {
    pane: u64,
    frames: FrameStore,
    frame: Rc<RefCell<Arc<Snapshot>>>,
    summary: String,
    invalidator: Invalidator,
    canvas: Option<View>,
    watch: Option<ComponentTask>,
}

impl Preview {
    fn new(pane: u64, frames: FrameStore, appearance: &Appearance, font_size: Option<f32>) -> Self {
        let snapshot = frames.load();
        let summary = preview_summary(&snapshot);
        let frame = Rc::new(RefCell::new(snapshot));
        let invalidator = Invalidator::default();
        let canvas = Renderer::with_size(
            &appearance.font_family,
            appearance.ligatures,
            appearance.terminal_font_size_for(font_size),
        )
        .map(|renderer| {
            let renderer = RefCell::new(renderer);
            let frame = Rc::clone(&frame);
            Canvas::invalidated(&invalidator, move |ctx| {
                renderer.borrow_mut().draw_preview(
                    ctx,
                    ctx.width,
                    ctx.height,
                    ctx.device_changed(),
                    &frame.borrow(),
                )
            })
            .on_error(|error| log::warn!("Drawing pane preview: {error:?}"))
            .into()
        })
        .inspect_err(|error| log::warn!("Creating pane preview: {error}"))
        .ok();
        Self {
            pane,
            frames,
            frame,
            summary,
            invalidator,
            canvas,
            watch: None,
        }
    }

    fn watch(&mut self, ctx: &ComponentContext<TerminalApp>) -> bool {
        if self
            .watch
            .as_ref()
            .is_none_or(|task| task.status() == ComponentTaskStatus::Rejected)
        {
            let pane = self.pane;
            let generation = self.frame.borrow().generation;
            let frames = self.frames.clone();
            self.watch = Some(ctx.spawn_background_with_rejection(
                move |cancel| {
                    frames.wait_for_change(generation, || cancel.is_cancelled());
                    Message::PickerFrame(pane, generation)
                },
                Message::WatchRejected,
            ));
        }
        self.watch
            .as_ref()
            .is_some_and(|task| task.status() != ComponentTaskStatus::Rejected)
    }

    fn refresh(&mut self, pane: u64, generation: u64, ctx: &ComponentContext<TerminalApp>) {
        if self.pane != pane || self.frame.borrow().generation != generation {
            return;
        }
        let latest = self.frames.load();
        if latest.generation != generation {
            self.summary = preview_summary(&latest);
            self.frame.replace(latest);
            self.invalidator.invalidate();
        }
        self.watch.take();
        self.watch(ctx);
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        if let Some(watch) = self.watch.take() {
            watch.cancel();
        }
    }
}

// Sample nearby rows for the screen reader. The canvas shows the whole grid.
fn preview_summary(frame: &Snapshot) -> String {
    const MAX_COLUMNS: usize = 1024;
    const MAX_CHARS: usize = 120;
    let Some(last_row) = frame.rows.len().checked_sub(1) else {
        return String::new();
    };
    let cursor_row = usize::try_from(frame.cursor.y)
        .ok()
        .and_then(|y| y.checked_add(frame.scroll_offset))
        .filter(|&y| y <= last_row);
    let anchor = cursor_row.unwrap_or(last_row);
    let nearby = (anchor.saturating_sub(6)..=anchor)
        .rev()
        .chain(anchor + 1..=(anchor + 2).min(last_row));
    let mut lines = Vec::new();
    for row in nearby {
        // Line's cell iterator advances from the beginning. Bound that work as
        // well as the string, including an unusually long combining sequence.
        let start = if Some(row) == cursor_row {
            frame
                .cursor
                .x
                .min(MAX_COLUMNS)
                .saturating_sub(MAX_CHARS / 2)
        } else {
            0
        };
        let mut sample = String::new();
        let mut remaining = MAX_CHARS;
        for cell in frame.rows[row]
            .visible_cells()
            .take_while(|c| c.cell_index() < frame.cols.min(MAX_COLUMNS))
            .skip_while(|c| c.cell_index() < start)
        {
            for ch in cell.str().chars().take(remaining) {
                sample.push(ch);
                remaining -= 1;
            }
            if remaining == 0 {
                break;
            }
        }
        let text = sample.trim();
        if !text.is_empty() {
            lines.push((row, text.to_owned()));
            if lines.len() == 2 {
                break;
            }
        }
    }
    lines.sort_by_key(|(row, _)| *row);
    lines
        .into_iter()
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join(" / ")
}

struct Choice {
    read_only: bool,
    pane: u64,
    pane_name: Option<String>,
    tab_title: Option<String>,
    title: String,
    label: String,
    profile: Option<String>,
    destination: String,
    tab_number: usize,
    pane_number: usize,
}

impl Choice {
    fn rank(&self, query: &str) -> Option<u8> {
        if query.is_empty() {
            return Some(0);
        }
        [&self.title, &self.label, &self.destination]
            .into_iter()
            .chain(self.pane_name.iter())
            .chain(self.profile.iter())
            .chain(self.tab_title.iter())
            .filter_map(|field| {
                let field = field.to_lowercase();
                if field == query {
                    Some(0)
                } else if field.starts_with(query) {
                    Some(1)
                } else {
                    field.contains(query).then_some(2)
                }
            })
            .min()
    }

    fn caption(&self) -> String {
        // The engine uses this title until a process sets a title of its own.
        let title = if self.title.is_empty() || self.title == "wezterm" {
            &self.label
        } else {
            &self.title
        };
        let mut fields = vec![self.pane_name.as_deref().unwrap_or(title.as_str())];
        if self.read_only {
            fields.push("Read-only");
        }
        for text in std::iter::once(title)
            .chain(self.tab_title.iter())
            .chain(self.profile.iter())
            .chain(std::iter::once(&self.destination))
        {
            if !text.is_empty() && !fields.contains(&text.as_str()) {
                fields.push(text);
            }
        }
        format!(
            "{}  ·  Tab {} / pane {}",
            fields.join("  ·  "),
            self.tab_number,
            self.pane_number
        )
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    Commands(command_palette::Search),
    Focus,
    HostRoster(TabId),
    Text,
    Destination { source: TabId },
    Placement { source: TabId, target: TabId },
}

impl Mode {
    fn has_commands(self) -> bool {
        matches!(self, Self::Commands(_))
    }
}

pub(super) struct Picker {
    mode: Mode,
    query: String,
    launch: Arc<()>,
    input: ElementRef<TextBox>,
    default_side: ElementRef<Button>,
    matches: Vec<Choice>,
    commands: Vec<command_palette::Row>,
    bookmarks: Option<Result<Vec<BookmarkEntry>, &'static str>>,
    text: text_search::Results,
    selected: usize,
    preview: Option<Preview>,
}

impl Picker {
    pub(super) fn bookmark_results_expired(&self, frame: &Snapshot) -> bool {
        let Mode::Commands(command_palette::Search::Bookmarks(_, screen)) = self.mode else {
            return false;
        };
        !self.commands.is_empty()
            && (frame.bookmark_screen() != Some(screen)
                || self.commands.iter().any(|choice| {
                    matches!(
                        choice.id,
                        command_palette::CommandId::OpenScrollbackBookmark(_, _, row)
                        if frame.scrollback_bookmarks.binary_search(&row).is_err()
                    )
                }))
    }

    pub(super) fn bookmark_status(&self, app: &TerminalApp) -> Option<&'static str> {
        let Mode::Commands(command_palette::Search::Bookmarks(origin, screen)) = self.mode else {
            return None;
        };
        if app.bookmark_frame(origin, screen).is_none() {
            return Some("The pane changed. Reopen bookmark search.");
        }
        match &self.bookmarks {
            None => Some("Loading scrollback bookmarks…"),
            Some(Err(message)) => Some(message),
            Some(Ok(_)) => None,
        }
    }
    pub(super) fn placement(&self) -> Option<(TabId, TabId)> {
        match self.mode {
            Mode::Placement { source, target } => Some((source, target)),
            _ => None,
        }
    }

    fn filter(choices: Vec<Choice>, query: &str) -> Vec<Choice> {
        let needle = query.trim().to_lowercase();
        let mut matches: Vec<_> = choices
            .into_iter()
            .filter_map(|choice| Some((choice.rank(&needle)?, choice)))
            .collect();
        matches.sort_by_key(|(rank, _)| *rank);
        matches.into_iter().map(|(_, choice)| choice).collect()
    }

    pub(super) fn move_by(&mut self, amount: isize) {
        if self.placement().is_some() || self.text.revealing {
            return;
        }
        let count = if self.mode.has_commands() {
            self.commands.len()
        } else if self.mode == Mode::Text {
            self.text.rows.len()
        } else {
            self.matches.len()
        };
        self.selected = self
            .selected
            .saturating_add_signed(amount)
            .min(count.saturating_sub(1));
    }

    pub(super) fn view(&self, app: &TerminalApp, ctx: &ViewContext<TerminalApp>) -> View {
        if self.mode == Mode::Text {
            return self.text_view(app, ctx);
        }
        if let Mode::Commands(search) = self.mode {
            return command_palette::view(
                &self.commands,
                self.selected,
                &self.query,
                &self.input,
                search,
                app,
                ctx,
            );
        }
        match self.placement() {
            Some((source, target)) => self.placement_view(source, target, app.width, ctx),
            None => self.search_view(app, ctx),
        }
    }

    fn placement_view(
        &self,
        source: TabId,
        target: TabId,
        width: f64,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let caption = self
            .matches
            .get(self.selected)
            .map(Choice::caption)
            .unwrap_or_default();
        let side = |side, label, row, column| {
            let button = Button::new()
                .grid_row(row)
                .grid_column(column)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .automation_name(label)
                .on_click(ctx.callback(move |_| Message::PlacePane(source, target, side)));
            let button = if side == Direction::Right {
                button.element_ref(&self.default_side)
            } else {
                button
            };
            button.content(label)
        };
        let content = StackPanel::new().spacing(8.0).children((
            TextBlock::new()
                .text(format!("Place pane relative to {caption}"))
                .text_wrapping(TextWrapping::Wrap),
            TextBlock::new().text("Press an arrow key or choose a side."),
            Grid::new()
                .rows([GridLength::Auto, GridLength::Auto])
                .columns([GridLength::STAR, GridLength::STAR])
                .row_spacing(6.0)
                .column_spacing(6.0)
                .children((
                    side(Direction::Left, "Move left", 0, 0),
                    side(Direction::Right, "Move right", 0, 1),
                    side(Direction::Up, "Move above", 1, 0),
                    side(Direction::Down, "Move below", 1, 1),
                )),
            Button::new()
                .on_click(ctx.callback(|_| Message::Shortcut(Shortcut::PickerCancel)))
                .content("Cancel"),
        ));
        Self::overlay(content, (width - 32.0).clamp(1.0, 500.0))
    }

    fn search_view(&self, app: &TerminalApp, ctx: &ViewContext<TerminalApp>) -> View {
        let moving = matches!(self.mode, Mode::Destination { .. });
        let roster = matches!(self.mode, Mode::HostRoster(_));
        let wide = app.width >= 800.0;
        let preview_height = if wide {
            (app.window_height - 160.0).clamp(0.0, 300.0)
        } else {
            ((app.window_height - 240.0) * 0.45).clamp(0.0, 170.0)
        };
        let preview_space = if wide || self.preview.is_none() {
            0.0
        } else {
            preview_height + 30.0
        };
        let page_size =
            ((app.window_height - 145.0 - preview_space) / 42.0).clamp(1.0, 8.0) as usize;
        let first = self.selected / page_size * page_size;
        let search = Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((
                TextBox::new()
                    .element_ref(&self.input)
                    .text(&self.query)
                    .placeholder_text(if moving {
                        "Find a destination by pane name, title, profile or user@host"
                    } else if roster {
                        "Search shown SSH hosts by pane name, title, profile or user@host"
                    } else {
                        "Search by pane name, title, profile or user@host"
                    })
                    .automation_name(if moving {
                        "Find destination pane"
                    } else if roster {
                        "Find SSH host in group"
                    } else {
                        "Find pane"
                    })
                    .on_text_changed(ctx.callback(Message::PickerQuery)),
                Button::new()
                    .grid_column(1)
                    .style(ButtonStyle::Subtle)
                    .automation_name("Close pane picker")
                    .on_click(ctx.callback(|_| Message::Shortcut(Shortcut::PickerCancel)))
                    .content(TextBlock::new().text("×")),
            ));
        let mut children = Vec::new();
        for (i, row) in self.matches.iter().enumerate().skip(first).take(page_size) {
            let id = row.pane;
            let caption = row.caption();
            let button = Button::new()
                .automation_name(&caption)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .style(if i == self.selected {
                    ButtonStyle::Accent
                } else {
                    ButtonStyle::Subtle
                })
                .on_click(ctx.callback(move |_| Message::PickerChoose(id)))
                .content(TextBlock::new().text(caption));
            let mut hit = Border::new();
            if i != self.selected {
                hit = hit.on_pointer_moved(ctx.callback(move |_| Message::PickerHover(id)));
            }
            children.push(KeyedView::new(format!("pane-{id}"), hit.content(button)));
        }
        if self.matches.is_empty() {
            children.push(KeyedView::new(
                "empty",
                TextBlock::new().text(if moving && self.query.is_empty() {
                    "Open a terminal in another tab to move this pane."
                } else if roster {
                    "No matching SSH hosts in this roster"
                } else {
                    "No matching panes"
                }),
            ));
        } else if self.matches.len() > page_size {
            let last = (first + page_size).min(self.matches.len());
            children.push(KeyedView::new(
                "pages",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(first > 0)
                            .automation_name("Previous pane results")
                            .on_click(ctx.callback(move |_| {
                                Message::Shortcut(Shortcut::PickerMove(-(page_size as isize)))
                            }))
                            .content(TextBlock::new().text("‹")),
                        TextBlock::new()
                            .vertical_alignment(VerticalAlignment::Center)
                            .text(format!("{}–{last} of {}", first + 1, self.matches.len())),
                        Button::new()
                            .is_enabled(last < self.matches.len())
                            .automation_name("Next pane results")
                            .on_click(ctx.callback(move |_| {
                                Message::Shortcut(Shortcut::PickerMove(page_size as isize))
                            }))
                            .content(TextBlock::new().text("›")),
                    )),
            ));
        }
        let results = StackPanel::new().spacing(4.0).keyed_children(children);
        let body: View = match (&self.preview, self.matches.get(self.selected)) {
            (Some(preview), Some(row)) => {
                let label = format!("Preview of {}", row.caption());
                let visual: View = match &preview.canvas {
                    Some(canvas) if preview_height >= 48.0 => {
                        Border::new().height(preview_height).content(canvas.clone())
                    }
                    _ => TextBlock::new().text(&preview.summary).into(),
                };
                let preview = StackPanel::new()
                    .grid_column(if wide { 1 } else { 0 })
                    .grid_row(if wide { 0 } else { 1 })
                    .spacing(4.0)
                    .children((
                        TextBlock::new()
                            .text(&label)
                            .automation_name(format!("{label}. {}", preview.summary)),
                        visual,
                    ));
                if wide {
                    Grid::new()
                        .columns([GridLength::STAR, GridLength::STAR])
                        .column_spacing(12.0)
                        .children((results, preview))
                } else {
                    Grid::new()
                        .rows([GridLength::Auto, GridLength::Auto])
                        .row_spacing(6.0)
                        .children((results, preview))
                }
            }
            _ => results,
        };
        Self::overlay(
            StackPanel::new().spacing(6.0).children((search, body)),
            (app.width - 32.0).clamp(1.0, if wide { 1060.0 } else { 660.0 }),
        )
    }

    pub(super) fn overlay(content: View, width: f64) -> View {
        Border::new()
            .grid_row_span(2)
            .background(Color::argb(112, 0, 0, 0))
            .content(
                Border::new()
                    .vertical_alignment(VerticalAlignment::Top)
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .width(width)
                    .margin(16.0)
                    .padding(12.0)
                    .corner_radius(8.0)
                    .background(Color::rgb(40, 42, 48))
                    .content(content),
            )
    }
}

impl TerminalApp {
    fn picker_choices(&self, mode: Mode) -> Vec<Choice> {
        let excluded = match mode {
            Mode::Destination { source } => self.tabs.group_id(source),
            _ => None,
        };
        let roster: Option<BTreeMap<_, _>> = match mode {
            Mode::HostRoster(group) => Some(
                self.roster_picker_hosts(group)
                    .into_iter()
                    .enumerate()
                    .map(|(index, (pane, address))| (pane, (index, address)))
                    .collect(),
            ),
            _ => None,
        };
        let mut choices = vec![];
        for (tab, header) in self.tabs.headers().into_iter().enumerate() {
            if excluded.is_some() && self.tabs.group_id(header.id) == excluded {
                continue;
            }
            if let Some(layout) = self.tabs.layout_for(header.id) {
                for (pane, leaf) in layout.leaves().into_iter().enumerate() {
                    if let Some(AppTab {
                        label,
                        page: Page::Terminal(live),
                    }) = self.tabs.get(leaf)
                    {
                        let pane_id = live.session.pane_id;
                        let destination = if let Some(roster) = &roster {
                            let Some((_, address)) = roster.get(&pane_id) else {
                                continue;
                            };
                            address.clone()
                        } else {
                            source_label(&live.source)
                        };
                        choices.push(Choice {
                            read_only: live.read_only.get(),
                            pane_name: live.pane_name.as_ref().map(|n| n.as_str().to_owned()),
                            tab_title: self.tabs.title_for(leaf).map(|t| t.as_str().to_owned()),
                            pane: pane_id,
                            label: label.clone(),
                            title: live.session.frames.load().title.clone(),
                            destination,
                            profile: live.profile_name.clone(),
                            tab_number: tab + 1,
                            pane_number: pane + 1,
                        });
                    }
                }
            }
        }
        if let Some(roster) = roster {
            choices.sort_by_key(|choice| roster[&choice.pane].0);
        }
        choices
    }

    pub(super) fn open_command_palette(&mut self, ctx: &ComponentContext<Self>) {
        if matches!(self.tabs.active().map(|t| &t.page), Some(Page::Terminal(live)) if live.prompt_id.is_some() || live.session.prompts.current().is_some() || live.device_busy)
        {
            if self.picker.is_none() {
                self.keyboard.set_picker(None);
            }
            return;
        }
        self.start_picker(Mode::Commands(command_palette::Search::Window), ctx);
    }

    pub(super) fn open_group_command_library(
        &mut self,
        group: TabId,
        ctx: &ComponentContext<Self>,
    ) {
        self.start_picker(
            Mode::Commands(command_palette::Search::GroupLibrary(group)),
            ctx,
        );
    }

    pub(super) fn open_tab_search(&mut self, ctx: &ComponentContext<Self>) {
        self.start_picker(Mode::Commands(command_palette::Search::Tabs), ctx);
    }

    pub(super) fn open_profile_search(&mut self, axis: Option<Axis>, ctx: &ComponentContext<Self>) {
        if self.picker.is_some() {
            return;
        }
        let destination = match axis {
            None => command_palette::ProfileDestination::Tab,
            Some(axis) => {
                let Some(origin) = self.move_source() else {
                    return;
                };
                command_palette::ProfileDestination::Split { axis, origin }
            }
        };
        self.start_picker(
            Mode::Commands(command_palette::Search::Profiles(destination)),
            ctx,
        );
        self.restore_picker_focus_after_menu(ctx);
    }

    pub(super) fn open_bookmark_search(
        &mut self,
        origin: clipboard_paste::Origin,
        screen: BookmarkScreen,
        ctx: &ComponentContext<Self>,
    ) {
        if self.picker.is_some() || self.bookmark_frame(origin, screen).is_none() {
            return;
        }
        let Some(Page::Terminal(tab)) = self.tabs.active().map(|tab| &tab.page) else {
            return;
        };
        let sender = tab.session.input.clone();
        self.start_picker(
            Mode::Commands(command_palette::Search::Bookmarks(origin, screen)),
            ctx,
        );
        let launch = self.picker.as_ref().unwrap().launch.clone();
        let (reply, receiver) = tokio::sync::oneshot::channel();
        if sender
            .send(Command::ReadScrollbackBookmarks { screen, reply })
            .is_err()
        {
            self.bookmarks_received(&launch, None);
            return;
        }
        let rejected = Message::BookmarksRead(launch.clone(), None);
        ctx.spawn_background_with_rejection(
            move |_| Message::BookmarksRead(launch, receiver.blocking_recv().ok().flatten()),
            rejected,
        );
    }

    pub(super) fn bookmarks_received(
        &mut self,
        launch: &Arc<()>,
        entries: Option<Vec<BookmarkEntry>>,
    ) {
        let Some(picker) = self
            .picker
            .as_mut()
            .filter(|picker| Arc::ptr_eq(&picker.launch, launch))
        else {
            return;
        };
        picker.bookmarks =
            Some(entries.ok_or("The bookmarks are unavailable. Reopen bookmark search."));
        self.refresh_open_commands();
    }

    fn bookmark_frame(
        &self,
        origin: clipboard_paste::Origin,
        screen: BookmarkScreen,
    ) -> Option<Arc<Snapshot>> {
        let Page::Terminal(tab) = &self.tabs.active()?.page else {
            return None;
        };
        let frame = tab.session.frames.load();
        (tab.session.pane_id == origin.pane
            && self.tabs.active_group_id() == Some(origin.group)
            && frame.connection_generation == origin.connection
            && frame.bookmark_screen() == Some(screen))
        .then_some(frame)
    }

    pub(super) fn reveal_bookmark(
        &self,
        origin: clipboard_paste::Origin,
        screen: BookmarkScreen,
        row: isize,
    ) {
        if self
            .bookmark_frame(origin, screen)
            .is_some_and(|frame| frame.scrollback_bookmarks.binary_search(&row).is_ok())
            && let Some(Page::Terminal(tab)) = self.tabs.active().map(|tab| &tab.page)
        {
            let _ = tab
                .session
                .input
                .send(Command::RevealScrollbackBookmark { screen, row });
        }
    }

    fn bookmark_rows(
        &self,
        origin: clipboard_paste::Origin,
        screen: BookmarkScreen,
        query: &str,
    ) -> Vec<command_palette::Row> {
        let Some(frame) = self.bookmark_frame(origin, screen) else {
            return Vec::new();
        };
        let Some(Ok(entries)) = self
            .picker
            .as_ref()
            .and_then(|picker| picker.bookmarks.as_ref())
        else {
            return Vec::new();
        };
        let query = query.trim().to_lowercase();
        let live = frame
            .first_stable_row
            .saturating_add_unsigned(frame.scroll_offset);
        entries
            .iter()
            .rev()
            .filter(|entry| frame.scrollback_bookmarks.binary_search(&entry.row).is_ok())
            .filter_map(|entry| {
                let label = if entry.excerpt.is_empty() {
                    "Blank line"
                } else {
                    &entry.excerpt
                };
                let chord = if entry.row < live {
                    format!("{} lines back", live.abs_diff(entry.row))
                } else {
                    format!("Screen row {}", live.abs_diff(entry.row) + 1)
                };
                let text = format!("{label} {chord}").to_lowercase();
                query
                    .split_whitespace()
                    .all(|word| text.contains(word))
                    .then(|| command_palette::Row {
                        id: command_palette::CommandId::OpenScrollbackBookmark(
                            origin, screen, entry.row,
                        ),
                        label: label.to_owned(),
                        chord,
                        enabled: true,
                    })
            })
            .collect()
    }

    pub(super) fn open_group_recent_commands(
        &mut self,
        group: TabId,
        ctx: &ComponentContext<Self>,
    ) {
        self.start_picker(
            Mode::Commands(command_palette::Search::GroupRecent(group)),
            ctx,
        );
    }

    pub(super) fn open_group_run_picker(&mut self, group: TabId, ctx: &ComponentContext<Self>) {
        self.start_picker(
            Mode::Commands(command_palette::Search::GroupRuns(group)),
            ctx,
        );
    }

    pub(super) fn open_window_group_run_picker(
        &mut self,
        scope: host_groups::WindowRunScope,
        ctx: &ComponentContext<Self>,
    ) {
        self.start_picker(
            Mode::Commands(command_palette::Search::WindowGroupRuns(scope)),
            ctx,
        );
        self.update_host_group_timer(ctx);
    }

    pub(super) fn window_group_run_picker_open(&self) -> bool {
        self.picker.as_ref().is_some_and(|picker| {
            matches!(
                picker.mode,
                Mode::Commands(command_palette::Search::WindowGroupRuns(_))
            )
        })
    }

    fn picker_command_rows(&self, mode: Mode, query: &str) -> Vec<command_palette::Row> {
        match mode {
            Mode::Commands(command_palette::Search::Tabs) => self.tab_search_rows(query),
            Mode::Commands(command_palette::Search::Profiles(destination)) => {
                if self.profile_error.is_some() {
                    Vec::new()
                } else {
                    command_palette::profile_search_rows(&self.catalog, destination, query)
                }
            }
            Mode::Commands(command_palette::Search::Bookmarks(origin, screen)) => {
                self.bookmark_rows(origin, screen, query)
            }
            Mode::Commands(command_palette::Search::GroupLibrary(group)) => {
                self.group_saved_command_rows(group, query)
            }
            Mode::Commands(command_palette::Search::GroupRecent(group)) => {
                self.group_recent_command_rows(group, query)
            }
            Mode::Commands(command_palette::Search::GroupRuns(group)) => {
                self.group_run_rows(group, query)
            }
            Mode::Commands(command_palette::Search::WindowGroupRuns(scope)) => {
                self.window_group_run_rows(scope, query)
            }
            _ => self.command_rows(query),
        }
    }

    pub(super) fn refresh_open_commands(&mut self) {
        let Some(picker) = self.picker.as_ref().filter(|p| p.mode.has_commands()) else {
            return;
        };
        let commands = self.picker_command_rows(picker.mode, &picker.query);
        let selected = picker
            .commands
            .get(picker.selected)
            .and_then(|previous| commands.iter().position(|row| row.id == previous.id))
            .unwrap_or_else(|| picker.selected.min(commands.len().saturating_sub(1)));
        let picker = self.picker.as_mut().unwrap();
        picker.commands = commands;
        picker.selected = selected;
    }

    pub(super) fn choose_command(
        &mut self,
        id: command_palette::CommandId,
        ctx: &ComponentContext<Self>,
    ) {
        let Some(picker) = &self.picker else { return };
        if !picker.mode.has_commands() || !picker.commands.iter().any(|r| r.id == id && r.enabled) {
            return;
        }
        // A profile, pane, selection, or remote connection can change while the palette is open.
        if matches!(picker.mode, Mode::Commands(search) if search.is_profiles()) {
            self.refresh_profiles();
        }
        let Some(picker) = &self.picker else { return };
        let rows = self.picker_command_rows(picker.mode, &picker.query);
        if rows.iter().any(|r| r.id == id && r.enabled) {
            self.close_picker(ctx);
            self.run_palette_command(id, ctx);
        } else if let Some(picker) = &mut self.picker {
            picker.commands = rows;
            picker.move_by(0);
        }
    }

    pub(super) fn open_picker(&mut self, ctx: &ComponentContext<Self>) {
        self.start_picker(Mode::Focus, ctx);
    }

    pub(super) fn open_roster_picker(&mut self, group: TabId, ctx: &ComponentContext<Self>) {
        if self.host_group_command_target() == Some(group) {
            self.start_picker(Mode::HostRoster(group), ctx);
        }
    }

    pub(super) fn refresh_roster_picker(&mut self, ctx: &ComponentContext<Self>) {
        let Some(picker) = self
            .picker
            .as_ref()
            .filter(|picker| matches!(picker.mode, Mode::HostRoster(_)))
        else {
            return;
        };
        let matches = Picker::filter(self.picker_choices(picker.mode), &picker.query);
        let selected = picker
            .matches
            .get(picker.selected)
            .and_then(|previous| matches.iter().position(|row| row.pane == previous.pane))
            .unwrap_or_else(|| picker.selected.min(matches.len().saturating_sub(1)));
        let picker = self.picker.as_mut().unwrap();
        picker.matches = matches;
        picker.selected = selected;
        self.sync_picker_preview(ctx);
    }

    pub(super) fn open_picker_from_menu(
        &mut self,
        shortcut: Shortcut,
        ctx: &ComponentContext<Self>,
    ) {
        if self.picker.is_some() {
            return;
        }
        self.update(Message::Shortcut(shortcut), ctx);
        self.restore_picker_focus_after_menu(ctx);
    }

    fn restore_picker_focus_after_menu(&self, ctx: &ComponentContext<Self>) {
        if let Some(picker) = &self.picker {
            let launch = picker.launch.clone();
            // WinUI's MenuFlyout restores focus to its owner when it closes.
            // Give the new editor a second chance after the close animation.
            ctx.spawn_background(move |_| {
                std::thread::sleep(Duration::from_millis(200));
                Message::PickerFocusAfterMenu(launch)
            });
        }
    }

    pub(super) fn focus_picker_after_menu(&self, launch: &Arc<()>) {
        if let Some(picker) = self.picker.as_ref().filter(|picker| {
            Arc::ptr_eq(&picker.launch, launch)
                && picker.placement().is_none()
                && picker.query.is_empty()
                && !picker.text.revealing
        }) {
            let _ = picker.input.request_focus();
        }
    }

    pub(super) fn open_text_picker(&mut self, ctx: &ComponentContext<Self>) {
        if matches!(self.tabs.active().map(|t| &t.page), Some(Page::Terminal(live)) if live.device_busy || live.session.prompts.current().is_some())
        {
            if self.picker.is_none() {
                self.keyboard.set_picker(None);
            }
            return;
        }
        self.start_picker(Mode::Text, ctx);
    }

    pub(super) fn open_move_picker(&mut self, ctx: &ComponentContext<Self>) {
        if let Some(source) = self.move_source() {
            self.start_picker(Mode::Destination { source }, ctx);
        } else if self.picker.is_none() {
            self.keyboard.set_picker(None);
        }
    }

    pub(super) fn move_source(&self) -> Option<TabId> {
        let id = self.tabs.active_id()?;
        let Page::Terminal(live) = &self.tabs.get(id)?.page else {
            return None;
        };
        (live.prompt_id.is_none() && live.session.prompts.current().is_none()).then_some(id)
    }

    fn start_picker(&mut self, mode: Mode, ctx: &ComponentContext<Self>) {
        self.close_copy_mode();
        self.close_hints();
        if self.picker.is_some() {
            return;
        }
        if self.switcher.is_some() {
            self.finish_switcher(false, ctx);
        }
        let (matches, commands) = if mode.has_commands() {
            if matches!(
                mode,
                Mode::Commands(
                    command_palette::Search::Window | command_palette::Search::Profiles(_)
                )
            ) {
                self.refresh_profiles();
            }
            if matches!(
                mode,
                Mode::Commands(
                    command_palette::Search::Window | command_palette::Search::GroupLibrary(_)
                )
            ) {
                self.reload_saved_commands();
            }
            (vec![], self.picker_command_rows(mode, ""))
        } else if mode == Mode::Text {
            (vec![], vec![])
        } else {
            (self.picker_choices(mode), vec![])
        };
        let active = self.tabs.active().and_then(AppTab::pane_id);
        let selected_tab = (mode == Mode::Commands(command_palette::Search::Tabs))
            .then(|| self.tabs.active_group_id())
            .flatten()
            .and_then(|group| {
                commands
                    .iter()
                    .position(|row| row.id == command_palette::CommandId::FocusTab(group))
            });
        let selected = selected_tab
            .or_else(|| matches.iter().position(|row| Some(row.pane) == active))
            .unwrap_or(0);
        self.picker = Some(Picker {
            mode,
            launch: Arc::new(()),
            input: ElementRef::new(),
            default_side: ElementRef::new(),
            query: String::new(),
            matches,
            commands,
            bookmarks: None,
            text: text_search::Results::default(),
            selected,
            preview: None,
        });
        self.sync_picker_preview(ctx);
        self.keyboard.set_picker(Some(if mode.has_commands() {
            PickerStage::Commands
        } else {
            PickerStage::Search
        }));
        self.focus_picker_input(0, ctx);
    }

    pub(super) fn focus_picker_input(&self, attempt: u8, ctx: &ComponentContext<Self>) {
        let Some(picker) = &self.picker else { return };
        let sender = ctx.sender();
        let retry = move |result: Result<bool, FocusError>| {
            if !result.unwrap_or(false) && attempt < 4 {
                sender.send(Message::PickerFocus(attempt + 1));
            }
        };
        let queued = if picker.placement().is_some() {
            picker.default_side.request_focus_result(retry)
        } else {
            picker.input.request_focus_result(retry)
        };
        if !queued && attempt < 4 {
            ctx.spawn_background(move |_| {
                std::thread::sleep(Duration::from_millis(20));
                Message::PickerFocus(attempt + 1)
            });
        }
    }

    pub(super) fn filter_picker(&mut self, query: String, ctx: &ComponentContext<Self>) {
        let Some(picker) = &self.picker else { return };
        if picker.placement().is_some() || picker.query == query || picker.mode == Mode::Text {
            return;
        }
        if picker.mode.has_commands() {
            let rows = self.picker_command_rows(picker.mode, &query);
            let picker = self.picker.as_mut().unwrap();
            picker.commands = rows;
            picker.query = query;
            picker.selected = 0;
            return;
        }
        let matches = Picker::filter(self.picker_choices(picker.mode), &query);
        if let Some(picker) = &mut self.picker {
            picker.query = query;
            picker.matches = matches;
            picker.selected = 0;
        }
        self.sync_picker_preview(ctx);
    }

    pub(super) fn hover_picker(&mut self, pane: u64, ctx: &ComponentContext<Self>) {
        if let Some(picker) = &mut self.picker
            && picker.placement().is_none()
            && let Some(index) = picker.matches.iter().position(|row| row.pane == pane)
            && index != picker.selected
        {
            picker.selected = index;
            self.sync_picker_preview(ctx);
        }
    }

    pub(super) fn refresh_picker_font(&mut self, ctx: &ComponentContext<Self>) {
        if let Some(picker) = &mut self.picker {
            picker.preview = None;
            self.sync_picker_preview(ctx);
        }
    }

    pub(super) fn sync_picker_preview(&mut self, ctx: &ComponentContext<Self>) {
        let Some(picker) = &self.picker else { return };
        if picker.placement().is_some() {
            return;
        }
        let selected = picker.matches.get(picker.selected).map(|row| row.pane);
        if selected == picker.preview.as_ref().map(|preview| preview.pane) {
            return;
        }
        let preview = selected.and_then(|pane| {
            let Page::Terminal(live) = &self.tabs.get(self.tab_for_pane(pane)?)?.page else {
                return None;
            };
            Some(Preview::new(
                pane,
                live.session.frames.clone(),
                &self.appearance.read().unwrap(),
                live.font_size_override(),
            ))
        });
        self.picker.as_mut().unwrap().preview = preview;
        self.watch_picker_preview(ctx);
    }

    pub(super) fn watch_picker_preview(&mut self, ctx: &ComponentContext<Self>) -> bool {
        self.picker
            .as_mut()
            .and_then(|picker| picker.preview.as_mut())
            .is_none_or(|p| p.watch(ctx))
    }

    pub(super) fn picker_frame(
        &mut self,
        pane: u64,
        generation: u64,
        ctx: &ComponentContext<Self>,
    ) {
        if let Some(preview) = self
            .picker
            .as_mut()
            .and_then(|picker| picker.preview.as_mut())
        {
            preview.refresh(pane, generation, ctx);
        }
    }

    pub(super) fn accept_picker(&mut self, ctx: &ComponentContext<Self>) {
        if let Some(picker) = &self.picker
            && picker.mode == Mode::Text
        {
            if let Some(action) = picker.text_choice(picker.selected) {
                self.pane_text_action(action, ctx);
            }
            return;
        }
        if let Some(picker) = &self.picker
            && picker.mode.has_commands()
        {
            if let Some(row) = picker.commands.get(picker.selected) {
                self.choose_command(row.id.clone(), ctx);
            }
            return;
        }
        if let Some(pane) = self
            .picker
            .as_ref()
            .and_then(|picker| picker.matches.get(picker.selected))
            .map(|row| row.pane)
        {
            self.choose_picker_pane(pane, ctx);
        }
    }

    pub(super) fn choose_picker_pane(&mut self, pane: u64, ctx: &ComponentContext<Self>) {
        if let Some(Mode::HostRoster(group)) = self.picker.as_ref().map(|picker| picker.mode)
            && !self
                .roster_picker_hosts(group)
                .iter()
                .any(|(id, _)| *id == pane)
        {
            self.refresh_roster_picker(ctx);
            return;
        }
        let id = self.tab_for_pane(pane);
        let Some(picker) = &mut self.picker else {
            return;
        };
        let Some(index) = picker.matches.iter().position(|row| row.pane == pane) else {
            return;
        };
        if picker.placement().is_some() {
            return;
        }
        let Some(id) = id else {
            // A pane can close while its previously rendered result is being chosen.
            picker.matches.retain(|row| row.pane != pane);
            picker.move_by(0);
            self.sync_picker_preview(ctx);
            return;
        };
        if let Mode::Destination { source } = picker.mode {
            if self.tabs.active_id() != Some(source)
                || self.tabs.group_id(source) == self.tabs.group_id(id)
            {
                self.close_picker(ctx);
                return;
            }
            picker.selected = index;
            picker.mode = Mode::Placement { source, target: id };
            picker.preview = None;
            self.keyboard.set_picker_placement();
            self.focus_picker_input(0, ctx);
            return;
        }
        self.tabs.select(id);
        if matches!(self.tabs.get(id), Some(AppTab { page: Page::Terminal(live), .. }) if live.find.open)
        {
            self.find(true, None, ctx);
        }
        self.sync_selection(ctx);
        self.close_picker(ctx);
    }

    pub(super) fn place_pane(
        &mut self,
        source: TabId,
        target: TabId,
        side: Direction,
        ctx: &ComponentContext<Self>,
    ) {
        if self.picker.as_ref().and_then(Picker::placement) != Some((source, target)) {
            return;
        }
        let source_was_single = matches!(
            self.tabs.active_layout(),
            Some(w11term::pane_layout::Layout::Leaf(_))
        );
        let old_group = self.tabs.group_id(source);
        if self.move_source() == Some(source)
            && self.tabs.get(target).and_then(AppTab::pane_id).is_some()
            && self.tabs.move_to_existing_tab(source, target, side)
        {
            if source_was_single {
                self.mru_groups.retain(|g| Some(*g) != old_group);
            }
            self.sync_selection(ctx);
        }
        self.close_picker(ctx);
    }

    pub(super) fn close_picker(&mut self, ctx: &ComponentContext<Self>) {
        // Once Enter commits a reveal, its acknowledgement owns the next focus change.
        if self.picker.as_ref().is_some_and(|p| !p.text.revealing) {
            // Searching and moving the highlight never changed the active tab or pane.
            self.activate(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spoken_preview_samples_the_cursor_neighborhood_on_large_sparse_screens() {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = w11term::terminal::create(
            TerminalSize {
                cols: 500,
                rows: 64,
                ..Default::default()
            },
            tx,
        );
        terminal.advance_bytes(b"old headline\x1b[60;1Hunrelated footer\x1b[12;1Hbuild completed\x1b[13;465Hhidden-marker-live");
        let snapshot = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        assert_eq!(
            preview_summary(&snapshot),
            "build completed / hidden-marker-live"
        );

        terminal.advance_bytes(b"\x1b[30;1H");
        let snapshot = Snapshot::capture(&terminal, None, 2, 0, String::new(), 0);
        assert_eq!(preview_summary(&snapshot), "");
    }
    fn row(pane: u64, title: &str, label: &str, destination: &str) -> Choice {
        Choice {
            read_only: false,
            pane,
            pane_name: None,
            tab_title: None,
            title: title.into(),
            label: label.into(),
            profile: Some(label.into()),
            destination: destination.into(),
            tab_number: 1,
            pane_number: 1,
        }
    }
    #[test]
    fn search_matches_titles_profiles_and_destinations_with_exact_matches_first() {
        let choices = || {
            vec![
                row(1, "logs", "dev20", "root@host:22"),
                row(2, "BÜILD LOG", "dev2", "pilot@game:6792"),
                row(3, "dev2 cockpit", "play", "pilot@game:6789"),
            ]
        };
        let ids = |query| {
            Picker::filter(choices(), query)
                .into_iter()
                .map(|row| row.pane)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("DEV2"), [2, 1, 3]);
        assert_eq!(ids(" üIlD "), [2]);
        assert_eq!(ids("PILOT@game"), [2, 3]);
        assert!(ids("missing").is_empty());
    }

    #[test]
    fn named_tab_matches_alongside_the_pane_title_and_profile() {
        let mut choice = row(7, "shell", "prod", "user@host");
        choice.tab_title = Some("Incident response".into());
        assert_eq!(choice.rank("incident response"), Some(0));
        assert_eq!(choice.rank("shell"), Some(0));
        assert!(choice.caption().contains("Incident response"));
    }

    #[test]
    fn individual_names_distinguish_identical_shells_and_keep_the_remote_title_searchable() {
        let mut first = row(7, "ops-shell", "prod", "user@host");
        let mut second = row(8, "ops-shell", "prod", "user@host");
        first.pane_name = Some("Déploy".into());
        second.pane_name = Some("Logs".into());
        for choice in [&mut first, &mut second] {
            choice.tab_title = Some("Operations".into());
        }
        assert!(first.caption().starts_with("Déploy  ·  ops-shell"));
        assert!(second.caption().starts_with("Logs  ·  ops-shell"));
        for choice in [&first, &second] {
            for query in ["ops-shell", "prod", "user@host", "operations"] {
                assert_eq!(choice.rank(query), Some(0));
            }
        }
        assert_eq!(Picker::filter(vec![second, first], "DÉPLOY")[0].pane, 7);

        let mut choice = row(9, "Logs", "Logs", "user@host");
        choice.pane_name = Some("Logs".into());
        assert_eq!(choice.caption().matches("Logs").count(), 1);
        choice.pane_name = None;
        choice.title = "wezterm".into();
        assert!(choice.caption().starts_with("Logs  ·  user@host"));
    }
}
