use super::*;
use w11term::saved_commands::{Commands, Entry, MAX_COMMANDS};

pub(super) struct Editor {
    draft: Option<Draft>,
    error: Option<String>,
}

struct Draft {
    original: Option<Entry>,
    entry: Entry,
}

pub enum Edit {
    Show,
    Back,
    Open(Option<Entry>),
    Name(String),
    Text(String),
    Save,
    Remove,
    Cancel,
}

impl TerminalApp {
    pub(super) fn reload_saved_commands(&mut self) -> Option<String> {
        self.saved_commands = Commands::default();
        match Commands::path().and_then(|path| Commands::load(&path)) {
            Ok(commands) => {
                self.saved_commands = commands;
                None
            }
            Err(error) => Some(format!("{error:#}")),
        }
    }

    pub(super) fn saved_command_origin(&self) -> Option<clipboard_paste::Origin> {
        let origin = self.paste_origin()?;
        let Page::Terminal(tab) = &self.tabs.active()?.page else {
            return None;
        };
        (tab.session.events.shell_generation() == Some(origin.connection)
            && tab.session.prompts.current().is_none()
            && !tab.read_only.get())
        .then_some(origin)
    }

    pub(super) fn insert_saved_command(
        &mut self,
        origin: clipboard_paste::Origin,
        entry: Entry,
        ctx: &ComponentContext<Self>,
    ) {
        if self.saved_command_origin() != Some(origin) {
            return;
        }
        if let Some(error) = self.reload_saved_commands() {
            self.window_error = Some(error);
        } else if self.saved_commands.entries().contains(&entry) {
            self.paste_saved_text(origin, entry.text, ctx);
        } else {
            self.window_error = Some("This saved command changed. Open the palette again.".into());
        }
    }

    pub(super) fn edit_saved_commands(&mut self, action: Edit) {
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        if !matches!(
            self.tabs.get(id).map(|tab| &tab.page),
            Some(Page::Settings(_))
        ) {
            return;
        }
        let load_error = if matches!(action, Edit::Show) {
            self.reload_saved_commands()
        } else {
            None
        };
        let Some(AppTab {
            page: Page::Settings(current),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        if matches!(action, Edit::Show) {
            *current = Some(SettingsEditor::SavedCommands(Editor {
                draft: None,
                error: load_error,
            }));
            return;
        }
        let Some(SettingsEditor::SavedCommands(editor)) = current else {
            return;
        };
        match action {
            Edit::Back => *current = None,
            Edit::Open(original) => {
                editor.draft = Some(Draft {
                    entry: original.clone().unwrap_or_else(|| Entry {
                        name: String::new(),
                        text: String::new(),
                    }),
                    original,
                });
                editor.error = None;
            }
            Edit::Name(name) => {
                if let Some(draft) = &mut editor.draft {
                    draft.entry.name = name;
                    editor.error = None;
                }
            }
            Edit::Text(text) => {
                if let Some(draft) = &mut editor.draft {
                    draft.entry.text = text;
                    editor.error = None;
                }
            }
            Edit::Save | Edit::Remove => {
                let Some(draft) = &editor.draft else { return };
                let entry = matches!(action, Edit::Save).then(|| draft.entry.clone());
                match Commands::path()
                    .and_then(|path| Commands::update(&path, draft.original.as_ref(), entry))
                {
                    Ok(commands) => {
                        self.saved_commands = commands;
                        editor.draft = None;
                        editor.error = None;
                    }
                    Err(error) => editor.error = Some(format!("{error:#}")),
                }
            }
            Edit::Cancel => {
                editor.draft = None;
                editor.error = None;
            }
            Edit::Show => unreachable!(),
        }
    }
}

pub(super) fn view(editor: &Editor, commands: &Commands, ctx: &ViewContext<TerminalApp>) -> View {
    let mut rows = vec![
        KeyedView::new(
            "back",
            Button::new()
                .on_click(ctx.callback(|_| Message::SavedCommands(Edit::Back)))
                .content("Back to settings"),
        ),
        KeyedView::new(
            "heading",
            TextBlock::new().font_size(26.0).text("Saved commands"),
        ),
    ];
    if let Some(draft) = &editor.draft {
        rows.extend([
            KeyedView::new("name-label", TextBlock::new().text("Command name")),
            KeyedView::new(
                "name",
                TextBox::new()
                    .automation_name("Command name")
                    .text(&draft.entry.name)
                    .on_text_changed(ctx.callback(|text| Message::SavedCommands(Edit::Name(text)))),
            ),
            KeyedView::new("text-label", TextBlock::new().text("Command text")),
            KeyedView::new(
                "text",
                TextBox::new()
                    .automation_name("Command text")
                    .text(&draft.entry.text)
                    .on_text_changed(ctx.callback(|text| Message::SavedCommands(Edit::Text(text)))),
            ),
            KeyedView::new(
                "actions",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(
                                !draft.entry.name.trim().is_empty()
                                    && !draft.entry.text.trim().is_empty(),
                            )
                            .on_click(ctx.callback(|_| Message::SavedCommands(Edit::Save)))
                            .content("Save command"),
                        Button::new()
                            .on_click(ctx.callback(|_| Message::SavedCommands(Edit::Cancel)))
                            .content("Cancel"),
                    )),
            ),
        ]);
        if draft.original.is_some() {
            rows.push(KeyedView::new(
                "remove",
                Button::new()
                    .on_click(ctx.callback(|_| Message::SavedCommands(Edit::Remove)))
                    .content("Remove command"),
            ));
        }
    } else {
        rows.extend([
            KeyedView::new("description", TextBlock::new()
                .text("Find a saved command in the command palette. It inserts text into the focused shell for you to review before pressing Enter.")
                .text_wrapping(TextWrapping::Wrap)),
            KeyedView::new("add", Button::new()
                .is_enabled(editor.error.is_none() && commands.entries().len() < MAX_COMMANDS)
                .on_click(ctx.callback(|_| Message::SavedCommands(Edit::Open(None))))
                .content("Add command")),
        ]);
        for (index, entry) in commands.entries().iter().enumerate() {
            let original = entry.clone();
            rows.push(KeyedView::new(
                format!("command-{index}"),
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(16.0)
                    .children((
                        StackPanel::new().children((
                            TextBlock::new().text(&entry.name),
                            TextBlock::new()
                                .text(&entry.text)
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                        Button::new()
                            .grid_column(1)
                            .automation_name(format!("Edit command {}", entry.name))
                            .on_click(ctx.callback(move |_| {
                                Message::SavedCommands(Edit::Open(Some(original.clone())))
                            }))
                            .content("Edit…"),
                    )),
            ));
        }
    }
    if let Some(error) = &editor.error {
        rows.push(KeyedView::new(
            "error",
            TextBlock::new()
                .text(error)
                .text_wrapping(TextWrapping::Wrap),
        ));
    }
    ScrollViewer::new().content(
        StackPanel::new()
            .margin(32.0)
            .max_width(660.0)
            .spacing(10.0)
            .keyed_children(rows),
    )
}
