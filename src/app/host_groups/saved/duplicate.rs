use super::*;
use w11term::profiles::SavedSshGroup;

pub enum Action {
    Open(String),
    Edit(u64, Edit),
}

pub enum Edit {
    Name(String),
    Save,
    Cancel,
}

pub(super) struct Draft {
    id: u64,
    source: SavedSshGroup,
    name: String,
}

impl Draft {
    pub(super) fn view(&self, tab: TabId, feedback: &str, ctx: &ViewContext<TerminalApp>) -> View {
        let id = self.id;
        let action = move |change| {
            Message::HostGroup(HostAction::Saved(
                tab,
                super::Action::Duplicate(Action::Edit(id, change)),
            ))
        };
        let sets = self.source.recipient_sets.as_ref().map_or_else(
            || "No saved recipient sets".into(),
            |sets| {
                format!(
                    "Recipient sets · {}",
                    sets.keys().cloned().collect::<Vec<_>>().join(", ")
                )
            },
        );
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(8.0).children((
                    TextBlock::new().font_size(24.0).text("Duplicate SSH group"),
                    TextBlock::new().text(format!("Copy from {}", self.source.name)),
                    TextBox::new()
                        .automation_name("New SSH group name")
                        .placeholder_text("New SSH group name")
                        .text(&self.name)
                        .on_text_changed(ctx.callback(move |name| action(Edit::Name(name)))),
                    StackPanel::new()
                        .spacing(8.0)
                        .orientation(Orientation::Horizontal)
                        .children((
                            Button::new()
                                .is_enabled(!self.name.trim().is_empty())
                                .on_click(ctx.callback(move |_| action(Edit::Save)))
                                .content("Save copy"),
                            Button::new()
                                .on_click(ctx.callback(move |_| action(Edit::Cancel)))
                                .content("Cancel"),
                        )),
                    if feedback.is_empty() {
                        View::empty()
                    } else {
                        TextBlock::new()
                            .text(feedback)
                            .text_wrapping(TextWrapping::Wrap)
                            .into()
                    },
                )),
                ScrollViewer::new().grid_row(1).content(
                    StackPanel::new().spacing(4.0).children((
                        TextBlock::new()
                            .text(format!("Profiles · {}", self.source.members.join(", ")))
                            .text_wrapping(TextWrapping::Wrap),
                        TextBlock::new()
                            .text(sets)
                            .text_wrapping(TextWrapping::Wrap),
                    )),
                ),
            ))
    }
}

impl TerminalApp {
    pub(super) fn handle_duplicate_group(&mut self, tab: TabId, action: Action) {
        let (source, name) = match action {
            Action::Open(name) => {
                if self.host_group_command_target() != Some(tab) {
                    return;
                }
                self.refresh_profiles();
                let source = SavedSshGroup::capture(&self.catalog, &name);
                if let Some(page) = self.host_group_mut(tab) {
                    let saved = &mut page.saved;
                    if let Some(source) = source {
                        saved.next_editor += 1;
                        saved.duplicate = Some(Box::new(Draft {
                            source,
                            id: saved.next_editor,
                            name: String::new(),
                        }));
                        saved.feedback.clear();
                    } else {
                        saved.feedback = "This SSH group was removed.".into();
                    }
                }
                return;
            }
            Action::Edit(id, edit) => {
                let Some(page) = self.host_group_mut(tab) else {
                    return;
                };
                let saved = &mut page.saved;
                let Some(draft) = saved.duplicate.as_mut().filter(|draft| draft.id == id) else {
                    return;
                };
                match edit {
                    Edit::Name(name) => {
                        draft.name = name;
                        saved.feedback.clear();
                        return;
                    }
                    Edit::Cancel => {
                        saved.duplicate = None;
                        saved.feedback.clear();
                        return;
                    }
                    Edit::Save => (draft.source.clone(), draft.name.trim().to_owned()),
                }
            }
        };
        match self.store.duplicate_ssh_host_group(&name, &source) {
            Ok(catalog) => {
                self.use_group_catalog(catalog);
                if let Some(page) = self.host_group_mut(tab) {
                    page.saved.duplicate = None;
                    page.saved.selected = Some(name);
                    page.saved.feedback = "SSH group copied".into();
                }
            }
            Err(error) => {
                self.refresh_profiles();
                if let Some(page) = self.host_group_mut(tab) {
                    page.saved.feedback = format!("{error:#}");
                }
            }
        }
    }
}
