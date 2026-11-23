use super::*;
use std::path::{Path, PathBuf};

pub enum Action {
    Open(TabId),
    Reply(TabId, Result<Option<PathBuf>, String>),
}

pub(super) struct Pending {
    tab: TabId,
    original: String,
}

struct Field<'a> {
    value: &'a mut String,
    error: &'a mut Option<String>,
    saved: Option<&'a mut bool>,
}

pub(super) fn field(
    id: TabId,
    value: &str,
    can_browse: bool,
    on_changed: Callback<String>,
    context: &ViewContext<TerminalApp>,
) -> View {
    StackPanel::new().spacing(5.0).children((
        TextBlock::new().text("Private key file"),
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                TextBox::new()
                    .text(value)
                    .placeholder_text("C:\\path\\to\\key or a path beside profiles.json")
                    .automation_name("Private key file")
                    .on_text_changed(on_changed),
                Button::new()
                    .grid_column(1)
                    .automation_name("Browse for private key")
                    .is_enabled(can_browse)
                    .on_click(context.callback(move |_| Message::SshIdentity(Action::Open(id))))
                    .content("Browse…"),
            )),
    ))
}

impl TerminalApp {
    fn identity_field(&mut self, id: TabId) -> Option<Field<'_>> {
        if self.tabs.active_id() != Some(id) {
            return None;
        }
        match &mut self.tabs.get_mut(id)?.page {
            Page::NewProfile(draft)
                if draft.authentication == PRIVATE_KEY_AUTH && !draft.generating =>
            {
                Some(Field {
                    value: &mut draft.identity,
                    error: &mut draft.error,
                    saved: Some(&mut draft.saved),
                })
            }
            Page::QuickConnect(draft) if draft.authentication == PRIVATE_KEY_AUTH => Some(Field {
                value: &mut draft.identity,
                error: &mut draft.error,
                saved: None,
            }),
            _ => None,
        }
    }

    pub(super) fn ssh_identity_action(&mut self, action: Action, context: &ComponentContext<Self>) {
        match action {
            Action::Open(id) => {
                if self.identity_picker.is_some() {
                    return;
                }
                let owner = self.keyboard.hwnd();
                let Some(field) = self.identity_field(id) else {
                    return;
                };
                let Some(owner) = owner else {
                    *field.error = Some("The window is unavailable.".into());
                    return;
                };
                let original = field.value.clone();
                let current = (!original.trim().is_empty()).then(|| {
                    self.store
                        .path()
                        .parent()
                        .unwrap_or(Path::new("."))
                        .join(original.trim())
                });
                self.identity_picker = Some(Pending { tab: id, original });
                let owner = owner.0 as usize;
                context.spawn_background_with_rejection(
                    move |cancel| {
                        let result = if cancel.is_cancelled() {
                            Ok(None)
                        } else {
                            file_picker::pick(owner, file_picker::Purpose::SshIdentity(current))
                                .map(|chosen| chosen.map(|file| file.path))
                                .map_err(|error| format!("{error:#}"))
                        };
                        Message::SshIdentity(Action::Reply(id, result))
                    },
                    Message::SshIdentity(Action::Reply(id, Err("File picker unavailable".into()))),
                );
            }
            Action::Reply(id, result) => {
                let Some(request) = self.identity_picker.take_if(|request| request.tab == id)
                else {
                    return;
                };
                let Some(field) = self.identity_field(id) else {
                    return;
                };
                if *field.value != request.original {
                    return;
                }
                match result {
                    Ok(Some(path)) => {
                        let Some(path) = path.to_str() else {
                            *field.error =
                                Some("The selected key path cannot be represented as text.".into());
                            return;
                        };
                        if *field.value != path {
                            *field.value = path.to_owned();
                            *field.error = None;
                            if let Some(saved) = field.saved {
                                *saved = false;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => *field.error = Some(error),
                }
            }
        }
    }
}
