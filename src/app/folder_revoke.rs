use super::*;

const LABEL: &str = "Revoke all shared folders…";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Confirm,
    Pending,
}

#[derive(Clone)]
pub(super) struct Editor {
    ticket: u64,
    phase: Phase,
    feedback: String,
}
impl Default for Editor {
    fn default() -> Self {
        Self {
            ticket: folder_shares::token(),
            phase: Phase::Idle,
            feedback: String::new(),
        }
    }
}
impl Editor {
    pub(super) fn cancel_confirmation(&mut self) {
        if self.phase == Phase::Confirm {
            *self = Self::default();
        }
    }
}

pub enum Action {
    Ask,
    Cancel,
    Revoke,
    Done(Result<Box<ProfileSnapshot>, String>),
}

impl TerminalApp {
    pub(super) fn folder_revoke_view(
        &self,
        id: TabId,
        base: &ProfileSnapshot,
        editor: &Editor,
        ctx: &ViewContext<Self>,
    ) -> View {
        let ticket = editor.ticket;
        if editor.phase == Phase::Confirm {
            let question = format!(
                "Revoke all shared folders for “{}”? Active shares will stop. To share a folder again, choose it from a verified SSH session.",
                base.name
            );
            let cancel = Button::new()
                .on_click(ctx.callback(move |_| Message::FolderRevoke(id, ticket, Action::Cancel)))
                .content("Keep permissions");
            let confirm = Button::new()
                .automation_name("Confirm revoke all shared folders")
                .on_click(ctx.callback(move |_| Message::FolderRevoke(id, ticket, Action::Revoke)))
                .content("Revoke all shared folders");
            return StackPanel::new()
                .spacing(5.0)
                .children((
                    TextBlock::new()
                        .text(question)
                        .text_wrapping(TextWrapping::Wrap),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((cancel, confirm)),
                ))
                .into();
        }
        let available = self.store.has_folder_permissions(base);
        let feedback = match &available {
            Err(e) => format!("Could not check folder permissions: {e:#}"),
            _ if editor.phase == Phase::Pending => "Revoking folder permissions…".into(),
            _ => editor.feedback.clone(),
        };
        let button = Button::new()
            .horizontal_alignment(HorizontalAlignment::Left)
            .is_enabled(editor.phase == Phase::Idle && matches!(available, Ok(true)))
            .on_click(ctx.callback(move |_| Message::FolderRevoke(id, ticket, Action::Ask)))
            .content(LABEL);
        if feedback.is_empty() {
            button
        } else {
            StackPanel::new()
                .spacing(5.0)
                .children((
                    button,
                    TextBlock::new()
                        .text(feedback)
                        .text_wrapping(TextWrapping::Wrap),
                ))
                .into()
        }
    }

    pub(super) fn folder_revoke(
        &mut self,
        id: TabId,
        ticket: u64,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        if !matches!(action, Action::Done(_)) && self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(AppTab {
            page: Page::NewProfile(draft),
            ..
        }) = self.tabs.get_mut(id)
        else {
            return;
        };
        let Some(base) = draft.origin.editing() else {
            return;
        };
        let editor = &mut draft.folder_revoke;
        if editor.ticket != ticket {
            return;
        }
        match action {
            Action::Ask
                if editor.phase == Phase::Idle
                    && matches!(self.store.has_folder_permissions(base), Ok(true)) =>
            {
                editor.phase = Phase::Confirm;
                editor.feedback.clear();
            }
            Action::Cancel => editor.cancel_confirmation(),
            Action::Revoke if editor.phase == Phase::Confirm => {
                let catalog = self.store.path().to_owned();
                let base = base.clone();
                editor.phase = Phase::Pending;
                ctx.spawn_background_with_rejection(
                    move |_| {
                        let result = (|| -> anyhow::Result<_> {
                            ProfileStore::new(catalog)?
                                .revoke_folder_permissions(&base, &w11term::keys::data_dir()?)
                        })();
                        Message::FolderRevoke(
                            id,
                            ticket,
                            Action::Done(result.map(Box::new).map_err(|e| format!("{e:#}"))),
                        )
                    },
                    Message::FolderRevoke(
                        id,
                        ticket,
                        Action::Done(Err(
                            "The folder permissions worker is busy. Try again.".into()
                        )),
                    ),
                );
            }
            Action::Done(result) if editor.phase == Phase::Pending => {
                // Keep all draft fields; only the persisted snapshot advanced.
                *editor = Editor::default();
                match result {
                    Ok(updated) => {
                        editor.feedback =
                            format!("Revoked all shared folders for “{}”.", updated.name);
                        draft.origin = DraftOrigin::Edit(*updated);
                    }
                    Err(e) => editor.feedback = format!("Could not revoke folder permissions: {e}"),
                }
            }
            _ => {}
        }
    }
}
