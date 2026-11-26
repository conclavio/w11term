use super::*;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Endpoint(TabId),
    Jump { tab: TabId, edit: u64 },
}

impl Target {
    fn tab(self) -> TabId {
        match self {
            Self::Endpoint(tab) | Self::Jump { tab, .. } => tab,
        }
    }
}

pub enum Action {
    Open(Target),
    Reply(Target, Result<Option<PathBuf>, String>),
}

pub(super) struct Pending {
    target: Target,
    original: String,
}

struct Field<'a> {
    value: &'a mut String,
    error: &'a mut Option<String>,
    saved: Option<&'a mut bool>,
}

pub(super) fn field(
    target: Target,
    value: &str,
    can_browse: bool,
    on_changed: Callback<String>,
    context: &ViewContext<TerminalApp>,
) -> View {
    let (label, browse) = match target {
        Target::Endpoint(_) => ("Private key file", "Browse for private key"),
        Target::Jump { .. } => (
            "Jump host private key file",
            "Browse for jump host private key",
        ),
    };
    StackPanel::new().spacing(5.0).children((
        TextBlock::new().text(label),
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                TextBox::new()
                    .text(value)
                    .placeholder_text("C:\\path\\to\\key or a path beside profiles.json")
                    .automation_name(label)
                    .on_text_changed(on_changed),
                Button::new()
                    .grid_column(1)
                    .automation_name(browse)
                    .is_enabled(can_browse)
                    .on_click(context.callback(move |_| Message::SshIdentity(Action::Open(target))))
                    .content("Browse…"),
            )),
    ))
}

impl TerminalApp {
    fn identity_field(&mut self, target: Target) -> Option<Field<'_>> {
        let id = target.tab();
        if self.tabs.active_id() != Some(id) {
            return None;
        }
        match &mut self.tabs.get_mut(id)?.page {
            Page::NewProfile(draft) => match target {
                Target::Endpoint(_)
                    if draft.authentication == PRIVATE_KEY_AUTH && !draft.generating =>
                {
                    Some(Field {
                        value: &mut draft.identity,
                        error: &mut draft.error,
                        saved: Some(&mut draft.saved),
                    })
                }
                Target::Jump { edit, .. } => {
                    let (value, error) = draft.jump_editor.identity_fields(edit)?;
                    Some(Field {
                        value,
                        error,
                        saved: None,
                    })
                }
                _ => None,
            },
            Page::QuickConnect(draft)
                if matches!(target, Target::Endpoint(_))
                    && draft.authentication == PRIVATE_KEY_AUTH =>
            {
                Some(Field {
                    value: &mut draft.identity,
                    error: &mut draft.error,
                    saved: None,
                })
            }
            _ => None,
        }
    }

    pub(super) fn ssh_identity_action(&mut self, action: Action, context: &ComponentContext<Self>) {
        match action {
            Action::Open(target) => {
                if self.identity_picker.is_some() {
                    return;
                }
                let owner = self.keyboard.hwnd();
                let Some(field) = self.identity_field(target) else {
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
                self.identity_picker = Some(Pending { target, original });
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
                        Message::SshIdentity(Action::Reply(target, result))
                    },
                    Message::SshIdentity(Action::Reply(
                        target,
                        Err("File picker unavailable".into()),
                    )),
                );
            }
            Action::Reply(target, result) => {
                let Some(request) = self
                    .identity_picker
                    .take_if(|request| request.target == target)
                else {
                    return;
                };
                let Some(field) = self.identity_field(target) else {
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
