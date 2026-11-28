use super::*;
use catalog::{COMMON, Key, LINUX, Loaded, PLATFORMS, PageSource, Platform, WINDOWS};
use std::collections::VecDeque;
use w11term::command_reference as catalog;
use w11term::events::{
    DotfileSyncReport, ToolBundleSyncReport, ToolBundleSyncStatus, ToolPathSyncStatus,
};
use w11term::sftp::{InstalledTools, Job, Operation, TransferCancel};

mod command_line;
mod dotfiles;
mod download;
mod example;
mod private_page;
mod related;
mod saved_pages;
mod tool_install;
mod tool_removal;

pub use saved_pages::Lookup as SavedManualLookup;

pub(super) const LABEL: &str = "Command reference";
pub(super) const SELECTION: &str = "Look up selected command";

pub enum Action {
    Open,
    Selection(Option<clipboard_paste::Origin>),
    Close,
    Query(String),
    Platform(Option<usize>),
    Select(usize),
    Example(example::Action),
    Copy(String),
    Link(String),
    Refresh,
    Loaded(Result<Loaded, String>),
    ListTools(ToolPane),
    ListedTools(Arc<()>, Result<InstalledTools, String>),
    ToolPage(Arc<()>, usize),
    Private(private_page::Action),
    SavedPage(saved_pages::Action),
    Related(related::Action),
    ToolInstall(tool_install::Action),
    ToolRemoval(tool_removal::Action),
    Dotfiles(dotfiles::Action),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ToolPane {
    pane: u64,
    generation: u64,
}

fn run_pane_sftp_job(
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
        .map_err(|_| "SSH file operation interrupted".to_owned())?
}

fn tool_sync_summary(report: &ToolBundleSyncReport) -> String {
    let status = match report.status {
        ToolBundleSyncStatus::Updated => "installed",
        ToolBundleSyncStatus::Current => "already current",
        ToolBundleSyncStatus::Held => "automatic sync paused",
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

struct Search {
    query: String,
    mode: SearchMode,
    platform: Platform,
    results: Vec<Key>,
}

#[derive(Clone, Default, PartialEq, Eq)]
enum SearchMode {
    #[default]
    Search,
    PreferredExact,
    Page(Key),
}

type ControlReply = tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>;

struct ControlLookup {
    query: String,
    platform: Platform,
    reply: ControlReply,
}

#[derive(Default)]
pub(super) struct Panel {
    pub(super) open: bool,
    loading: bool,
    library: Option<Loaded>,
    error: Option<String>,
    query: String,
    mode: SearchMode,
    platform: Option<Platform>,
    cached: RefCell<Option<Search>>,
    selected: usize,
    draft: Option<example::Draft>,
    command_lookup: Option<command_line::Lookup>,
    control_lookup: Option<ControlLookup>,
    related_history: VecDeque<related::Location>,
    query_field: ElementRef<TextBox>,
    draft_field: ElementRef<TextBox>,
    draft_option: ElementRef<ComboBox>,
    focus: Option<u64>,
    tools: Option<ToolLookup>,
    private_page: Option<private_page::Editor>,
    saved_pages: saved_pages::State,
    tool_install: Option<tool_install::Installer>,
    tool_removal: Option<tool_removal::Panel>,
    dotfiles: Option<dotfiles::Installer>,
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
        self.cancel_control_lookup();
        self.open = false;
        self.draft = None;
        self.command_lookup = None;
        self.related_history.clear();
        self.private_page = None;
        self.tool_install = None;
        self.tool_removal = None;
        self.dotfiles = None;
        self.saved_pages.close();
        if let Some(lookup) = self.tools.take() {
            lookup.cancel.cancel();
        }
    }

    pub(super) fn subpanel_open(&self) -> bool {
        self.private_page.is_some()
            || self.tool_install.is_some()
            || self.tool_removal.is_some()
            || self.dotfiles.is_some()
    }

    fn set_query(&mut self, text: String) -> bool {
        let text = text.chars().take(128).filter(|c| !c.is_control()).collect();
        if self.query == text {
            return false;
        }
        self.query = text;
        self.command_lookup = None;
        self.mode = SearchMode::Search;
        self.selected = 0;
        self.draft = None;
        true
    }

    fn request_focus(&mut self) {
        self.focus = Some(self.focus.unwrap_or_default().wrapping_add(1));
    }

    fn cancel_control_lookup(&mut self) {
        if let Some(lookup) = self.control_lookup.take() {
            let _ = lookup.reply.send(Err(
                "Command reference changed before the manual opened".into()
            ));
        }
    }

    fn show_page(&mut self, key: Key, auto: Platform) {
        self.set_page(key, auto);
        self.request_focus();
    }

    fn set_page(&mut self, key: Key, auto: Platform) {
        self.command_lookup = None;
        self.related_history.clear();
        self.set_query(key.name.clone());
        if self.platform.unwrap_or(auto) != key.platform {
            self.platform = Some(key.platform);
        }
        self.mode = SearchMode::Page(key);
        self.selected = 0;
        self.draft = None;
    }

    fn resolve_command_lookup(&mut self) {
        let Some(lookup) = self.command_lookup.take() else {
            return;
        };
        let Some(key) = self
            .library
            .as_ref()
            .and_then(|library| lookup.resolve(&library.catalog))
        else {
            return;
        };
        self.set_query(key.name.clone());
        self.mode = SearchMode::Page(key);
        self.selected = 0;
    }

    fn results(&self, platform: Platform) -> std::cell::Ref<'_, [Key]> {
        let previous = self.cached.borrow();
        if previous.as_ref().is_none_or(|search| {
            search.query != self.query || search.platform != platform || search.mode != self.mode
        }) {
            drop(previous);
            let results = self
                .library
                .as_ref()
                .map(|loaded| match &self.mode {
                    SearchMode::Page(key) => loaded
                        .catalog
                        .page(key)
                        .map(|_| key.clone())
                        .into_iter()
                        .collect(),
                    SearchMode::PreferredExact => loaded
                        .catalog
                        .exact(&self.query, platform)
                        .into_iter()
                        .collect(),
                    SearchMode::Search => loaded
                        .catalog
                        .search(&self.query, platform)
                        .into_iter()
                        .map(|(key, _)| key.clone())
                        .collect(),
                })
                .unwrap_or_default();
            self.cached.replace(Some(Search {
                query: self.query.clone(),
                mode: self.mode.clone(),
                platform,
                results,
            }));
        }
        std::cell::Ref::map(self.cached.borrow(), |cache| {
            cache.as_ref().unwrap().results.as_slice()
        })
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
        dotfile_sync: Option<&DotfileSyncReport>,
        ctx: &mut ViewContext<TerminalApp>,
    ) -> View {
        if let Some(editor) = &self.private_page {
            return editor.view(ctx);
        }
        if let Some(installer) = &self.tool_install {
            return installer.view(tool_pane, tool_sync, ctx);
        }
        if let Some(removal) = &self.tool_removal {
            return removal.view(tool_pane, ctx);
        }
        if let Some(installer) = &self.dotfiles {
            return installer.view(tool_pane, dotfile_sync, ctx);
        }
        let preferred = self.platform.unwrap_or(auto);
        let results = self.results(preferred);
        let selected = results.get(self.selected).or_else(|| results.first());
        let draft = self
            .draft
            .as_ref()
            .filter(|draft| selected == Some(&draft.page));
        let target = if draft.is_some() {
            self.draft_field.clone()
        } else {
            self.query_field.clone()
        };
        let option = draft
            .filter(|draft| draft.focus_option())
            .map(|_| self.draft_option.clone());
        let focus_requested = self.focus.is_some();
        ctx.use_effect("focus command reference", self.focus, move || {
            if focus_requested {
                if let Some(option) = option {
                    let _ = option.request_focus();
                } else {
                    let _ = target.request_focus();
                }
            }
            None
        });
        let page = selected.and_then(|key| {
            let page = self.library.as_ref()?.catalog.page(key)?;
            Some((page, key.platform))
        });
        let lookup = self
            .tools
            .as_ref()
            .filter(|lookup| Some(lookup.pane) == tool_pane);
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
                .text_wrapping(TextWrapping::Wrap)
                .is_text_selection_enabled(true),
        )];
        pieces.push(self.saved_pages.section(self.library.as_ref(), ctx));
        if let Some(target) = tool_pane {
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
                                tool_install::Action::Open(target, None),
                            ))
                        }))
                        .content("Install SSH tools"),
                    Button::new()
                        .on_click(ctx.callback(move |_| {
                            Message::CommandReference(Action::Dotfiles(dotfiles::Action::Open(
                                target,
                            )))
                        }))
                        .content("Apply SSH dotfiles"),
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
            if let Some(report) = dotfile_sync {
                pieces.push(KeyedView::new(
                    "dotfile-login-sync",
                    TextBlock::new()
                        .text(dotfiles::login_summary(report))
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
        if !self.related_history.is_empty() {
            pieces.push(KeyedView::new(
                "related-back",
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .on_click(ctx.callback(|_| {
                        Message::CommandReference(Action::Related(related::Action::Back))
                    }))
                    .content("Back to previous manual"),
            ));
        }
        if let Some((page, platform)) = page {
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
            if let Some(key) = selected {
                pieces.push(self.saved_pages.button(key, ctx));
                if !page.related.is_empty() {
                    pieces.push(KeyedView::new(
                        "related-commands",
                        self.related_view(key, page, preferred, ctx),
                    ));
                }
            }
        }
        if let (Some(target), Some(key), Some(_)) = (tool_pane, selected, page)
            && matches!(key.platform, LINUX | COMMON)
            && w11term::sftp::builtin_tool_names().contains(&key.name)
        {
            let name = key.name.clone();
            let label = format!("Review installing {name}");
            pieces.push(KeyedView::new(
                "install-command",
                Button::new()
                    .on_click(ctx.callback(move |_| {
                        Message::CommandReference(Action::ToolInstall(tool_install::Action::Open(
                            target,
                            Some(name.clone()),
                        )))
                    }))
                    .content(label),
            ));
        }
        if let Some(Ok(installed)) = lookup.and_then(|lookup| lookup.result.as_ref()) {
            let command = match selected {
                Some(key) if matches!(key.platform, LINUX | COMMON) => Some(key.name.as_str()),
                None if self.mode == SearchMode::PreferredExact && preferred == LINUX => {
                    Some(self.query.as_str())
                }
                _ => None,
            };
            for tool in installed
                .tools
                .iter()
                .filter(|tool| Some(tool.name.as_str()) == command)
            {
                let bundle = tool.bundle.clone();
                let name = tool.name.clone();
                let label = format!("Review removing {name} from {bundle}");
                pieces.push(KeyedView::new(
                    format!("managed-launcher-{}", tool.bundle),
                    StackPanel::new().spacing(4.0).children((
                        TextBlock::new().text(format!("Managed SSH launcher · {}", tool.bundle)),
                        TextBlock::new()
                            .text(&tool.path)
                            .text_wrapping(TextWrapping::Wrap)
                            .is_text_selection_enabled(true),
                        Button::new()
                            .on_click(ctx.callback(move |_| {
                                Message::CommandReference(Action::ToolRemoval(
                                    tool_removal::Action::Open(bundle.clone(), name.clone()),
                                ))
                            }))
                            .content(label),
                    )),
                ));
            }
        }
        if let (Some((page, _)), Some(key)) = (page, selected) {
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
            if let Some(draft) = draft {
                pieces.push(KeyedView::new(
                    "draft",
                    draft.view(origin, &self.draft_field, &self.draft_option, ctx),
                ));
            }
            for (index, example) in page.examples.iter().enumerate() {
                let copy = example.command.clone();
                let edit = key.clone();
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
                                    .on_click(ctx.callback(move |_| {
                                        Message::CommandReference(Action::Example(
                                            example::Action::Edit(edit.clone(), index, origin),
                                        ))
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
                                .placeholder_text("Find a command or task")
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
    pub(super) fn captured_reference_query(
        &self,
        pane: u64,
        link: w11term::events::CommandLink,
    ) -> Option<command_line::Lookup> {
        let frame = self.history_frame(pane)?;
        let text = frame.command_for_link(link)?.command.as_deref()?;
        self.recorded_reference_query(pane, text)
    }

    pub(super) fn recorded_reference_query(
        &self,
        pane: u64,
        text: &str,
    ) -> Option<command_line::Lookup> {
        let Page::Terminal(tab) = &self.tabs.get(self.tab_for_pane(pane)?)?.page else {
            return None;
        };
        let platform = pane_platform(Some(&tab.source));
        command_line::Lookup::new(text, platform, platform)
            .filter(|_| !self.command_reference.subpanel_open())
    }

    pub(super) fn open_recorded_reference(
        &mut self,
        lookup: command_line::Lookup,
        ctx: &ComponentContext<Self>,
    ) {
        if self.command_reference.subpanel_open() {
            return;
        }
        self.set_diagnostics_drawer(None);
        let panel = &mut self.command_reference;
        panel.cancel_control_lookup();
        let reload = !panel.open || panel.library.is_none();
        panel.open = true;
        panel.draft = None;
        panel.saved_pages.close();
        panel.related_history.clear();
        panel.platform = Some(lookup.platform());
        panel.request_focus();
        panel.set_query(lookup.executable().to_owned());
        panel.mode = SearchMode::PreferredExact;
        panel.selected = 0;
        panel.command_lookup = Some(lookup);
        if reload {
            panel.saved_pages.refresh(ctx);
            self.start_reference_load(false, ctx);
        } else if !panel.loading {
            panel.resolve_command_lookup();
        }
    }

    pub(super) fn control_open_reference(
        &mut self,
        query: String,
        platform: Option<Platform>,
        reply: ControlReply,
        ctx: &ComponentContext<Self>,
    ) {
        if reply.is_closed() {
            return;
        }
        let platform = platform.unwrap_or_else(|| self.reference_platform());
        let panel = &mut self.command_reference;
        if panel.subpanel_open() || panel.draft.is_some() {
            let _ = reply.send(Err(
                "Close the Command reference editor before opening a manual".into(),
            ));
        } else if panel
            .control_lookup
            .as_ref()
            .is_some_and(|lookup| !lookup.reply.is_closed())
        {
            let _ = reply.send(Err("A Command reference lookup is already loading".into()));
        } else if panel.open && panel.library.is_some() {
            self.finish_control_reference(
                ControlLookup {
                    query,
                    platform,
                    reply,
                },
                ctx,
            );
        } else {
            panel.control_lookup = Some(ControlLookup {
                query,
                platform,
                reply,
            });
            self.start_reference_load(false, ctx);
        }
    }

    fn finish_control_reference(&mut self, lookup: ControlLookup, ctx: &ComponentContext<Self>) {
        if lookup.reply.is_closed() {
            return;
        }
        let result = (|| -> anyhow::Result<serde_json::Value> {
            let auto = self.reference_platform();
            let panel = &self.command_reference;
            anyhow::ensure!(
                !panel.subpanel_open() && panel.draft.is_none(),
                "Close the Command reference editor before opening a manual"
            );
            let key = panel
                .library
                .as_ref()
                .and_then(|library| library.catalog.exact(&lookup.query, lookup.platform))
                .with_context(|| {
                    format!(
                        "No offline manual for {:?} on {}",
                        lookup.query, lookup.platform.1
                    )
                })?;
            let result = serde_json::json!({
                "window_id": self.registry.window_id(),
                "command": key.name,
                "platform": key.platform.0,
            });
            self.set_diagnostics_drawer(None);
            let panel = &mut self.command_reference;
            panel.saved_pages.close();
            if !panel.open {
                panel.open = true;
                panel.saved_pages.refresh(ctx);
            }
            panel.focus = None;
            panel.set_page(key, auto);
            Ok(result)
        })();
        let _ = lookup
            .reply
            .send(result.map_err(|error| format!("{error:#}")));
    }

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
        let dotfile_sync =
            tools.and_then(|(target, session)| session.events.dotfile_sync(target.generation));
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
                            dotfile_sync.as_deref(),
                            ctx,
                        )),
                ),
            ])
    }

    fn reference_selection(&self, origin: clipboard_paste::Origin) -> Option<String> {
        let text = self.pane_selection_text(origin)?;
        let text = text.trim();
        catalog::insertable(text).then(|| text.to_owned())
    }

    pub(super) fn pane_selection_text(&self, origin: clipboard_paste::Origin) -> Option<String> {
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
        Some(selection.text(&frame, true))
    }

    fn start_reference_load(&mut self, refresh: bool, ctx: &ComponentContext<Self>) {
        let panel = &mut self.command_reference;
        if panel.loading || panel.subpanel_open() {
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
                    let value = run_pane_sftp_job(
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
        if matches!(action, Action::Open | Action::Selection(_)) {
            self.set_diagnostics_drawer(None);
        }
        let owner = self.keyboard.hwnd().map(|window| window.0 as usize);
        let panel = &mut self.command_reference;
        match action {
            Action::Open | Action::Selection(_) => {
                if panel.subpanel_open() {
                    return;
                }
                panel.cancel_control_lookup();
                let was_open = panel.open;
                panel.open = true;
                panel.draft = None;
                panel.command_lookup = None;
                panel.request_focus();
                if let Some(text) = selected {
                    panel.related_history.clear();
                    let lookup =
                        command_line::Lookup::new(&text, auto, panel.platform.unwrap_or(auto));
                    panel.set_query(text);
                    panel.mode = SearchMode::Search;
                    panel.selected = 0;
                    panel.command_lookup = lookup;
                }
                if !was_open || panel.library.is_none() {
                    panel.saved_pages.refresh(ctx);
                    self.start_reference_load(false, ctx);
                } else if !panel.loading {
                    panel.resolve_command_lookup();
                }
            }
            Action::Close => panel.close(),
            Action::Query(text) => {
                if panel.set_query(text) {
                    panel.cancel_control_lookup();
                    panel.related_history.clear();
                }
            }
            Action::Platform(Some(index)) => {
                let platform = PLATFORMS.get(index.wrapping_sub(1)).copied();
                if panel.platform != platform {
                    panel.cancel_control_lookup();
                    panel.platform = platform;
                    panel.command_lookup = None;
                    panel.related_history.clear();
                    panel.selected = 0;
                    panel.draft = None;
                    if matches!(panel.mode, SearchMode::Page(_)) {
                        panel.mode = SearchMode::Search;
                    }
                }
            }
            Action::Platform(None) => {}
            Action::Select(index) => {
                if panel.selected != index {
                    panel.selected = index;
                    panel.draft = None;
                }
            }
            Action::Example(action) => self.reference_example_action(action, auto, origin, ctx),
            Action::Copy(text) => {
                panel.error = w11term::clipboard::write(&text)
                    .err()
                    .map(|err| format!("Copying the command example: {err:#}"));
            }
            Action::Link(link) => Self::open_link(&link),
            Action::Refresh => self.start_reference_load(true, ctx),
            Action::Private(action) => panel.author_private_page(action, auto, owner, ctx),
            Action::SavedPage(action) => panel.saved_page_action(action, auto, ctx),
            Action::Related(action) => panel.related_action(action, auto),
            Action::ToolInstall(action) => self.tool_install_action(action, ctx),
            Action::ToolRemoval(action) => self.tool_removal_action(action, ctx),
            Action::Dotfiles(action) => self.reference_dotfiles_action(action, ctx),
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
                    panel.related_history.clear();
                    panel.set_query(tool.name.clone());
                    panel.mode = SearchMode::PreferredExact;
                    panel.platform = None;
                    panel.request_focus();
                }
            }
            Action::Loaded(result) => {
                panel.loading = false;
                let mut control_lookup = panel.control_lookup.take();
                match result {
                    Ok(library) => {
                        panel.library = Some(library);
                        panel.cached.replace(None);
                        panel.selected = 0;
                        panel.draft = None;
                    }
                    Err(err) => {
                        if let Some(lookup) = control_lookup.take() {
                            let _ = lookup.reply.send(Err(err.clone()));
                        }
                        panel.error = Some(if panel.library.is_some() {
                            format!("Keeping the current pages. {err}")
                        } else {
                            err
                        });
                    }
                }
                panel.resolve_command_lookup();
                if let Some(lookup) = control_lookup {
                    self.finish_control_reference(lookup, ctx);
                }
            }
        }
    }
}
