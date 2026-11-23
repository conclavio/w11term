use super::{Action as HostAction, *};
use crate::app::saved_commands;
use w11term::saved_commands::{Commands, Entry, valid_command_text};

#[derive(Default)]
pub(super) struct State {
    form: Option<Form>,
    next_form: u64,
    feedback: String,
}

struct Form {
    id: u64,
    text: Arc<str>,
    name: String,
}

pub enum Action {
    Begin(Arc<str>),
    Form(u64, FormAction),
}

pub enum FormAction {
    Name(String),
    Save,
    Cancel,
}

impl State {
    pub(super) fn update(
        &mut self,
        action: Action,
        current: &str,
        create: impl FnOnce(Entry) -> anyhow::Result<Commands>,
    ) -> Option<Commands> {
        match action {
            Action::Begin(text) => {
                if self.form.is_none() && text.as_ref() == current && valid_command_text(&text) {
                    self.next_form += 1;
                    self.form = Some(Form {
                        id: self.next_form,
                        text,
                        name: String::new(),
                    });
                    self.feedback.clear();
                }
            }
            Action::Form(id, action) => {
                let form = self.form.as_mut().filter(|form| form.id == id)?;
                match action {
                    FormAction::Name(name) => {
                        form.name = name;
                        self.feedback.clear();
                    }
                    FormAction::Cancel => {
                        self.form = None;
                        self.feedback.clear();
                    }
                    FormAction::Save if !form.name.trim().is_empty() => {
                        let entry = Entry {
                            name: form.name.clone(),
                            text: form.text.to_string(),
                        };
                        match create(entry) {
                            Ok(commands) => {
                                self.feedback = format!("Saved command · {}", form.name.trim());
                                self.form = None;
                                return Some(commands);
                            }
                            Err(error) => self.feedback = format!("{error:#}"),
                        }
                    }
                    FormAction::Save => {}
                }
            }
        }
        None
    }

    pub(super) fn button(&self, tab: TabId, draft: &str, ctx: &ViewContext<TerminalApp>) -> View {
        let text: Arc<str> = draft.into();
        Button::new()
            .is_enabled(self.form.is_none() && valid_command_text(draft))
            .on_click(ctx.callback(move |_| {
                Message::HostGroup(HostAction::SaveCommand(tab, Action::Begin(text.clone())))
            }))
            .content("Save as command…")
    }

    pub(super) fn view(&self, tab: TabId, ctx: &ViewContext<TerminalApp>) -> View {
        let feedback = if self.feedback.is_empty() {
            View::empty()
        } else {
            TextBlock::new()
                .text(&self.feedback)
                .text_wrapping(TextWrapping::Wrap)
                .into()
        };
        let Some(form) = &self.form else {
            return feedback;
        };
        let id = form.id;
        let message = move |action| {
            Message::HostGroup(HostAction::SaveCommand(tab, Action::Form(id, action)))
        };
        StackPanel::new().spacing(6.0).children((
            TextBlock::new().text("Command to save"),
            ScrollViewer::new().max_height(72.0).content(
                TextBlock::new()
                    .is_text_selection_enabled(true)
                    .text_wrapping(TextWrapping::Wrap)
                    .text(form.text.as_ref()),
            ),
            saved_commands::name_form(
                &form.name,
                ctx.callback(move |name| message(FormAction::Name(name))),
                ctx.callback(move |_| message(FormAction::Save)),
                ctx.callback(move |_| message(FormAction::Cancel)),
            ),
            feedback,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_the_opened_text_and_rejects_stale_forms_and_duplicate_names() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("saved-commands.json");
        let create = |entry| Commands::update(&path, None, Some(entry));
        create(Entry {
            name: "Existing".into(),
            text: "date".into(),
        })
        .unwrap();
        let mut naming = State::default();
        naming.update(Action::Begin("old".into()), "changed", create);
        assert!(naming.form.is_none());
        naming.update(
            Action::Begin("original  spacing".into()),
            "original  spacing",
            create,
        );
        let canceled = naming.form.as_ref().unwrap().id;
        naming.update(Action::Form(canceled, FormAction::Cancel), "", create);
        naming.update(
            Action::Begin("original  spacing".into()),
            "original  spacing",
            create,
        );
        let active = naming.form.as_ref().unwrap().id;
        naming.update(
            Action::Form(canceled, FormAction::Name("stale".into())),
            "",
            create,
        );
        assert!(naming.form.as_ref().unwrap().name.is_empty());

        naming.update(
            Action::Form(active, FormAction::Name("existing".into())),
            "",
            create,
        );
        assert!(
            naming
                .update(Action::Form(active, FormAction::Save), "edited", create)
                .is_none()
        );
        assert!(naming.feedback.contains("already exists"));
        naming.update(
            Action::Form(active, FormAction::Name("New command".into())),
            "",
            create,
        );
        let entries = naming
            .update(Action::Form(active, FormAction::Save), "edited", create)
            .unwrap();
        assert_eq!(
            entries.entries(),
            &[
                Entry {
                    name: "Existing".into(),
                    text: "date".into()
                },
                Entry {
                    name: "New command".into(),
                    text: "original  spacing".into()
                },
            ]
        );
        assert!(naming.form.is_none());
    }
}
