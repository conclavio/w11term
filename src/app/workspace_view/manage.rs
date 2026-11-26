use super::*;
use w11term::workspace::WorkspaceEntry;

pub(super) struct Editor {
    original: String,
    entry: WorkspaceEntry,
    name: String,
    confirm_removal: bool,
}

#[derive(Clone)]
pub enum Action {
    Open(String),
    Name(String),
    Rename,
    ReviewRemoval,
    CancelRemoval,
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
                            confirm_removal: false,
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
                    Action::Name(name) => {
                        editor.name = name;
                        return;
                    }
                    Action::ReviewRemoval | Action::CancelRemoval => {
                        editor.confirm_removal = matches!(action, Action::ReviewRemoval);
                        return;
                    }
                    Action::Rename if !editor.confirm_removal => {
                        Some(editor.name.trim().to_owned())
                    }
                    Action::Remove if editor.confirm_removal => None,
                    _ => return,
                };
                let result = WorkspaceStore::open().and_then(|store| match &change {
                    Some(name) => store.rename(&editor.original, name, &editor.entry),
                    None => store.remove(&editor.original, &editor.entry),
                });
                match result {
                    Ok(names) => {
                        if self.name.trim() == editor.original {
                            self.name = change.clone().unwrap_or_default();
                        }
                        self.status = match change {
                            Some(name) => {
                                self.query.clear();
                                format!("Renamed {} to {name}", editor.original)
                            }
                            None => format!("Removed saved workspace {}", editor.original),
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
    let content = if editor.confirm_removal {
        panel.children((
            heading.text(format!("Remove saved workspace {}?", editor.original)),
            TextBlock::new()
                .text("This removes the saved layout. Open windows and their sessions stay open.")
                .text_wrapping(TextWrapping::Wrap),
            buttons.children((
                Button::new()
                    .on_click(click(Action::CancelRemoval))
                    .content("Cancel"),
                Button::new()
                    .on_click(click(Action::Remove))
                    .content("Remove workspace"),
            )),
            status,
        ))
    } else {
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
                    .on_click(click(Action::ReviewRemoval))
                    .content("Remove saved workspace…"),
                Button::new().on_click(click(Action::Back)).content("Back"),
            )),
            status,
        ))
    };
    ScrollViewer::new().content(content)
}
