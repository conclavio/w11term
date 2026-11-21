use super::*;
use w11term::shortcuts::BINDINGS;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandId {
    Menu(String),
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
            MenuItem::Submenu { items, .. } => menu_rows(items, profiles, rows),
            MenuItem::Separator { .. } => {}
            MenuItem::Item {
                key,
                label,
                enabled,
            } => {
                if !enabled && key == "panes".into() {
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
        (
            !row.enabled,
            label != query,
            !label.starts_with(&query),
            label,
        )
    });
    rows
}

impl TerminalApp {
    pub(super) fn command_rows(&self, query: &str) -> Vec<Row> {
        let mut rows = vec![];
        menu_rows(self.caret_items(), false, &mut rows);
        menu_rows(self.profile_items(), true, &mut rows);
        let live = self.tabs.active().and_then(|tab| match &tab.page {
            Page::Terminal(live) => Some(live),
            _ => None,
        });
        let tab_count = self.tabs.headers().len();
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
                        Shortcut::CloseTab => tab_count > 0,
                        Shortcut::ClosePane => live.is_some(),
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
                Shortcut::Find,
                "Find in scrollback",
                "Ctrl+Shift+F",
                live.is_some(),
            ),
            (Shortcut::FindNext, "Find next", "F3", live.is_some()),
            (
                Shortcut::FindPrevious,
                "Find previous",
                "Shift+F3",
                live.is_some(),
            ),
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
                id: CommandId::Shortcut(shortcut),
                label: label.into(),
                chord: chord.into(),
                enabled,
            });
        }
        filter(rows, query)
    }

    pub(super) fn run_palette_command(&mut self, id: CommandId, ctx: &ComponentContext<Self>) {
        let message = match id {
            CommandId::Profile(label) => Message::ProfileMenu(label),
            CommandId::Menu(label) => tab_menu_message(label),
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
