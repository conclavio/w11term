use super::*;
use catalog::{COMMON, Key, LINUX, Loaded, PLATFORMS, PageSource, Platform, WINDOWS};
use w11term::events::{ToolBundleSyncReport, ToolBundleSyncStatus, ToolPathSyncStatus};
use w11term::sftp::{InstalledTools, Job, Operation, TransferCancel};

mod catalog;
mod download;
mod private_page;
mod tool_install;

pub(super) const LABEL: &str = "Command reference";
pub(super) const SELECTION: &str = "Look up selected command";

pub enum Action {
    Open,
    Selection(Option<clipboard_paste::Origin>),
    Close,
    Query(String),
    Platform(Option<usize>),
    Select(usize),
    Edit(clipboard_paste::Origin, String),
    Draft(String),
    CancelEdit,
    Copy(String),
    Insert(clipboard_paste::Origin),
    Link(String),
    Refresh,
    Loaded(Result<Loaded, String>),
    ListTools(ToolPane),
    ListedTools(Arc<()>, Result<InstalledTools, String>),
    ToolPage(Arc<()>, usize),
    Private(private_page::Action),
    ToolInstall(tool_install::Action),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ToolPane {
    pane: u64,
    generation: u64,
}

fn run_pane_tool_job(
    input: tokio::sync::mpsc::UnboundedSender<Command>,
    target: ToolPane,
    operation: Operation,
    timeout: Duration,
    cancel: Option<TransferCancel>,
) -> Result<serde_json::Value, String> {
    let (reply, response) = tokio::sync::oneshot::channel();
    input
        .send(Command::Sftp(Job {
            expected_generation: Some(target.generation),
            operation,
            deadline: tokio::time::Instant::now() + timeout,
            reply,
            progress: None,
            cancel,
        }))
        .map_err(|_| "SSH session has closed".to_owned())?;
    response
        .blocking_recv()
        .map_err(|_| "SSH tool operation interrupted".to_owned())?
}

fn tool_sync_summary(report: &ToolBundleSyncReport) -> String {
    let status = match report.status {
        ToolBundleSyncStatus::Updated => "installed",
        ToolBundleSyncStatus::Current => "already current",
        ToolBundleSyncStatus::Held => "paused after rollback",
        ToolBundleSyncStatus::Skipped => "sync skipped",
        ToolBundleSyncStatus::Unconfirmed => "sync unconfirmed",
        ToolBundleSyncStatus::NotRun => "skipped for this session",
    };
    format!("Last login sync · {} {status}", report.bundle)
}

struct ToolLookup {
    pane: ToolPane,
    request: Arc<()>,
    cancel: TransferCancel,
    result: Option<Result<InstalledTools, String>>,
}

struct Draft {
    origin: clipboard_paste::Origin,
    text: String,
}

struct Search {
    query: String,
    exact: bool,
    platform: Platform,
    results: Vec<Key>,
}

#[derive(Default)]
pub(super) struct Panel {
    pub(super) open: bool,
    loading: bool,
    library: Option<Loaded>,
    error: Option<String>,
    query: String,
    exact: bool,
    platform: Option<Platform>,
    cached: RefCell<Option<Search>>,
    selected: usize,
    draft: Option<Draft>,
    query_field: ElementRef<TextBox>,
    draft_field: ElementRef<TextBox>,
    focus: u64,
    tools: Option<ToolLookup>,
    private_page: Option<private_page::Editor>,
    tool_install: Option<tool_install::Installer>,
}

fn pane_platform(source: Option<&Source>) -> Platform {
    match source {
        Some(Source::Ssh(_)) => LINUX,
        Some(Source::Local(local))
            if local.wsl_distribution().is_some()
                || local
                    .executable
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| stem.eq_ignore_ascii_case("wsl")) =>
        {
            LINUX
        }
        Some(Source::Local(_)) => WINDOWS,
        _ => COMMON,
    }
}

impl Panel {
    pub(super) fn close(&mut self) {
        self.open = false;
        self.draft = None;
        self.private_page = None;
        self.tool_install = None;
        if let Some(lookup) = self.tools.take() {
            lookup.cancel.cancel();
        }
    }

    fn set_query(&mut self, text: String) {
        let text = text.chars().take(128).filter(|c| !c.is_control()).collect();
        if self.query != text {
            self.query = text;
            self.exact = false;
            self.selected = 0;
            self.draft = None;
        }
    }

    fn results(&self, platform: Platform) -> std::cell::Ref<'_, [Key]> {
        let previous = self.cached.borrow();
        if previous.as_ref().is_none_or(|search| {
            search.query != self.query || search.platform != platform || search.exact != self.exact
        }) {
            drop(previous);
            let results = self
                .library
                .as_ref()
                .map(|loaded| {
                    if self.exact {
                        loaded
                            .catalog
                            .exact(&self.query, platform)
                            .into_iter()
                            .collect()
                    } else {
                        loaded
                            .catalog
                            .search(&self.query, platform)
                            .into_iter()
                            .map(|(key, _)| key.clone())
                            .collect()
                    }
                })
                .unwrap_or_default();
            self.cached.replace(Some(Search {
                query: self.query.clone(),
                exact: self.exact,
                platform,
                results,
            }));
        }
        std::cell::Ref::map(self.cached.borrow(), |cache| {
            cache.as_ref().unwrap().results.as_slice()
        })
    }

    fn page<'a>(&'a self, results: &[Key]) -> Option<(&'a catalog::Page, Platform)> {
        let key = results.get(self.selected).or_else(|| results.first())?;
        let page = self.library.as_ref()?.catalog.page(key)?;
        Some((page, key.platform))
    }

    fn private_page_target(&self, preferred: Platform) -> Option<(Key, bool)> {
        let library = self.library.as_ref().filter(|_| !self.loading)?;
        let name = catalog::query(&self.query);
        if !private_page::editable_name(&name) {
            return None;
        }
        let is_private = |key| {
            library
                .catalog
                .page(key)
                .is_some_and(|page| matches!(page.source, PageSource::Custom))
        };
        let results = self.results(preferred);
        let key = results
            .get(self.selected)
            .or_else(|| results.first())
            .filter(|key| key.name == name && is_private(key))
            .cloned()
            .unwrap_or(Key {
                platform: preferred,
                name,
            });
        let editing = is_private(&key);
        Some((key, editing))
    }

    fn view(
        &self,
        auto: Platform,
        origin: Option<clipboard_paste::Origin>,
        tool_pane: Option<ToolPane>,
        tool_sync: Option<&ToolBundleSyncReport>,
        ctx: &mut ViewContext<TerminalApp>,
    ) -> View {
        if let Some(editor) = &self.private_page {
            return editor.view(ctx);
        }
        if let Some(installer) = &self.tool_install {
            return installer.view(tool_pane, tool_sync, ctx);
        }
        let target = if self
            .draft
            .as_ref()
            .is_some_and(|draft| Some(draft.origin) == origin)
        {
            self.draft_field.clone()
        } else {
            self.query_field.clone()
        };
        ctx.use_effect("focus command reference", self.focus, move || {
            let _ = target.request_focus();
            None
        });
        let preferred = self.platform.unwrap_or(auto);
        let results = self.results(preferred);
        let mut pieces = vec![KeyedView::new(
            "status",
            TextBlock::new()
                .text(if self.loading {
                    "Loading TLDR pages…"
                } else {
                    self.error
                        .as_deref()
                        .or_else(|| self.library.as_ref().map(|library| library.status.as_str()))
                        .unwrap_or("Command reference unavailable")
                })
                .text_wrapping(TextWrapping::Wrap),
        )];
        if let Some(target) = tool_pane {
            let lookup = self.tools.as_ref().filter(|lookup| lookup.pane == target);
            pieces.push(KeyedView::new(
                "installed-tools",
                StackPanel::new().spacing(6.0).children((
                    Button::new()
                        .is_enabled(lookup.is_none_or(|lookup| lookup.result.is_some()))
                        .on_click(ctx.callback(move |_| {
                            Message::CommandReference(Action::ListTools(target))
                        }))
                        .content("Installed SSH tools"),
                    Button::new()
                        .on_click(ctx.callback(move |_| {
                            Message::CommandReference(Action::ToolInstall(
                                tool_install::Action::Open(target),
                            ))
                        }))
                        .content("Install SSH tools"),
                )),
            ));
            if let Some(report) = tool_sync {
                pieces.push(KeyedView::new(
                    "tool-login-sync",
                    TextBlock::new()
                        .text(tool_sync_summary(report))
                        .text_wrapping(TextWrapping::Wrap),
                ));
            }
            if self.query.is_empty()
                && let Some(lookup) = lookup
            {
                Self::tool_results(lookup, &mut pieces, ctx);
            }
        }
        if let Some(library) = &self.library {
            if results.is_empty() && !self.query.trim().is_empty() {
                pieces.push(KeyedView::new(
                    "empty",
                    TextBlock::new().text("No matching command pages"),
                ));
            }
            if results.len() > 1 {
                let result_labels = results.iter().filter_map(|key| {
                    let page = library.catalog.page(key)?;
                    Some(format!("{} · {}", page.title, key.platform.1))
                });
                pieces.push(KeyedView::new(
                    "matches",
                    ComboBox::new()
                        .automation_name("Matching commands")
                        .items_source(result_labels)
                        .selected_index(self.selected.min(results.len() - 1))
                        .on_selection_changed(ctx.callback(|index: Option<usize>| {
                            index.map_or(Message::ControlIdle, |index| {
                                Message::CommandReference(Action::Select(index))
                            })
                        })),
                ));
            }
        }
        if let Some((_, editing)) = self.private_page_target(preferred) {
            pieces.push(KeyedView::new(
                "author-private-page",
                Button::new()
                    .on_click(ctx.callback(|_| {
                        Message::CommandReference(Action::Private(private_page::Action::Open))
                    }))
                    .content(if editing {
                        "Edit private page"
                    } else {
                        "New private page"
                    }),
            ));
        }
        if let Some((page, platform)) = self.page(&results) {
            pieces.extend([
                KeyedView::new("title", TextBlock::new().font_size(22.0).text(&page.title)),
                KeyedView::new(
                    "platform",
                    TextBlock::new().text(format!("{} · {}", platform.1, page.source.label())),
                ),
                KeyedView::new(
                    "description",
                    TextBlock::new()
                        .text(&page.description)
                        .text_wrapping(TextWrapping::Wrap),
                ),
            ]);
            if let Some(link) = &page.link {
                let url = link.clone();
                pieces.push(KeyedView::new(
                    "link",
                    HyperlinkButton::new()
                        .automation_name(format!("Documentation for {}", page.title))
                        .on_click(ctx.callback(move |_| {
                            Message::CommandReference(Action::Link(url.clone()))
                        }))
                        .content(link.as_str()),
                ));
            }
            if let Some(draft) = &self.draft {
                let insert = draft.origin;
                let available = Some(insert) == origin && catalog::insertable(&draft.text);
                pieces.push(KeyedView::new(
                    "draft",
                    StackPanel::new().spacing(6.0).children((
                        TextBox::new()
                            .element_ref(&self.draft_field)
                            .automation_name("Edit command example")
                            .text(&draft.text)
                            .on_text_changed(
                                ctx.callback(|text| Message::CommandReference(Action::Draft(text))),
                            ),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                Button::new()
                                    .is_enabled(available)
                                    .on_click(ctx.callback(move |_| {
                                        Message::CommandReference(Action::Insert(insert))
                                    }))
                                    .content("Insert into terminal"),
                                Button::new()
                                    .on_click(ctx.callback(|_| {
                                        Message::CommandReference(Action::CancelEdit)
                                    }))
                                    .content("Cancel edit"),
                            )),
                    )),
                ));
            }
            for (index, example) in page.examples.iter().enumerate() {
                let copy = example.command.clone();
                let edit = example.command.clone();
                pieces.push(KeyedView::new(
                    format!("example-{index}"),
                    StackPanel::new().spacing(4.0).children((
                        TextBlock::new()
                            .text(&example.description)
                            .text_wrapping(TextWrapping::Wrap),
                        TextBlock::new()
                            .text(&example.command)
                            .text_wrapping(TextWrapping::Wrap),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                Button::new()
                                    .automation_name(format!("Copy example {}", index + 1))
                                    .on_click(ctx.callback(move |_| {
                                        Message::CommandReference(Action::Copy(copy.clone()))
                                    }))
                                    .content("Copy"),
                                Button::new()
                                    .automation_name(format!("Edit example {}", index + 1))
                                    .is_enabled(origin.is_some())
                                    .on_click(ctx.callback(move |_| {
                                        origin.map_or(Message::ControlIdle, |origin| {
                                            Message::CommandReference(Action::Edit(
                                                origin,
                                                edit.clone(),
                                            ))
                                        })
                                    }))
                                    .content("Edit example"),
                            )),
                    )),
                ));
            }
        }
        let platforms = std::iter::once(format!("Auto ({})", auto.1))
            .chain(PLATFORMS.iter().map(|platform| platform.1.to_owned()));
        let override_index = self
            .platform
            .and_then(|platform| PLATFORMS.iter().position(|p| *p == platform))
            .map_or(0, |index| index + 1);
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .keyed_children([
                KeyedView::new(
                    "header",
                    Border::new().grid_row(0).padding(12.0).content(
                        StackPanel::new().spacing(8.0).children((
                            StackPanel::new()
                                .orientation(Orientation::Horizontal)
                                .spacing(8.0)
                                .children((
                                    TextBlock::new().font_size(18.0).text(LABEL),
                                    Button::new()
                                        .automation_name("Close command reference")
                                        .on_click(
                                            ctx.callback(|_| {
                                                Message::CommandReference(Action::Close)
                                            }),
                                        )
                                        .content("Close"),
                                )),
                            TextBox::new()
                                .element_ref(&self.query_field)
                                .automation_name("Search command reference")
                                .placeholder_text("Find a command")
                                .text(&self.query)
                                .on_text_changed(ctx.callback(|text| {
                                    Message::CommandReference(Action::Query(text))
                                })),
                            StackPanel::new()
                                .orientation(Orientation::Horizontal)
                                .spacing(8.0)
                                .children((
                                    ComboBox::new()
                                        .automation_name("Command reference platform")
                                        .items_source(platforms)
                                        .selected_index(override_index)
                                        .on_selection_changed(ctx.callback(|index| {
                                            Message::CommandReference(Action::Platform(index))
                                        })),
                                    Button::new()
                                        .is_enabled(!self.loading)
                                        .on_click(ctx.callback(|_| {
                                            Message::CommandReference(Action::Refresh)
                                        }))
                                        .content("Update pages"),
                                )),
                        )),
                    ),
                ),
                KeyedView::new(
                    "pages",
                    Border::new().grid_row(1).padding(12.0).content(
                        ScrollViewer::new()
                            .content(StackPanel::new().spacing(10.0).keyed_children(pieces)),
                    ),
                ),
            ])
    }

    fn tool_results(
        lookup: &ToolLookup,
        pieces: &mut Vec<KeyedView>,
        ctx: &ViewContext<TerminalApp>,
    ) {
        let status = match lookup.result.as_ref() {
            None => Some("Reading installed tools…".into()),
            Some(Err(error)) => Some(error.clone()),
            Some(Ok(result)) => {
                if !result.tools.is_empty() {
                    let request = lookup.request.clone();
                    pieces.push(KeyedView::new(
                        "installed-commands",
                        ComboBox::new()
                            .automation_name("Installed SSH commands")
                            .placeholder_text("Choose a command to look up")
                            .items_source(
                                result
                                    .tools
                                    .iter()
                                    .map(|tool| format!("{} · {}", tool.name, tool.bundle)),
                            )
                            .selected_index(None)
                            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                                index.map_or(Message::ControlIdle, |index| {
                                    Message::CommandReference(Action::ToolPage(
                                        request.clone(),
                                        index,
                                    ))
                                })
                            })),
                    ));
                }
                if result.skipped_bundles != 0 {
                    Some(format!(
                        "Managed bundles skipped ({})",
                        result.skipped_bundles
                    ))
                } else {
                    result
                        .tools
                        .is_empty()
                        .then(|| "No active managed tools on this host".into())
                }
            }
        };
        if let Some(status) = status {
            pieces.push(KeyedView::new(
                "tools-status",
                TextBlock::new()
                    .text(status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
    }
}

impl TerminalApp {
    fn reference_tool_target(&self) -> Option<(ToolPane, &Session)> {
        let Page::Terminal(live) = &self.tabs.active()?.page else {
            return None;
        };
        let session = &live.session;
        if !matches!(live.source, Source::Ssh(_)) {
            return None;
        }
        let generation = session.ssh_connection_generation()?;
        Some((
            ToolPane {
                pane: session.pane_id,
                generation,
            },
            session,
        ))
    }

    fn reference_platform(&self) -> Platform {
        pane_platform(self.tabs.active().and_then(|tab| match &tab.page {
            Page::Terminal(live) => Some(&live.source),
            _ => None,
        }))
    }

    fn reference_origin(&self) -> Option<clipboard_paste::Origin> {
        let origin = self.saved_command_origin()?;
        let Page::Terminal(live) = &self.tabs.active()?.page else {
            return None;
        };
        (!live.session.frames.load().alternate
            && live
                .session
                .events
                .running_command_watch(origin.connection)
                .is_none())
        .then_some(origin)
    }

    pub(super) fn with_command_reference(
        &self,
        content: View,
        ctx: &mut ViewContext<Self>,
    ) -> View {
        let width = (self.width * 0.38).clamp(245.0, 430.0);
        let tools = self.reference_tool_target();
        let tool_sync =
            tools.and_then(|(target, session)| session.events.tool_bundle_sync(target.generation));
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Pixel(width)])
            .keyed_children([
                KeyedView::new("terminal", Border::new().grid_column(0).content(content)),
                KeyedView::new(
                    "reference",
                    Border::new()
                        .grid_column(1)
                        .background(Color::rgb(35, 35, 35))
                        .content(self.command_reference.view(
                            self.reference_platform(),
                            self.reference_origin(),
                            tools.map(|(target, _)| target),
                            tool_sync.as_deref(),
                            ctx,
                        )),
                ),
            ])
    }

    fn reference_selection(&self, origin: clipboard_paste::Origin) -> Option<String> {
        let tab = self.pane_paste_target(self.tabs.active_id()?, origin)?;
        let frame = tab.session.frames.load();
        let selection = self
            .copy_mode
            .borrow()
            .as_ref()
            .and_then(|mode| {
                (Some(mode.pane) == self.tabs.active_id() && mode.state.valid(&frame))
                    .then(|| mode.state.selection())
                    .flatten()
            })
            .or_else(|| tab.selection.borrow().clone())?;
        if !selection.can_copy(&frame) {
            return None;
        }
        let text = selection.text(&frame, true);
        let text = text.trim();
        (!text.is_empty() && text.len() <= 128 && !text.chars().any(char::is_control))
            .then(|| text.to_owned())
    }

    fn start_reference_load(&mut self, refresh: bool, ctx: &ComponentContext<Self>) {
        let panel = &mut self.command_reference;
        if panel.loading || panel.private_page.is_some() || panel.tool_install.is_some() {
            return;
        }
        panel.loading = true;
        panel.error = None;
        ctx.spawn_background_with_rejection(
            move |cancel| {
                let result = if refresh {
                    download::latest(|| cancel.is_cancelled()).and_then(catalog::update)
                } else {
                    catalog::load()
                };
                Message::CommandReference(Action::Loaded(result.map_err(|err| format!("{err:#}"))))
            },
            Message::CommandReference(Action::Loaded(Err(
                "Command reference worker unavailable".into()
            ))),
        );
    }

    fn start_tool_lookup(&mut self, target: ToolPane, ctx: &ComponentContext<Self>) {
        let Some((current, session)) = self.reference_tool_target() else {
            return;
        };
        if current != target || !self.command_reference.open {
            return;
        }
        let input = session.input.clone();
        let panel = &mut self.command_reference;
        if panel
            .tools
            .as_ref()
            .is_some_and(|lookup| lookup.pane == current && lookup.result.is_none())
        {
            return;
        }
        if let Some(previous) = panel.tools.take() {
            previous.cancel.cancel();
        }
        let request = Arc::new(());
        let rejected = Message::CommandReference(Action::ListedTools(
            request.clone(),
            Err("Installed tools worker unavailable".into()),
        ));
        let cancel = TransferCancel::default();
        panel.tools = Some(ToolLookup {
            pane: target,
            request: request.clone(),
            cancel: cancel.clone(),
            result: None,
        });
        panel.set_query(String::new());
        ctx.spawn_background_with_rejection(
            move |scope| {
                let result = (|| -> Result<InstalledTools, String> {
                    if scope.is_cancelled() || cancel.is_cancelled() {
                        return Err("Installed tools lookup cancelled".into());
                    }
                    let value = run_pane_tool_job(
                        input,
                        target,
                        Operation::InstalledTools,
                        Duration::from_secs(12),
                        Some(cancel),
                    )?;
                    serde_json::from_value(value).map_err(|error| error.to_string())
                })();
                Message::CommandReference(Action::ListedTools(request, result))
            },
            rejected,
        );
    }

    pub(super) fn command_reference_action(
        &mut self,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        let origin = self.reference_origin();
        let auto = self.reference_platform();
        let tool_pane = self.reference_tool_target().map(|(target, _)| target);
        let selected = match action {
            Action::Selection(target) => target
                .or_else(|| self.paste_origin())
                .and_then(|target| self.reference_selection(target)),
            _ => None,
        };
        let panel = &mut self.command_reference;
        match action {
            Action::Open | Action::Selection(_) => {
                self.triage.open = false;
                self.flame_graph.open = false;
                if panel.private_page.is_some() || panel.tool_install.is_some() {
                    return;
                }
                let was_open = panel.open;
                panel.open = true;
                panel.draft = None;
                panel.focus = panel.focus.wrapping_add(1);
                if let Some(text) = selected {
                    panel.set_query(text);
                }
                if !was_open || panel.library.is_none() {
                    self.start_reference_load(false, ctx);
                }
            }
            Action::Close => panel.close(),
            Action::Query(text) => panel.set_query(text),
            Action::Platform(Some(index)) => {
                let platform = PLATFORMS.get(index.wrapping_sub(1)).copied();
                if panel.platform != platform {
                    panel.platform = platform;
                    panel.selected = 0;
                    panel.draft = None;
                }
            }
            Action::Platform(None) => {}
            Action::Select(index) => {
                if panel.selected != index {
                    panel.selected = index;
                    panel.draft = None;
                }
            }
            Action::Edit(target, text) => {
                if Some(target) == origin {
                    panel.draft = Some(Draft {
                        origin: target,
                        text,
                    });
                    panel.focus = panel.focus.wrapping_add(1);
                }
            }
            Action::Copy(text) => {
                panel.error = w11term::clipboard::write(&text)
                    .err()
                    .map(|err| format!("Copying the command example: {err:#}"));
            }
            Action::Draft(text) => {
                if let Some(draft) = &mut panel.draft {
                    draft.text = text.chars().take(4096).collect();
                }
            }
            Action::CancelEdit => panel.draft = None,
            Action::Insert(target) => {
                if Some(target) == origin
                    && let Some(draft) = &panel.draft
                    && draft.origin == target
                    && catalog::insertable(&draft.text)
                {
                    let text = draft.text.clone();
                    panel.draft = None;
                    self.paste_saved_text(target, text, ctx);
                }
            }
            Action::Link(link) => Self::open_link(&link),
            Action::Refresh => self.start_reference_load(true, ctx),
            Action::Private(action) => panel.author_private_page(action, auto, ctx),
            Action::ToolInstall(action) => self.tool_install_action(action, ctx),
            Action::ListTools(target) => self.start_tool_lookup(target, ctx),
            Action::ListedTools(request, result) => {
                if let Some(lookup) = &mut panel.tools
                    && Arc::ptr_eq(&lookup.request, &request)
                {
                    lookup.result = Some(result);
                }
            }
            Action::ToolPage(request, index) => {
                if let Some(lookup) = panel.tools.as_ref()
                    && Some(lookup.pane) == tool_pane
                    && Arc::ptr_eq(&lookup.request, &request)
                    && panel.query.is_empty()
                    && let Some(Ok(tools)) = &lookup.result
                    && let Some(tool) = tools.tools.get(index)
                {
                    panel.set_query(tool.name.clone());
                    panel.exact = true;
                    panel.platform = None;
                    panel.focus = panel.focus.wrapping_add(1);
                }
            }
            Action::Loaded(result) => {
                panel.loading = false;
                match result {
                    Ok(library) => {
                        panel.library = Some(library);
                        panel.cached.replace(None);
                        panel.selected = 0;
                        panel.draft = None;
                    }
                    Err(err) => {
                        panel.error = Some(if panel.library.is_some() {
                            format!("Keeping the current pages. {err}")
                        } else {
                            err
                        });
                    }
                }
            }
        }
    }
}
