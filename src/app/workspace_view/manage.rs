use super::*;
use w11term::workspace::WorkspaceEntry;

pub(super) struct Editor {
    original: String,
    entry: WorkspaceEntry,
    name: String,
    mode: Mode,
}

enum Mode {
    Manage,
    ConfirmRemoval,
    Duplicate(String),
}

enum Change {
    Rename(String),
    Duplicate(String),
    Remove,
}

#[derive(Clone)]
pub enum Action {
    Open(String),
    Name(String),
    Rename,
    ReviewDuplicate,
    CopyName(String),
    SaveCopy,
    ReviewRemoval,
    Cancel,
    Remove,
    Back,
}

impl WorkspacePage {
    pub(super) fn manage(&mut self, action: Action) {
        match action {
            Action::Open(name) if !self.switching => {
                self.status.clear();
                match WorkspaceStore::open().and_then(|store| store.entry(&name)) {
                    Ok(entry) => {
                        self.editor = Some(Editor {
                            original: name.clone(),
                            name,
                            entry,
                            mode: Mode::Manage,
                        });
                    }
                    Err(error) => self.status = format!("{error:#}"),
                }
            }
            Action::Open(_) => {}
            Action::Back => {
                self.editor = None;
                self.reload_names();
            }
            action => {
                let Some(editor) = &mut self.editor else {
                    return;
                };
                self.status.clear();
                let change = match action {
                    Action::Name(name) if matches!(editor.mode, Mode::Manage) => {
                        editor.name = name;
                        return;
                    }
                    Action::ReviewDuplicate if matches!(editor.mode, Mode::Manage) => {
                        editor.mode = Mode::Duplicate(copy_name(&editor.original, &self.names));
                        return;
                    }
                    Action::CopyName(name) => {
                        if let Mode::Duplicate(current) = &mut editor.mode {
                            *current = name;
                        }
                        return;
                    }
                    Action::ReviewRemoval if matches!(editor.mode, Mode::Manage) => {
                        editor.mode = Mode::ConfirmRemoval;
                        return;
                    }
                    Action::Cancel => {
                        editor.mode = Mode::Manage;
                        return;
                    }
                    Action::Rename if matches!(editor.mode, Mode::Manage) => {
                        Change::Rename(editor.name.trim().to_owned())
                    }
                    Action::SaveCopy if let Mode::Duplicate(name) = &editor.mode => {
                        Change::Duplicate(name.trim().to_owned())
                    }
                    Action::Remove if matches!(editor.mode, Mode::ConfirmRemoval) => Change::Remove,
                    _ => return,
                };
                let result = WorkspaceStore::open().and_then(|store| match &change {
                    Change::Rename(name) => store.rename(&editor.original, name, &editor.entry),
                    Change::Duplicate(name) => {
                        store.duplicate(&editor.original, name, &editor.entry)
                    }
                    Change::Remove => store.remove(&editor.original, &editor.entry),
                });
                match result {
                    Ok(names) => {
                        let updates_entered_name = self.name.trim() == editor.original;
                        self.status = match change {
                            Change::Rename(name) => {
                                if updates_entered_name {
                                    self.name = name.clone();
                                }
                                self.query.clear();
                                format!("Renamed {} to {name}", editor.original)
                            }
                            Change::Duplicate(name) => {
                                self.query.clear();
                                format!("Duplicated {} as {name}", editor.original)
                            }
                            Change::Remove => {
                                if updates_entered_name {
                                    self.name.clear();
                                }
                                format!("Removed saved workspace {}", editor.original)
                            }
                        };
                        self.names = names;
                        self.editor = None;
                    }
                    Err(error) => self.status = format!("{error:#}"),
                }
            }
        }
    }
}

fn copy_name(source: &str, names: &[String]) -> String {
    let base = format!("{source} copy");
    if names.binary_search(&base).is_err() {
        return base;
    }
    (2..)
        .map(|number| format!("{base} {number}"))
        .find(|name| names.binary_search(name).is_err())
        .expect("finite workspace catalog")
}

pub(super) fn view(
    id: TabId,
    page: &WorkspacePage,
    editor: &Editor,
    context: &ViewContext<TerminalApp>,
) -> View {
    let click = |action: Action| {
        context.callback(move |_| Message::Workspace(WorkspaceAction::Manage(id, action.clone())))
    };
    let heading = TextBlock::new()
        .font_size(24.0)
        .text_wrapping(TextWrapping::Wrap);
    let buttons = StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0);
    let panel = StackPanel::new()
        .margin(24.0)
        .spacing(12.0)
        .max_width(550.0)
        .horizontal_alignment(HorizontalAlignment::Left);
    let status = TextBlock::new()
        .text(&page.status)
        .text_wrapping(TextWrapping::Wrap);
    let content = match &editor.mode {
        Mode::ConfirmRemoval => panel.children((
            heading.text(format!("Remove saved workspace {}?", editor.original)),
            TextBlock::new()
                .text("This removes the saved layout. Open windows and their sessions stay open.")
                .text_wrapping(TextWrapping::Wrap),
            buttons.children((
                Button::new()
                    .on_click(click(Action::Cancel))
                    .content("Cancel"),
                Button::new()
                    .on_click(click(Action::Remove))
                    .content("Remove workspace"),
            )),
            status,
        )),
        Mode::Duplicate(name) => panel.children((
            heading.text(format!("Duplicate saved workspace {}", editor.original)),
            TextBox::new()
                .width(340.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .automation_name("Workspace copy name")
                .text(name)
                .on_text_changed(context.callback(move |name| {
                    Message::Workspace(WorkspaceAction::Manage(id, Action::CopyName(name)))
                })),
            buttons.children((
                Button::new()
                    .on_click(click(Action::Cancel))
                    .content("Cancel"),
                Button::new()
                    .is_enabled(!name.trim().is_empty() && name.trim() != editor.original)
                    .on_click(click(Action::SaveCopy))
                    .content("Save workspace copy"),
            )),
            status,
        )),
        Mode::Manage => {
            let name = editor.name.trim();
            panel.children((
                heading.text(format!("Manage workspace {}", editor.original)),
                TextBox::new()
                    .width(340.0)
                    .horizontal_alignment(HorizontalAlignment::Left)
                    .automation_name("New workspace name")
                    .text(&editor.name)
                    .on_text_changed(context.callback(move |name| {
                        Message::Workspace(WorkspaceAction::Manage(id, Action::Name(name)))
                    })),
                buttons.children((
                    Button::new()
                        .is_enabled(!name.is_empty() && name != editor.original)
                        .on_click(click(Action::Rename))
                        .content("Rename workspace"),
                    Button::new()
                        .automation_name("Duplicate saved workspace")
                        .on_click(click(Action::ReviewDuplicate))
                        .content("Duplicate…"),
                    Button::new()
                        .on_click(click(Action::ReviewRemoval))
                        .content("Remove saved workspace…"),
                    Button::new().on_click(click(Action::Back)).content("Back"),
                )),
                status,
            ))
        }
    };
    ScrollViewer::new().content(content)
}
