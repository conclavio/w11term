use super::*;
use w11term::workspace::{SavedLayout, SavedSource, SshStartup, Workspace, WorkspaceStore};

#[derive(Default)]
pub(super) struct WorkspacePage {
    name: String,
    query: String,
    switching: bool,
    names: Vec<String>,
    status: String,
    startup: Option<StartupEditor>,
}

struct StartupEditor {
    pane: TabId,
    destination: String,
    fields: Rc<RefCell<StartupFields>>,
    directory_changed: Callback<String>,
    command_changed: Callback<String>,
    apply: Callback<()>,
}

#[derive(Default)]
struct StartupFields {
    directory: String,
    command: String,
}

impl StartupFields {
    fn description(&self) -> String {
        match (self.directory.is_empty(), self.command.is_empty()) {
            (true, true) => "Open the profile’s default shell".into(),
            (false, true) => format!("Open an interactive shell in {}", self.directory),
            (true, false) => format!("Run {}", self.command),
            (false, false) => format!("Run {} in {}", self.command, self.directory),
        }
    }
}

impl StartupEditor {
    fn new(pane: TabId, destination: String, fields: StartupFields, apply: Callback<()>) -> Self {
        let fields = Rc::new(RefCell::new(fields));
        let directory = Rc::clone(&fields);
        let command = Rc::clone(&fields);
        // Keep both subscriptions alive throughout the edit. Rebuilding them after
        // one change can retire the adjacent field's queued native TextChanged.
        Self {
            pane,
            destination,
            directory_changed: Callback::new(move |value| directory.borrow_mut().directory = value),
            command_changed: Callback::new(move |value| command.borrow_mut().command = value),
            fields,
            apply,
        }
    }
}

pub enum WorkspaceAction {
    Show,
    ConfigureStartup,
    ApplyStartup(TabId),
    CancelStartup(TabId),
    Name(TabId, String),
    Search(TabId, String),
    Switch(TabId, String),
    Switched(TabId, Result<(), String>),
    Save(TabId),
    Open(TabId, String),
}

impl TerminalApp {
    pub(super) fn workspace_action(
        &mut self,
        action: WorkspaceAction,
        context: &ComponentContext<Self>,
    ) {
        match action {
            WorkspaceAction::ConfigureStartup => {
                let Some(pane) = self.tabs.active_id() else {
                    return;
                };
                let Some(AppTab {
                    page: Page::Terminal(live),
                    ..
                }) = self.tabs.get(pane)
                else {
                    return;
                };
                if !matches!(&live.source, Source::Ssh(ssh) if !ssh.durable && !ssh.player.rustmush)
                {
                    return;
                }
                let destination = source_label(&live.source);
                let fields = live
                    .startup
                    .as_ref()
                    .map(|s| StartupFields {
                        directory: s.directory.clone(),
                        command: s.command.clone(),
                    })
                    .unwrap_or_default();
                self.workspace_action(WorkspaceAction::Show, context);
                if let Some(id) = self.tabs.active_id()
                    && let Some(page) = self.workspace_page_mut(id)
                {
                    let apply = context
                        .sender()
                        .callback(move |_| Message::Workspace(WorkspaceAction::ApplyStartup(id)));
                    page.status.clear();
                    page.startup = Some(StartupEditor::new(pane, destination, fields, apply));
                }
            }
            WorkspaceAction::CancelStartup(id) => {
                if let Some(page) = self.workspace_page_mut(id) {
                    page.startup = None;
                    page.status.clear();
                }
            }
            WorkspaceAction::ApplyStartup(id) => {
                let Some(draft) = self.workspace_page_mut(id).and_then(|p| p.startup.take()) else {
                    return;
                };
                let result = (|| -> anyhow::Result<()> {
                    let Some(AppTab {
                        page: Page::Terminal(live),
                        ..
                    }) = self.tabs.get_mut(draft.pane)
                    else {
                        anyhow::bail!("The SSH pane was closed")
                    };
                    let Source::Ssh(ssh) = &live.source else {
                        anyhow::bail!("The pane requires SSH")
                    };
                    let fields = draft.fields.borrow();
                    live.startup = if fields.directory.is_empty() && fields.command.is_empty() {
                        None
                    } else {
                        Some(Box::new(SshStartup::new(
                            ssh,
                            fields.directory.clone(),
                            fields.command.clone(),
                        )?))
                    };
                    Ok(())
                })();
                if let Some(page) = self.workspace_page_mut(id) {
                    match result {
                        Ok(()) => {
                            page.status = format!(
                                "On reopen · {}. Save or update the workspace to keep this setting.",
                                draft.fields.borrow().description()
                            );
                        }
                        Err(error) => {
                            page.status = error.to_string();
                            page.startup = Some(draft);
                        }
                    }
                }
            }
            WorkspaceAction::Show => {
                let page = self
                    .tabs
                    .entries()
                    .iter()
                    .find(|tab| matches!(tab.value.page, Page::Workspaces(_)))
                    .map(|tab| tab.id);
                let id = if let Some(id) = page {
                    self.tabs.select(id);
                    id
                } else {
                    let id = self.tabs.insert(AppTab {
                        label: "Workspaces".into(),
                        page: Page::Workspaces(WorkspacePage::default()),
                    });
                    self.sync_selection(context);
                    id
                };
                if let Some(page) = self.workspace_page_mut(id) {
                    page.startup = None;
                    match WorkspaceStore::open().and_then(|store| store.load()) {
                        Ok(catalog) => page.names = catalog.into_keys().collect(),
                        Err(error) => page.status = format!("{error:#}"),
                    }
                }
                self.activate(context);
            }
            WorkspaceAction::Name(id, value) => {
                if let Some(page) = self.workspace_page_mut(id) {
                    page.name = value;
                }
            }
            WorkspaceAction::Search(id, query) => {
                if let Some(page) = self.workspace_page_mut(id) {
                    page.query = query;
                }
            }
            WorkspaceAction::Switch(id, name) => {
                let Some(page) = self.workspace_page_mut(id) else {
                    return;
                };
                if page.switching {
                    return;
                }
                page.switching = true;
                page.status = format!("Switching to {name}…");
                let profiles = self.store.path().to_owned();
                context.spawn_background(move |_| {
                    let result = crate::workspace_window::switch(&name, &profiles)
                        .map_err(|error| format!("{error:#}"));
                    Message::Workspace(WorkspaceAction::Switched(id, result))
                });
            }
            WorkspaceAction::Switched(id, result) => {
                let Some(page) = self.workspace_page_mut(id) else {
                    return;
                };
                page.switching = false;
                page.status.clear();
                match result {
                    Ok(_) if self.tabs.headers().len() > 1 => self.close_tab(id, context),
                    Ok(_) => {}
                    Err(error) => self.workspace_page_mut(id).unwrap().status = error,
                }
            }
            WorkspaceAction::Save(id) => {
                let Some(page) = self.workspace_page_mut(id) else {
                    return;
                };
                let name = page.name.trim().to_owned();
                let result = self.capture_workspace().and_then(|workspace| {
                    crate::workspace_window::save(
                        &name,
                        self.store.path(),
                        &workspace,
                        &self._control,
                    )
                });
                if let Some(page) = self.workspace_page_mut(id) {
                    page.status = match result {
                        Ok(()) => {
                            if let Err(index) = page.names.binary_search(&name) {
                                page.names.insert(index, name.clone());
                            }
                            format!("Saved {name}")
                        }
                        Err(error) => format!("{error:#}"),
                    };
                }
            }
            WorkspaceAction::Open(id, name) => {
                if self.workspace_page_mut(id).is_none() {
                    return;
                }
                let result = WorkspaceStore::open().and_then(|store| {
                    let workspace = store
                        .load()?
                        .remove(&name)
                        .with_context(|| format!("Workspace {name} was removed"))?;
                    let osc52 = self.appearance.read().unwrap().interaction.osc52;
                    append_workspace(&mut self.tabs, &workspace, &self.profiles, osc52)
                });
                match result {
                    Ok(()) => {
                        self.sync_selection(context);
                        self.activate(context);
                    }
                    Err(error) => {
                        if let Some(page) = self.workspace_page_mut(id) {
                            page.status = format!("{error:#}");
                        }
                    }
                }
            }
        }
    }

    fn workspace_page_mut(&mut self, id: TabId) -> Option<&mut WorkspacePage> {
        match &mut self.tabs.get_mut(id)?.page {
            Page::Workspaces(page) => Some(page),
            _ => None,
        }
    }

    fn capture_workspace(&self) -> anyhow::Result<Workspace> {
        let headers: Vec<_> = self
            .tabs
            .headers()
            .into_iter()
            .filter(|tab| matches!(tab.value.page, Page::Terminal(_)))
            .collect();
        // The manager itself is a utility tab. Save the most recently visited terminal group.
        let active_tab = self
            .mru_groups
            .iter()
            .find_map(|&group| {
                headers
                    .iter()
                    .position(|h| self.tabs.group_id(h.id) == Some(group))
            })
            .unwrap_or_default();
        let layouts = headers.iter().map(|header| {
            (
                self.tabs
                    .layout_for(header.id)
                    .expect("terminal tab has a layout"),
                header.id,
            )
        });
        Workspace::capture(layouts, active_tab, |id| {
            let tab = self.tabs.get(id).context("Workspace pane was closed")?;
            let Page::Terminal(live) = &tab.page else {
                anyhow::bail!("Expected a terminal pane")
            };
            let source = if live.discovery_page.is_some() {
                SavedSource::Discovery(
                    live.profile_name
                        .clone()
                        .context("Missing discovery profile")?,
                )
            } else {
                SavedSource::capture(&live.source, live.profile_name.as_deref())?
            };
            if let Some(launch) = live.startup.as_deref() {
                return Ok(SavedSource::Startup {
                    connection: Box::new(source),
                    launch: launch.clone(),
                });
            }
            source.with_session(
                live.session
                    .durable_state()
                    .and_then(|state| state.reference),
            )
        })
    }

    pub(super) fn workspace_view(
        &self,
        id: TabId,
        page: &WorkspacePage,
        context: &ViewContext<Self>,
    ) -> View {
        if let Some(editor) = &page.startup {
            let fields = editor.fields.borrow();
            let directory = TextBox::new()
                .automation_name("Remote starting directory")
                .text(&fields.directory)
                .placeholder_text("/home/me/project")
                .on_text_changed(editor.directory_changed.clone());
            let command = TextBox::new()
                .automation_name("Remote startup command")
                .text(&fields.command)
                .placeholder_text("tail -f build.log")
                .on_text_changed(editor.command_changed.clone());
            let buttons = StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .on_click(editor.apply.clone())
                        .content("Apply to pane"),
                    Button::new()
                        .on_click(context.callback(move |_| {
                            Message::Workspace(WorkspaceAction::CancelStartup(id))
                        }))
                        .content("Cancel"),
                ));
            let instructions = "These values apply when you next open this pane from a saved workspace. Leave both empty for the default shell.";
            let panel = StackPanel::new()
                .margin(32.0)
                .spacing(10.0)
                .max_width(600.0)
                .children((
                    TextBlock::new()
                        .text(format!("Workspace startup · {}", editor.destination))
                        .font_size(22.0),
                    TextBlock::new()
                        .text(instructions)
                        .text_wrapping(TextWrapping::Wrap),
                    TextBlock::new().text("Remote starting directory"),
                    directory,
                    TextBlock::new().text("Remote command"),
                    command,
                    TextBlock::new()
                        .text(format!("On reopen · {}", fields.description()))
                        .text_wrapping(TextWrapping::Wrap),
                    buttons,
                    TextBlock::new()
                        .text(&page.status)
                        .text_wrapping(TextWrapping::Wrap),
                ));
            return ScrollViewer::new().content(panel);
        }
        let name = page.name.trim();
        let save_label = if page.names.iter().any(|entry| entry == name) {
            "Update saved workspace"
        } else {
            "Save workspace"
        };
        let mut children = vec![
            KeyedView::new("title", TextBlock::new().text("Workspaces").font_size(26.0)),
            KeyedView::new(
                "name",
                TextBox::new()
                    .automation_name("Workspace name")
                    .placeholder_text("Workspace name")
                    .text(&page.name)
                    .on_text_changed(
                        context.callback(move |name| {
                            Message::Workspace(WorkspaceAction::Name(id, name))
                        }),
                    ),
            ),
            KeyedView::new(
                "save",
                Button::new()
                    .is_enabled(
                        !name.is_empty()
                            && self
                                .tabs
                                .entries()
                                .iter()
                                .any(|tab| matches!(tab.value.page, Page::Terminal(_))),
                    )
                    .on_click(
                        context.callback(move |_| Message::Workspace(WorkspaceAction::Save(id))),
                    )
                    .content(save_label),
            ),
            KeyedView::new(
                "status",
                TextBlock::new()
                    .text(&page.status)
                    .text_wrapping(TextWrapping::Wrap),
            ),
        ];
        if !page.names.is_empty() {
            children.push(KeyedView::new(
                "list",
                TextBlock::new().text("Switch workspace").font_size(18.0),
            ));
            children.push(KeyedView::new(
                "filter",
                TextBox::new()
                    .automation_name("Find workspace")
                    .placeholder_text("Find workspace")
                    .text(&page.query)
                    .on_text_changed(context.callback(move |query| {
                        Message::Workspace(WorkspaceAction::Search(id, query))
                    })),
            ));
        }
        let mut matches = matching_names(&page.names, &page.query).peekable();
        if !page.names.is_empty() && matches.peek().is_none() {
            children.push(KeyedView::new(
                "no-matches",
                TextBlock::new().text("No matching workspaces"),
            ));
        }
        for name in matches {
            let switch = name.clone();
            let saved = name.clone();
            children.push(KeyedView::new(
                format!("workspace-{name}"),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(!page.switching)
                            .automation_name(format!("Switch to workspace {name}"))
                            .on_click(context.callback(move |_| {
                                Message::Workspace(WorkspaceAction::Switch(id, switch.clone()))
                            }))
                            .content(name.as_str()),
                        Button::new()
                            .automation_name(format!("Open workspace {name} in new tabs"))
                            .on_click(context.callback(move |_| {
                                Message::Workspace(WorkspaceAction::Open(id, saved.clone()))
                            }))
                            .content("Append in this window"),
                    )),
            ));
        }
        ScrollViewer::new().content(
            StackPanel::new()
                .spacing(12.0)
                .margin(32.0)
                .width(420.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .keyed_children(children),
        )
    }
}

fn matching_names<'a>(names: &'a [String], query: &str) -> impl Iterator<Item = &'a String> {
    let query = query.trim().to_lowercase();
    names
        .iter()
        .filter(move |name| name.to_lowercase().contains(&query))
}

/// Resolve every source before starting connections, including when opening a workspace at launch.
pub(super) fn append_workspace(
    tabs: &mut Tabs<AppTab>,
    workspace: &Workspace,
    profiles: &BTreeMap<String, SshOptions>,
    osc52: bool,
) -> anyhow::Result<()> {
    let sources = workspace.resolve_sources(profiles)?;
    append_resolved_workspace(tabs, workspace, sources, osc52);
    Ok(())
}

pub(super) fn append_resolved_workspace(
    tabs: &mut Tabs<AppTab>,
    workspace: &Workspace,
    mut sources: BTreeMap<u64, Source>,
    osc52: bool,
) {
    restore_tabs(tabs, workspace, |slot, saved| {
        let source = sources.remove(&slot).expect("validated workspace slot");
        let profile_name = saved.profile_name().map(str::to_owned);
        let label = profile_name
            .clone()
            .unwrap_or_else(|| source_label(&source));
        if let (true, Source::Ssh(options)) = (saved.is_discovery(), &source) {
            return session_browser::discovery_tab(
                label,
                options.clone(),
                osc52,
                saved.session().cloned(),
            );
        }
        if let (SavedSource::Startup { launch, .. }, Source::Ssh(options)) = (saved, &source) {
            let session = Session::start_workspace_ssh(
                options.clone(),
                TerminalSize::default(),
                osc52,
                launch.clone(),
            );
            let mut tab = live_tab_for_session(label, source, session).with_profile(profile_name);
            if let Page::Terminal(live) = &mut tab.page {
                live.startup = Some(Box::new(launch.clone()));
            }
            return tab;
        }
        live_tab_sized(
            label,
            source,
            TerminalSize::default(),
            osc52,
            None,
            saved.session().cloned(),
        )
        .with_profile(profile_name)
    });
}

fn first_leaf(mut layout: &SavedLayout) -> (u64, &SavedSource) {
    loop {
        match layout {
            SavedLayout::Leaf { slot, source } => return (*slot, source),
            SavedLayout::Split { first, .. } => layout = first,
        }
    }
}

/// Every subtree starts with its first leaf already inserted. Split off the second child
/// and visit both children, restoring each divider at its original path.
fn restore_tabs<T>(
    tabs: &mut Tabs<T>,
    workspace: &Workspace,
    mut create: impl FnMut(u64, &SavedSource) -> T,
) {
    fn restore_split<T>(
        tabs: &mut Tabs<T>,
        layout: &SavedLayout,
        entry: TabId,
        path: &mut Vec<bool>,
        ids: &mut BTreeMap<u64, TabId>,
        create: &mut impl FnMut(u64, &SavedSource) -> T,
    ) {
        match layout {
            SavedLayout::Leaf { slot, .. } => {
                ids.insert(*slot, entry);
            }
            SavedLayout::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (slot, source) = first_leaf(second);
                let sibling = tabs
                    .split(entry, create(slot, source), *axis)
                    .expect("restore parent exists");
                let set = tabs.set_ratio(path, *ratio);
                debug_assert!(set, "validated workspace divider");
                path.push(false);
                restore_split(tabs, first, entry, path, ids, create);
                *path.last_mut().unwrap() = true;
                restore_split(tabs, second, sibling, path, ids, create);
                path.pop();
            }
        }
    }
    let mut active = None;
    for (index, tab) in workspace.tabs.iter().enumerate() {
        let (slot, source) = first_leaf(&tab.layout);
        let entry = tabs.insert(create(slot, source));
        let mut ids = BTreeMap::new();
        restore_split(tabs, &tab.layout, entry, &mut vec![], &mut ids, &mut create);
        let focused = ids[&tab.focused];
        tabs.select(focused);
        if index == workspace.active_tab {
            active = Some(focused);
        }
    }
    tabs.select(active.expect("validated workspace has an active tab"));
}

#[cfg(feature = "ui-smoke")]
impl TerminalApp {
    /// Exercise the same workspace actions that the native buttons dispatch.
    pub(super) fn workspace_smoke_step(&mut self, step: u8, context: &ComponentContext<Self>) {
        const NAME: &str = "saved-by-smoke";
        eprintln!("Workspace UI smoke step {step}");
        match step {
            0 => {
                assert_eq!(self.tabs.headers().len(), 1);
                self.update(Message::SplitPane(Axis::Rows, None), context);
            }
            1 => {
                let epoch = self.watch_epoch;
                self.update(Message::DividerPress(epoch, vec![], 3.0), context);
                self.update(Message::PaneRatio(epoch, vec![], 0.6), context);
                self.update(Message::DividerEnd(epoch, vec![]), context);
                self.open("Second lab".into(), Source::Demo, None, context);
                self.update(Message::Shortcut(Shortcut::Select(0)), context);
                self.update(Message::Workspace(WorkspaceAction::Show), context);
            }
            2 => {
                let id = self.tabs.active_id().expect("workspace page");
                self.update(
                    Message::Workspace(WorkspaceAction::Name(id, NAME.into())),
                    context,
                );
                self.update(Message::Workspace(WorkspaceAction::Save(id)), context);
                let page = self.workspace_page_mut(id).expect("workspace page");
                assert_eq!(page.status, format!("Saved {NAME}"));
                assert!(page.names.iter().any(|name| name == NAME));
                let workspace = WorkspaceStore::open()
                    .unwrap()
                    .load()
                    .unwrap()
                    .remove(NAME)
                    .unwrap();
                assert_eq!(workspace.active_tab, 0);
                assert_eq!(workspace.tabs.len(), 2);
                let saved = &workspace.tabs[0];
                let SavedLayout::Split {
                    axis: Axis::Rows,
                    ratio,
                    second,
                    ..
                } = &saved.layout
                else {
                    panic!("expected vertical stack")
                };
                assert!((*ratio - 0.6).abs() < 0.001);
                assert_eq!(saved.focused, first_leaf(second).0);
                self.update(
                    Message::Workspace(WorkspaceAction::Open(id, NAME.into())),
                    context,
                );
            }
            3 => {
                assert_eq!(
                    self.tabs.headers().len(),
                    5,
                    "two restored groups appended to three existing tabs"
                );
                assert_eq!(self.tabs.active_index(), Some(3));
                let original = self.tabs.headers()[0].id;
                let opened = self.tabs.active_id().unwrap();
                assert_ne!(opened, original);
                let restored = self.tabs.active_layout().unwrap().leaves();
                assert_eq!(restored.len(), 2);
                assert_eq!(restored[1], opened);
                for id in restored {
                    let Page::Terminal(tab) = &self.tabs.get(id).unwrap().page else {
                        panic!("expected terminal")
                    };
                    let size = tab.surface.metrics.get();
                    assert!(tab.surface.area.get().is_some());
                    assert_eq!(tab.session.frames.load().size.cols, size.cols);
                    assert_eq!(tab.session.frames.load().size.rows, size.rows);
                }
                let ids: Vec<_> = self.tabs.headers().into_iter().map(|tab| tab.id).collect();
                for id in ids {
                    self.close_tab(id, context);
                }
                eprintln!("Workspace UI smoke passed");
                return;
            }
            _ => panic!("unexpected workspace smoke step"),
        }
        context.spawn_background(move |_| {
            std::thread::sleep(Duration::from_millis(600));
            Message::Smoke(step + 1)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use w11term::pane_layout::{Axis, Layout};
    use w11term::workspace::WorkspaceTab;

    #[test]
    fn workspace_search_preserves_names_and_matches_case_insensitive_substrings() {
        let names = vec![String::from("Admin console"), String::from("Test rig")];
        assert_eq!(
            matching_names(&names, "  CoNs  ").collect::<Vec<_>>(),
            [&names[0]]
        );
        assert_eq!(matching_names(&names, "").count(), 2);
        assert_eq!(matching_names(&names, "absent").count(), 0);
    }

    fn demo(slot: u64) -> SavedLayout {
        SavedLayout::Leaf {
            slot,
            source: SavedSource::Demo,
        }
    }

    fn nested() -> Workspace {
        Workspace {
            active_tab: 0,
            tabs: vec![
                WorkspaceTab {
                    focused: 12,
                    layout: SavedLayout::Split {
                        axis: Axis::Columns,
                        ratio: 0.6,
                        first: Box::new(SavedLayout::Split {
                            axis: Axis::Rows,
                            ratio: 0.3,
                            first: Box::new(demo(11)),
                            second: Box::new(demo(12)),
                        }),
                        second: Box::new(demo(13)),
                    },
                },
                WorkspaceTab {
                    focused: 14,
                    layout: demo(14),
                },
            ],
        }
    }

    #[test]
    fn workspace_restore_appends_fresh_groups_and_preserves_dividers_and_focus() {
        let workspace = nested();
        workspace.validate().unwrap();
        let mut tabs = Tabs::default();
        let existing = tabs.insert(0);
        restore_tabs(&mut tabs, &workspace, |slot, _| slot);
        assert_eq!(tabs.get(existing), Some(&0));
        assert_eq!(
            tabs.headers()
                .iter()
                .map(|tab| tab.value)
                .collect::<Vec<_>>(),
            [0, 12, 14]
        );
        assert_eq!(tabs.active_index(), Some(1));
        let entry = |slot| {
            tabs.entries()
                .iter()
                .find(|entry| entry.value == slot)
                .unwrap()
                .id
        };
        let left_top = entry(11);
        let left_bottom = entry(12);
        let right = entry(13);
        assert_eq!(tabs.active_id(), Some(left_bottom));
        assert_eq!(
            tabs.active_layout(),
            Some(&Layout::Split {
                axis: Axis::Columns,
                ratio: 0.6,
                first: Box::new(Layout::Split {
                    axis: Axis::Rows,
                    ratio: 0.3,
                    first: Box::new(Layout::Leaf(left_top)),
                    second: Box::new(Layout::Leaf(left_bottom)),
                }),
                second: Box::new(Layout::Leaf(right)),
            })
        );
        restore_tabs(&mut tabs, &workspace, |slot, _| slot);
        assert_eq!(tabs.active_index(), Some(3));
        assert_ne!(tabs.active_id(), Some(left_bottom));
        assert_eq!(tabs.get(left_bottom), Some(&12));
        assert_eq!(tabs.headers().len(), 5);
    }

    #[test]
    fn missing_profile_in_last_tab_leaves_existing_tabs_and_focus_intact() {
        let mut workspace = nested();
        workspace.tabs[1].layout = SavedLayout::Leaf {
            slot: 14,
            source: SavedSource::SavedProfile("missing".into()),
        };
        let mut tabs = Tabs::default();
        let existing = tabs.insert(AppTab {
            label: "Settings".into(),
            page: Page::Settings(None),
        });
        let error = append_workspace(&mut tabs, &workspace, &BTreeMap::new(), false).unwrap_err();
        assert!(error.to_string().contains("missing"));
        assert_eq!(tabs.active_id(), Some(existing));
        assert_eq!(tabs.entries().len(), 1);
    }
}
