use super::*;
use std::io::Write;
use w11term::profiles::{SavedSshGroup, encode_group_file, read_group_file};

pub enum Action {
    Export(String),
    Open,
    Loaded(u64, Result<Option<SavedSshGroup>, String>),
    Edit(u64, Edit),
}

pub enum Edit {
    Name(String),
    Refresh,
    Save,
    Cancel,
}

pub(super) enum State {
    Reading(u64),
    Review(Box<Review>),
}

pub(super) struct Review {
    revision: u64,
    group: SavedSshGroup,
    name: String,
    matches: Vec<Option<SshOptions>>,
}

impl Review {
    fn refresh(&mut self, catalog: &Profiles) {
        self.matches = self
            .group
            .members
            .iter()
            .map(|name| catalog.ssh_host_group_member(name).cloned())
            .collect();
    }

    fn options(&self) -> Option<Vec<SshOptions>> {
        self.matches.iter().cloned().collect()
    }
}

impl State {
    pub(super) fn view(&self, id: TabId, feedback: &str, ctx: &ViewContext<TerminalApp>) -> View {
        let Self::Review(review) = self else {
            return TextBlock::new()
                .margin(16.0)
                .text("Opening SSH group definition…")
                .into();
        };
        let revision = review.revision;
        let action = move |change| message(id, Action::Edit(revision, change));
        let rows = review
            .group
            .members
            .iter()
            .zip(&review.matches)
            .map(|(name, options)| {
                let address = options.as_ref().map_or_else(
                    || "Local SSH profile unavailable".into(),
                    |ssh| address::display(&ssh.user, &ssh.host, ssh.port),
                );
                KeyedView::new(
                    name.clone(),
                    TextBlock::new()
                        .text(format!("{name} · {address}"))
                        .text_wrapping(TextWrapping::Wrap),
                )
            });
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(8.0).children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text("Import SSH group definition"),
                    TextBlock::new()
                        .text("Review the local SSH profiles that the group will use.")
                        .text_wrapping(TextWrapping::Wrap),
                    TextBox::new()
                        .automation_name("Imported SSH group name")
                        .placeholder_text("SSH group name")
                        .text(&review.name)
                        .on_text_changed(ctx.callback(move |name| action(Edit::Name(name)))),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .is_enabled(
                                    !review.name.trim().is_empty()
                                        && review.matches.iter().all(Option::is_some),
                                )
                                .on_click(ctx.callback(move |_| action(Edit::Save)))
                                .content("Save imported group"),
                            Button::new()
                                .on_click(ctx.callback(move |_| action(Edit::Refresh)))
                                .content("Refresh local profiles"),
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
                ScrollViewer::new()
                    .grid_row(1)
                    .content(StackPanel::new().keyed_children(rows)),
            ))
    }
}

fn message(id: TabId, action: Action) -> Message {
    Message::HostGroup(HostAction::Saved(id, super::Action::Definition(action)))
}

pub(super) fn buttons(
    id: TabId,
    selected: Option<&str>,
    export_busy: bool,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let export = selected.map_or_else(View::empty, |name| {
        let name = name.to_owned();
        Button::new()
            .is_enabled(!export_busy)
            .on_click(ctx.callback(move |_| message(id, Action::Export(name.clone()))))
            .content("Export group definition…")
    });
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0)
        .children((
            export,
            Button::new()
                .on_click(ctx.callback(move |_| message(id, Action::Open)))
                .content("Import group definition…"),
        ))
}

impl TerminalApp {
    pub(super) fn handle_group_definition(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Export(name) => {
                if self.export.pending.is_some() || self.host_group_command_target() != Some(id) {
                    return;
                }
                let encoded = self.store.catalog().and_then(|catalog| {
                    let group = SavedSshGroup::capture(&catalog, &name)
                        .ok_or_else(|| anyhow::anyhow!("This SSH group was removed."))?;
                    encode_group_file(&group)
                });
                match encoded {
                    Ok(text) => {
                        if let Some(page) = self.host_group_mut(id) {
                            page.saved.feedback.clear();
                        }
                        self.begin_text_export(
                            text_export::Subject::SshGroupDefinition,
                            move |file| file.write_all(text.as_bytes()),
                            ctx,
                        );
                    }
                    Err(error) => {
                        if let Some(page) = self.host_group_mut(id) {
                            page.saved.feedback = format!("{error:#}");
                        }
                    }
                }
            }
            Action::Open => self.open_group_definition(id, ctx),
            Action::Loaded(revision, result) => self.group_definition_loaded(id, revision, result),
            Action::Edit(revision, edit) => self.edit_group_definition(id, revision, edit),
        }
    }

    fn open_group_definition(&mut self, id: TabId, ctx: &ComponentContext<Self>) {
        if self.host_group_command_target() != Some(id) {
            return;
        }
        let owner = self.keyboard.hwnd().map(|window| window.0 as usize);
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let Some(owner) = owner else {
            page.saved.feedback = "The window is unavailable.".into();
            return;
        };
        page.saved.next_editor += 1;
        let revision = page.saved.next_editor;
        page.saved.definition = Some(State::Reading(revision));
        page.saved.feedback.clear();
        ctx.spawn_background_with_rejection(
            move |cancel| {
                let read = || -> Result<Option<SavedSshGroup>> {
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    let selection =
                        file_picker::pick(owner, file_picker::Purpose::OpenSshGroupDefinition)?;
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    selection
                        .map(|file| read_group_file(&file.path))
                        .transpose()
                };
                message(
                    id,
                    Action::Loaded(revision, read().map_err(|error| format!("{error:#}"))),
                )
            },
            message(
                id,
                Action::Loaded(revision, Err("The file picker is unavailable.".into())),
            ),
        );
    }

    fn group_definition_loaded(
        &mut self,
        id: TabId,
        revision: u64,
        result: Result<Option<SavedSshGroup>, String>,
    ) {
        let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
            return;
        };
        let saved = &mut page.saved;
        if !matches!(saved.definition, Some(State::Reading(pending)) if pending == revision) {
            return;
        }
        saved.definition = None;
        let result = result.and_then(|group| {
            group
                .map(|group| {
                    let catalog = self.store.catalog().map_err(|error| format!("{error:#}"))?;
                    let mut review = Review {
                        revision,
                        name: group.name.clone(),
                        group,
                        matches: Vec::new(),
                    };
                    review.refresh(&catalog);
                    Ok(review)
                })
                .transpose()
        });
        match result {
            Ok(Some(review)) => saved.definition = Some(State::Review(Box::new(review))),
            Ok(None) => {}
            Err(error) => saved.feedback = error,
        }
    }

    fn edit_group_definition(&mut self, id: TabId, revision: u64, edit: Edit) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
            return;
        };
        let saved = &mut page.saved;
        let Some(State::Review(review)) = &mut saved.definition else {
            return;
        };
        if review.revision != revision {
            return;
        }
        match edit {
            Edit::Name(name) => {
                review.name = name;
                saved.feedback.clear();
            }
            Edit::Cancel => {
                saved.definition = None;
                saved.feedback.clear();
            }
            Edit::Refresh => match self.store.catalog() {
                Ok(catalog) => {
                    review.refresh(&catalog);
                    saved.feedback.clear();
                }
                Err(error) => saved.feedback = format!("{error:#}"),
            },
            Edit::Save => {
                let Some(options) = review.options() else {
                    return;
                };
                let name = review.name.trim().to_owned();
                let definition = review.group.clone();
                match self
                    .store
                    .import_ssh_group_definition(&name, &definition, &options)
                {
                    Ok(catalog) => {
                        self.use_group_catalog(catalog);
                        if let Some(page) = self.host_group_mut(id) {
                            page.saved.definition = None;
                            page.saved.selected = Some(name);
                            page.saved.feedback = "SSH group imported".into();
                        }
                    }
                    Err(error) => {
                        if let Some(page) = self.host_group_mut(id) {
                            page.saved.feedback = format!("{error:#}");
                        }
                    }
                }
            }
        }
    }
}
