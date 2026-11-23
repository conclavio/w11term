use super::*;
use w11term::shortcuts::BINDINGS;

#[derive(Clone, Debug, PartialEq)]
pub enum CommandId {
    Menu(String),
    MoveTabToExistingWindow(Option<TabId>, u64),
    BroadcastTyping(pane_broadcast::Action),
    RenamePane(Option<(u64, TabId)>),
    FocusPreviousPane(Option<previous_pane::Target>),
    SaveScrollback(Option<u64>),
    Recording(Option<recordings::Choice>),
    ClearScrollback(Option<u64>),
    SelectAll(Option<clipboard_paste::Origin>),
    PaneFind(Option<clipboard_paste::Origin>, Shortcut),
    CopySelection(Option<clipboard_paste::Origin>),
    PaneColorScheme(u64, ColorScheme),
    SharedFolders(Option<u64>),
    LocalForward(Option<local_forwards::Origin>),
    RestartExited(Option<ExitedPane>),
    Clipboard(Option<clipboard_paste::Origin>, bool),
    SavedCommand(
        Option<clipboard_paste::Origin>,
        w11term::saved_commands::Entry,
    ),
    TabAction(Shortcut, Option<TabId>),
    SetTabPinned(Option<TabId>, bool),
    ReopenClosedTab(Option<TabId>),
    ReopenClosedPane(Option<TabId>),
    SwapPanes(PaneSwap),
    RotateSplit(PaneRotation),
    BalancePanes(PaneBalance),
    OpenInDirectory(
        directory_split::Destination,
        Option<directory_split::Origin>,
    ),
    Profile(String),
    Shortcut(Shortcut),
}

pub(super) struct Row {
    pub id: CommandId,
    label: String,
    chord: String,
    pub enabled: bool,
}

fn menu_shortcut(message: Message) -> Option<Shortcut> {
    match message {
        Message::Shortcut(shortcut) => Some(shortcut),
        Message::PaneNeighbor(direction) => Some(Shortcut::FocusPane(direction)),
        Message::SplitPane(Axis::Columns, None) => Some(Shortcut::SplitRight),
        Message::SplitPane(Axis::Rows, None) => Some(Shortcut::SplitBelow),
        Message::ZoomPane => Some(Shortcut::ZoomPane),
        Message::ClosePane => Some(Shortcut::ClosePane),
        Message::MovePaneToTab => Some(Shortcut::MovePane),
        Message::Workspace(workspace_view::WorkspaceAction::Show) => Some(Shortcut::Workspaces),
        Message::Paste => Some(Shortcut::Paste),
        Message::PasteRaw => Some(Shortcut::PasteRaw),
        _ => None,
    }
}

// Use the same profile and caret menu entries (including nested menus) and their enabled state.
// Preserve the complete callback label for names containing spaces, colons or other punctuation.
fn menu_rows(items: Vec<MenuItem>, profiles: bool, rows: &mut Vec<Row>) {
    for item in items {
        match item {
            // Color choices are bound below to the native pane ID and get a searchable verb.
            MenuItem::Submenu { key, .. } if !profiles && key == pane_colors::MENU_KEY.into() => {}
            MenuItem::Submenu { items, .. } => menu_rows(items, profiles, rows),
            MenuItem::Separator { .. } => {}
            MenuItem::Item {
                key,
                label,
                enabled,
            } => {
                if (!enabled && key == "panes".into())
                    || (!profiles
                        && (key == "save-scrollback".into()
                            || key == "copy-selection".into()
                            || key == "restart-exited".into()
                            || key == "shared-folders".into()
                            || key == "local-forward".into()))
                {
                    continue;
                }
                let id = if profiles {
                    CommandId::Profile(label.clone())
                } else {
                    menu_shortcut(tab_menu_message(label.clone()))
                        .map_or_else(|| CommandId::Menu(label.clone()), CommandId::Shortcut)
                };
                if id == CommandId::Shortcut(Shortcut::CommandPalette) {
                    continue;
                }
                let (label, chord) = match &id {
                    CommandId::Profile(_) if key != "new-profile".into() && key != "lab".into() => {
                        (format!("New tab with {label}"), String::new())
                    }
                    CommandId::Shortcut(Shortcut::Fullscreen) => {
                        (label.replace(" (F11)", ""), "F11".into())
                    }
                    CommandId::Shortcut(Shortcut::Paste) => (label, "Ctrl+V".into()),
                    CommandId::Shortcut(Shortcut::PasteRaw) => (label, "Ctrl+Alt+V".into()),
                    CommandId::Shortcut(_) => match label.rsplit_once("    ") {
                        Some((title, chord)) => (title.to_owned(), chord.to_owned()),
                        None => (label, String::new()),
                    },
                    _ => (label, String::new()),
                };
                rows.push(Row {
                    id,
                    label,
                    chord,
                    enabled,
                });
            }
        }
    }
}

fn filter(mut rows: Vec<Row>, query: &str) -> Vec<Row> {
    let query = query.trim().to_lowercase();
    let words: Vec<_> = query.split_whitespace().collect();
    rows.retain(|row| {
        let text = format!("{} {}", row.label, row.chord).to_lowercase();
        words.iter().all(|word| text.contains(word))
    });
    rows.sort_by_cached_key(|row| {
        let label = row.label.to_lowercase();
        // Stable sorting keeps profiles within each action in the Settings order.
        let group = [
            "new tab with ",
            "split right with ",
            "split below with ",
            "find sessions on ",
            "browse herdr terminals on ",
        ]
        .into_iter()
        .find(|prefix| label.starts_with(prefix));
        (
            !row.enabled,
            label != query,
            !label.starts_with(&query),
            group.map_or(label, str::to_owned),
        )
    });
    rows
}

impl TerminalApp {
    pub(super) fn command_rows(&self, query: &str) -> Vec<Row> {
        let mut rows = vec![];
        let exited = self.tabs.active().and_then(AppTab::exited_pane);
        let arrangement = self.pane_arrangement();
        let directory_split = self.directory_split_origin();
        let previous = previous_pane::target(&self.tabs);
        let recordings = self.recording_choices();
        menu_rows(
            self.caret_items(
                exited.is_some(),
                &arrangement,
                directory_split.is_some(),
                previous,
                recordings,
            ),
            false,
            &mut rows,
        );
        let origin = self.paste_origin();
        let broadcast = self.broadcast_offer();
        let rename_target = self
            .tabs
            .active()
            .and_then(AppTab::pane_id)
            .zip(self.tabs.active_group_id());
        for row in &mut rows {
            if let CommandId::Menu(label) = &row.id
                && let Some(window) = tab_windows::menu_window(label)
            {
                let title = label.strip_prefix("Window ").unwrap_or(label);
                row.label = format!("Move tab to existing window {title}");
                row.id = CommandId::MoveTabToExistingWindow(self.tabs.active_group_id(), window);
            }
            if matches!(&row.id, CommandId::Menu(label) if pane_broadcast::is_label(label))
                && let Some(action) = broadcast
                    .as_ref()
                    .filter(|action| action.label() == row.label)
            {
                row.id = CommandId::BroadcastTyping(action.clone());
            }
            if let CommandId::Shortcut(shortcut @ (Shortcut::Paste | Shortcut::PasteRaw)) = row.id {
                row.id = CommandId::Clipboard(origin, shortcut == Shortcut::PasteRaw);
            }
            if let CommandId::Menu(label) = &row.id
                && recordings::is_command(label)
            {
                row.id = CommandId::Recording(recordings.target(label));
            }
            if matches!(&row.id, CommandId::Menu(label) if label == tab_title::RENAME_PANE) {
                row.id = CommandId::RenamePane(rename_target);
            }
            if let CommandId::Menu(label) = &row.id
                && let Some(pinned) = tab_pin_command(label)
            {
                row.id = CommandId::SetTabPinned(self.active_terminal_group(), pinned);
            }
        }
        menu_rows(self.profile_items(), true, &mut rows);
        let live = self.tabs.active().and_then(|tab| match &tab.page {
            Page::Terminal(live) => Some(live),
            _ => None,
        });
        let tab_count = self.tabs.headers().len();
        let pane = live.map(|tab| tab.session.pane_id);
        if let Some(live) = live.filter(|tab| pane_colors::available(tab)) {
            rows.extend(
                pane_colors::choices(live.color_scheme).map(|(scheme, label)| Row {
                    id: CommandId::PaneColorScheme(live.session.pane_id, scheme),
                    label: format!("Set pane color scheme to {label}"),
                    chord: String::new(),
                    enabled: true,
                }),
            );
        }
        let forward = self.forward_origin();
        rows.push(Row {
            id: CommandId::LocalForward(forward),
            label: local_forwards::LABEL.into(),
            chord: String::new(),
            enabled: forward.is_some(),
        });
        rows.push(Row {
            id: CommandId::SharedFolders(pane),
            label: folder_shares::LABEL.into(),
            chord: String::new(),
            enabled: pane.and_then(|p| self.folder_authority(p)).is_some(),
        });
        rows.push(Row {
            id: CommandId::RestartExited(exited),
            label: RESTART_EXITED_PANE.into(),
            chord: String::new(),
            enabled: exited.is_some(),
        });
        rows.push(Row {
            id: CommandId::SaveScrollback(pane),
            label: scrollback_export::LABEL.into(),
            chord: String::new(),
            enabled: pane.is_some() && self.export.pending.is_none(),
        });
        let selection = self.active_selection();
        let appearance = self.appearance.read().unwrap();
        for binding in BINDINGS
            .iter()
            .filter(|b| b.action != KeyAction::CommandPalette)
        {
            let shortcut = Shortcut::from(binding.action);
            let id = CommandId::Shortcut(shortcut);
            let row = match rows.iter_mut().find(|r| r.id == id) {
                Some(row) => row,
                None => {
                    // Pane commands absent from the native menu require a different layout or source.
                    let enabled = match shortcut {
                        Shortcut::NewTab => true,
                        Shortcut::CloseTab => tab_count > 0 && !self.active_tab_is_pinned(),
                        Shortcut::ClosePane => self.can_close_active_pane(),
                        Shortcut::Find | Shortcut::FindNext | Shortcut::FindPrevious => {
                            live.is_some_and(LiveTab::find_available)
                        }
                        Shortcut::NextTab | Shortcut::PreviousTab => tab_count > 1,
                        Shortcut::Select(index) => index < tab_count,
                        _ => false,
                    };
                    rows.push(Row {
                        id,
                        label: binding.label.into(),
                        chord: String::new(),
                        enabled,
                    });
                    rows.last_mut().unwrap()
                }
            };
            row.chord = appearance
                .shortcuts
                .get(binding)
                .map(|chord| chord.to_string())
                .unwrap_or_default();
            match shortcut {
                Shortcut::ReopenClosedTab => {
                    let closed = self.closed_tabs.back().map(|tab| tab.group);
                    row.enabled = closed.is_some();
                    row.id = CommandId::ReopenClosedTab(closed);
                }
                Shortcut::ReopenClosedPane => {
                    let closed = self.closed_pane.as_ref().map(|pane| pane.id);
                    row.enabled = closed.is_some();
                    row.id = CommandId::ReopenClosedPane(closed);
                }
                Shortcut::FocusPreviousPane => {
                    row.enabled = previous.is_some();
                    row.id = CommandId::FocusPreviousPane(previous);
                }
                shortcut if let Some(destination) = directory_split::destination(shortcut) => {
                    row.id = CommandId::OpenInDirectory(destination, directory_split.clone());
                }
                Shortcut::ClearScrollback => {
                    row.id = CommandId::ClearScrollback(pane);
                }
                Shortcut::SelectAll => {
                    row.id = CommandId::SelectAll(origin);
                    row.enabled = self.copy_mode_target().is_some();
                }
                Shortcut::Find | Shortcut::FindNext | Shortcut::FindPrevious => {
                    row.id = CommandId::PaneFind(origin, shortcut);
                }
                Shortcut::DuplicateTabLayout | Shortcut::MoveTabLeft | Shortcut::MoveTabRight => {
                    // A click queued on this row must keep its original tab even
                    // when a control client switches tabs before we receive it.
                    row.id = CommandId::TabAction(shortcut, self.tabs.active_group_id());
                }
                Shortcut::SwapPane(direction) => {
                    if let Some(target) = arrangement.get(direction) {
                        row.id = CommandId::SwapPanes(target);
                    }
                }
                Shortcut::RotateSplit => {
                    if let Some(target) = &arrangement.rotation {
                        row.id = CommandId::RotateSplit(target.clone());
                    } else {
                        row.enabled = false;
                    }
                }
                Shortcut::BalancePanes => {
                    if let Some(target) = &arrangement.balance {
                        row.id = CommandId::BalancePanes(target.clone());
                    } else {
                        row.enabled = false;
                    }
                }
                Shortcut::FocusPane(dir) => {
                    row.enabled = self
                        .tabs
                        .active_id()
                        .zip(self.tabs.active_layout())
                        .is_some_and(|(id, layout)| layout.neighbor(id, dir).is_some());
                }
                Shortcut::KeyboardHints | Shortcut::CopyMode => {
                    row.enabled = live.is_some_and(|live| {
                        !live.find.open
                            && !live.device_busy
                            && !live.browsing_sessions()
                            && live.session.frames.load().prompt.is_none()
                    });
                }
                _ => {}
            }
        }
        for (shortcut, label, chord, enabled) in [
            (Shortcut::Copy, "Copy selection", "Ctrl+Shift+C", selection),
            (
                Shortcut::Zoom(1),
                "Increase font size",
                "Ctrl++",
                appearance.font_size < 72.0,
            ),
            (
                Shortcut::Zoom(-1),
                "Decrease font size",
                "Ctrl+-",
                appearance.font_size > 6.0,
            ),
            (Shortcut::ResetZoom, "Reset font size", "Ctrl+0", true),
        ] {
            rows.push(Row {
                id: if shortcut == Shortcut::Copy {
                    CommandId::CopySelection(origin)
                } else {
                    CommandId::Shortcut(shortcut)
                },
                label: label.into(),
                chord: chord.into(),
                enabled,
            });
        }
        let origin = self.saved_command_origin();
        rows.extend(self.saved_commands.entries().iter().map(|entry| Row {
            id: CommandId::SavedCommand(origin, entry.clone()),
            label: format!("Insert command · {}", entry.name),
            chord: String::new(),
            enabled: origin.is_some(),
        }));
        filter(rows, query)
    }

    pub(super) fn run_palette_command(&mut self, id: CommandId, ctx: &ComponentContext<Self>) {
        let message = match id {
            CommandId::RenamePane(target) => tab_title::show_pane(target),
            CommandId::ReopenClosedTab(group) => Message::ReopenClosedTab(group),
            CommandId::ReopenClosedPane(id) => Message::ReopenClosedPane(id),
            CommandId::FocusPreviousPane(target) => previous_pane::message(target),
            CommandId::PaneColorScheme(pane, scheme) => Message::PaneColorScheme(pane, scheme),
            CommandId::LocalForward(origin) => local_forwards::open(origin),
            CommandId::OpenInDirectory(destination, target) => {
                directory_split::message(destination, target.as_ref())
            }
            CommandId::Clipboard(origin, raw) => {
                Message::Clipboard(clipboard_paste::Action::Start(origin, raw))
            }
            CommandId::SavedCommand(Some(origin), entry) => {
                return self.insert_saved_command(origin, entry, ctx);
            }
            CommandId::SavedCommand(None, _) => return,
            CommandId::TabAction(shortcut, group) => {
                bind_tab_action(Message::Shortcut(shortcut), group)
            }
            CommandId::SetTabPinned(group, pinned) => group.map_or(Message::ControlIdle, |group| {
                Message::SetTabPinned(group, pinned)
            }),
            CommandId::SwapPanes(target) => Message::SwapPanes(target),
            CommandId::RotateSplit(target) => Message::RotateSplit(target),
            CommandId::BalancePanes(target) => Message::BalancePanes(target),
            CommandId::SharedFolders(Some(pane)) => {
                Message::FolderShare(folder_shares::Action::Open(pane))
            }
            CommandId::SharedFolders(None) => return,
            CommandId::RestartExited(Some(exit)) => Message::RestartExited(exit),
            CommandId::RestartExited(None) => return,
            CommandId::SaveScrollback(pane) => {
                Message::Export(scrollback_export::Action::Start(pane))
            }
            CommandId::ClearScrollback(pane) => {
                pane.map_or(Message::ControlIdle, Message::ClearScrollback)
            }
            CommandId::SelectAll(origin) => Message::SelectAll(origin),
            CommandId::PaneFind(origin, shortcut) => Message::PaneFind(origin, shortcut),
            CommandId::CopySelection(origin) => Message::CopySelection(origin),
            CommandId::Profile(label) => Message::ProfileMenu(label),
            CommandId::Menu(label) => tab_menu_message(label),
            CommandId::MoveTabToExistingWindow(group, window) => {
                Message::MoveTabToExistingWindow(group, window)
            }
            CommandId::BroadcastTyping(action) => Message::BroadcastTyping(action),
            CommandId::Recording(choice) => recordings::invoke(choice),
            CommandId::Shortcut(shortcut) => Message::Shortcut(shortcut),
        };
        let tab_switch = matches!(
            &message,
            Message::Shortcut(Shortcut::NextTab | Shortcut::PreviousTab)
        );
        self.update(message, ctx);
        // Invoking from the palette has no switcher modifier to release.
        if tab_switch {
            self.finish_switcher(true, ctx);
        }
    }
}

pub(super) fn view(
    rows: &[Row],
    selected: usize,
    query: &str,
    input: &ElementRef<TextBox>,
    app: &TerminalApp,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let page = ((app.window_height - 156.0) / 40.0).clamp(1.0, 10.0) as usize;
    let first = selected / page * page;
    let search = Grid::new()
        .columns([GridLength::STAR, GridLength::Auto])
        .children((
            TextBox::new()
                .element_ref(input)
                .text(query)
                .placeholder_text("Search commands")
                .automation_name("Search commands")
                .on_text_changed(ctx.callback(Message::PickerQuery)),
            Button::new()
                .grid_column(1)
                .style(ButtonStyle::Subtle)
                .automation_name("Close command palette")
                .on_click(ctx.callback(|_| Message::Shortcut(Shortcut::PickerCancel)))
                .content(TextBlock::new().text("×")),
        ));
    let mut results = Vec::new();
    for (index, row) in rows.iter().enumerate().skip(first).take(page) {
        let id = row.id.clone();
        let caption = if row.chord.is_empty() {
            row.label.clone()
        } else {
            format!("{}    {}", row.label, row.chord)
        };
        let button = Button::new()
            .automation_name(&caption)
            .is_enabled(row.enabled)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Stretch)
            .style(if index == selected {
                ButtonStyle::Accent
            } else {
                ButtonStyle::Subtle
            })
            .on_click(ctx.callback(move |_| Message::PaletteChoose(id.clone())))
            .content(
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .children((
                        TextBlock::new()
                            .text(&row.label)
                            .horizontal_alignment(HorizontalAlignment::Left),
                        TextBlock::new().grid_column(1).text(&row.chord),
                    )),
            );
        results.push(KeyedView::new(index, button));
    }
    if rows.is_empty() {
        results.push(KeyedView::new(
            "empty",
            TextBlock::new().text("No matching commands"),
        ));
    } else if rows.len() > page {
        let last = (first + page).min(rows.len());
        results.push(KeyedView::new(
            "pages",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .automation_name("Previous commands")
                        .is_enabled(first > 0)
                        .on_click(ctx.callback(move |_| {
                            Message::Shortcut(Shortcut::PickerMove(-(page as isize)))
                        }))
                        .content(TextBlock::new().text("‹")),
                    TextBlock::new()
                        .vertical_alignment(VerticalAlignment::Center)
                        .text(format!("{}–{last} of {}", first + 1, rows.len())),
                    Button::new()
                        .automation_name("Next commands")
                        .is_enabled(last < rows.len())
                        .on_click(ctx.callback(move |_| {
                            Message::Shortcut(Shortcut::PickerMove(page as isize))
                        }))
                        .content(TextBlock::new().text("›")),
                )),
        ));
    }
    pane_picker::Picker::overlay(
        StackPanel::new().spacing(6.0).children((
            search,
            StackPanel::new().spacing(4.0).keyed_children(results),
        )),
        (app.width - 32.0).clamp(1.0, 660.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_all_menu_and_palette_share_the_configured_chord() {
        let mut rows = Vec::new();
        menu_rows(
            vec![
                MenuItem::item("select-all", "Select all terminal text    F10"),
                MenuItem::item("copy-selection", "Copy selection    Ctrl+Shift+C"),
            ],
            false,
            &mut rows,
        );
        assert_eq!(rows.len(), 1, "Copy has a separate pane-bound palette row");
        assert_eq!(rows[0].id, CommandId::Shortcut(Shortcut::SelectAll));
        assert_eq!(rows[0].chord, "F10");
        assert_eq!(rows[0].label, KeyAction::SelectAll.binding().label);
    }

    #[test]
    fn profile_results_preserve_menu_order_within_each_action() {
        let mut rows = Vec::new();
        menu_rows(
            vec![
                MenuItem::item("local:z", "Zulu · Local"),
                MenuItem::item("profile:a", "Alpha    user@host:22"),
            ],
            true,
            &mut rows,
        );
        menu_rows(
            vec![
                MenuItem::item("z", "Split right with Zulu"),
                MenuItem::item("a", "Split right with Alpha"),
            ],
            false,
            &mut rows,
        );
        let labels = filter(rows, "with")
            .into_iter()
            .map(|r| r.label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                "New tab with Zulu · Local",
                "New tab with Alpha    user@host:22",
                "Split right with Zulu",
                "Split right with Alpha",
            ]
        );
    }

    #[test]
    fn nested_profile_names_survive_filtering_and_dispatch() {
        let profile = "ops    west: blue";
        let label = format!("Split right with {profile}");
        let mut rows = Vec::new();
        menu_rows(
            vec![MenuItem::submenu(
                "right-profile",
                "Split",
                [MenuItem::item("command-palette", &label)],
            )],
            false,
            &mut rows,
        );
        let found = filter(rows, "WEST ops");
        assert_eq!(found.len(), 1);
        let CommandId::Menu(callback) = &found[0].id else {
            panic!("expected menu command")
        };
        assert_eq!(found[0].label, label);
        assert!(matches!(tab_menu_message(callback.clone()),
            Message::SplitPane(Axis::Columns, Some(name)) if name == profile));

        let display = format!("{profile}    me@example:23");
        let mut rows = Vec::new();
        menu_rows(
            vec![MenuItem::item("profile:ops", &display)],
            true,
            &mut rows,
        );
        assert_eq!(rows[0].id, CommandId::Profile(display));
    }

    #[test]
    fn unavailable_tab_duplication_is_searchable_with_its_reason() {
        let mut rows = Vec::new();
        menu_rows(
            vec![MenuItem::disabled(
                "duplicate_tab_layout",
                "Duplicate tab layout: Select a terminal tab",
            )],
            false,
            &mut rows,
        );
        let found = filter(rows, "duplicate layout");
        assert_eq!(found.len(), 1);
        assert!(!found[0].enabled);
        assert!(found[0].label.contains("Select a terminal tab"));
        assert_eq!(
            found[0].id,
            CommandId::Shortcut(Shortcut::DuplicateTabLayout)
        );
    }

    #[test]
    fn search_includes_chords_and_orders_disabled_matches_after_enabled_commands() {
        let mut rows = Vec::new();
        menu_rows(
            vec![
                MenuItem::disabled("focus-left", "Focus left    F9"),
                MenuItem::item("focus-right", "Focus right    F8"),
                MenuItem::item("command-palette", "Command palette…    F7"),
            ],
            false,
            &mut rows,
        );
        let found = filter(rows, " FOCUS ");
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].label.as_str(), found[0].chord.as_str()),
            ("Focus right", "F8")
        );
        assert!(!found[1].enabled);
        let found = filter(found, "f9  left");
        assert_eq!(
            found[0].id,
            CommandId::Shortcut(Shortcut::FocusPane(Direction::Left))
        );
        assert_eq!(found.len(), 1);
    }
}
