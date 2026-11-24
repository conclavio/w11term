use super::{Action as HostAction, *};
use anyhow::{Result, ensure};

#[derive(Default)]
pub(super) struct SavedUi {
    selected: Option<String>,
    editor: Option<Editor>,
    next_editor: u64,
    pub(super) feedback: String,
}

struct Editor {
    id: u64,
    original: Option<(String, Vec<String>)>,
    name: String,
    members: Vec<String>,
    confirm_delete: bool,
    without_profile: usize,
}

#[derive(Default)]
struct Selection {
    members: Vec<String>,
    without_profile: usize,
}

impl Selection {
    fn capture(
        roster: &[Target],
        selected: &BTreeSet<u64>,
        available: impl Fn(&str) -> bool,
    ) -> Self {
        let mut seen = BTreeSet::new();
        let mut selection = Self::default();
        for target in roster
            .iter()
            .filter(|target| selected.contains(&target.pane))
        {
            match target.profile.as_deref().filter(|name| available(name)) {
                Some(name) if seen.insert(name) => selection.members.push(name.into()),
                Some(_) => {}
                None => selection.without_profile += 1,
            }
        }
        selection
    }
}

impl SavedUi {
    pub(super) fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    fn name<'a>(&'a self, catalog: &'a Profiles) -> Option<&'a str> {
        self.selected
            .as_deref()
            .filter(|name| catalog.ssh_host_groups.contains_key(*name))
            .or_else(|| {
                catalog
                    .ssh_host_groups
                    .first_key_value()
                    .map(|(name, _)| name.as_str())
            })
    }

    fn edit(&mut self, original: Option<(String, Vec<String>)>, selection: Selection) {
        self.next_editor += 1;
        let (name, members) = original
            .clone()
            .unwrap_or_else(|| (String::new(), selection.members));
        self.editor = Some(Editor {
            id: self.next_editor,
            original,
            name,
            members,
            confirm_delete: false,
            without_profile: selection.without_profile,
        });
        self.feedback.clear();
    }
}

pub enum Action {
    Select(String),
    New,
    NewFromSelection(BTreeSet<u64>),
    Edit(String),
    Open { name: String, members: Vec<String> },
    Draft(u64, DraftAction),
}

pub enum DraftAction {
    Name(String),
    Member(String, bool),
    Save,
    Delete,
    ConfirmDelete,
    Cancel,
}

impl TerminalApp {
    pub(super) fn handle_saved_host_group(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        match action {
            Action::Select(name) => {
                if self.catalog.ssh_host_groups.contains_key(&name)
                    && let Some(page) = self.host_group_mut(id)
                {
                    page.saved.selected = Some(name);
                    page.saved.feedback.clear();
                }
            }
            Action::New => {
                self.refresh_profiles();
                if let Some(page) = self.host_group_mut(id) {
                    page.saved.edit(None, Selection::default());
                }
            }
            Action::NewFromSelection(offered) => self.new_saved_group_from_selection(id, offered),
            Action::Edit(name) => {
                self.refresh_profiles();
                let original = self
                    .catalog
                    .ssh_host_groups
                    .get(&name)
                    .cloned()
                    .map(|members| (name, members));
                if let Some(page) = self.host_group_mut(id) {
                    if let Some(original) = original {
                        page.saved.edit(Some(original), Selection::default());
                    } else {
                        page.saved.feedback = "This SSH group was removed.".into();
                    }
                }
            }
            Action::Open { name, members } => {
                if self
                    .host_group_mut(id)
                    .is_some_and(|page| page.saved.editor.is_none())
                    && let Err(error) = self.open_saved_host_group(Some(id), &name, &members, ctx)
                {
                    self.refresh_profiles();
                    if let Some(page) = self.host_group_mut(id) {
                        page.saved.feedback = format!("{error:#}");
                    }
                }
            }
            Action::Draft(editor, action) => self.edit_saved_host_group(id, editor, action),
        }
    }

    fn new_saved_group_from_selection(&mut self, id: TabId, offered: BTreeSet<u64>) {
        if offered.is_empty() || self.host_group_command_target() != Some(id) {
            return;
        }
        self.refresh_profiles();
        let roster = self.host_group_targets();
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        if page.selected_shown(&roster) != offered {
            page.saved.feedback = "The shown selection changed. Try again.".into();
            return;
        }
        let selection = Selection::capture(&roster, &offered, |name| {
            self.catalog.ssh_host_group_member(name).is_some()
        });
        let profile_error = self.profile_error.clone();
        if let Some(page) = self.host_group_mut(id) {
            if let Some(error) = profile_error {
                page.saved.feedback = format!("Could not read SSH profiles. {error}");
            } else {
                page.saved.edit(None, selection);
            }
        }
    }

    fn edit_saved_host_group(&mut self, id: TabId, editor: u64, action: DraftAction) {
        let member_allowed = match &action {
            DraftAction::Member(name, true) => self.catalog.ssh_host_group_member(name).is_some(),
            _ => true,
        };
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let Some(draft) = page
            .saved
            .editor
            .as_mut()
            .filter(|draft| draft.id == editor)
        else {
            return;
        };
        let (name, members, original, delete) = match action {
            DraftAction::Name(name) => {
                draft.name = name;
                draft.confirm_delete = false;
                return;
            }
            DraftAction::Member(name, checked) => {
                if checked {
                    if member_allowed && !draft.members.contains(&name) {
                        draft.members.push(name);
                    }
                } else {
                    draft.members.retain(|member| member != &name);
                }
                draft.confirm_delete = false;
                return;
            }
            DraftAction::Cancel => {
                page.saved.editor = None;
                page.saved.feedback.clear();
                return;
            }
            DraftAction::Delete => {
                draft.confirm_delete = draft.original.is_some();
                return;
            }
            DraftAction::ConfirmDelete if !draft.confirm_delete => return,
            DraftAction::Save | DraftAction::ConfirmDelete => (
                draft.name.clone(),
                draft.members.clone(),
                draft.original.clone(),
                matches!(action, DraftAction::ConfirmDelete),
            ),
        };
        let original = original
            .as_ref()
            .map(|(name, members)| (name.as_str(), members.as_slice()));
        let result = if delete {
            original
                .context("Only saved SSH groups can be deleted")
                .and_then(|(old, expected)| self.store.delete_ssh_host_group(old, expected))
        } else {
            self.store
                .save_ssh_host_group(name.trim(), &members, original)
        };
        match result {
            Ok(catalog) => {
                self.use_group_catalog(catalog);
                if let Some(page) = self.host_group_mut(id) {
                    page.saved.selected = (!delete).then(|| name.trim().to_owned());
                    page.saved.editor = None;
                    page.saved.feedback = if delete {
                        "SSH group deleted".into()
                    } else {
                        "SSH group saved".into()
                    };
                }
            }
            Err(error) => {
                self.refresh_profiles();
                if let Some(page) = self.host_group_mut(id) {
                    page.saved.feedback = format!("{error:#}");
                }
            }
        }
    }

    pub(super) fn use_group_catalog(&mut self, catalog: Profiles) {
        self.catalog = catalog;
        self.profile_error = None;
        self.header.borrow_mut().take();
        self.bind_folder_shares();
    }

    fn open_saved_host_group(
        &mut self,
        id: Option<TabId>,
        name: &str,
        expected: &[String],
        ctx: &ComponentContext<Self>,
    ) -> Result<()> {
        if let Some(id) = id
            && let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page)
        {
            ensure!(
                !page.saved.is_editing(),
                "Finish editing the SSH group before opening another group."
            );
            ensure!(
                !page.import.is_open(),
                "Finish reviewing the host list before opening another group."
            );
            ensure!(
                !page.prompt_setup.is_open(),
                "Finish prompt setup before opening another group."
            );
            ensure!(
                !page.upload.is_open(),
                "Return from the group upload before opening another group."
            );
        }
        let catalog = self.store.catalog()?;
        ensure!(
            catalog.ssh_host_groups.get(name).map(Vec::as_slice) == Some(expected),
            "The SSH group changed on disk. Select it again."
        );
        let members = catalog
            .resolve_ssh_host_group_members(expected)?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        self.use_group_catalog(catalog);
        let id = id.unwrap_or_else(|| self.ensure_host_group_tab());
        self.start_host_group_panes(id, expected.iter().cloned().map(Some).zip(members), ctx);
        if let Some(page) = self.host_group_mut(id) {
            page.saved.selected = Some(name.into());
            page.saved.feedback = format!("Opened {} SSH panes for {name}", expected.len());
        }
        Ok(())
    }

    pub(super) fn open_host_group_from_palette(
        &mut self,
        name: &str,
        members: &[String],
        ctx: &ComponentContext<Self>,
    ) {
        if let Err(error) = self.open_saved_host_group(self.host_group_tab_id(), name, members, ctx)
        {
            self.refresh_profiles();
            self.window_error = Some(format!("{error:#}"));
        }
    }

    pub(super) fn saved_host_group_toolbar(
        &self,
        id: TabId,
        page: &HostGroup,
        ctx: &ViewContext<Self>,
    ) -> View {
        let saved = &page.saved;
        let names: Vec<_> = self.catalog.ssh_host_groups.keys().cloned().collect();
        let selected = saved.name(&self.catalog);
        let selection = selected.and_then(|name| names.iter().position(|item| item == name));
        let members = selected.and_then(|name| self.catalog.ssh_host_groups.get(name));
        let action = move |action| Message::HostGroup(HostAction::Saved(id, action));
        let mut items = Vec::new();
        if let Some((name, members)) = selected.zip(members) {
            let choices = names.clone();
            let offered = (name.to_owned(), members.clone());
            let edit = name.to_owned();
            items.push(KeyedView::new(
                "choose",
                ComboBox::new()
                    .automation_name("Saved SSH groups")
                    .max_width(240.0)
                    .items_source(names)
                    .selected_index(selection)
                    .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                        index
                            .and_then(|index| choices.get(index))
                            .map_or(Message::ControlIdle, |name| {
                                action(Action::Select(name.clone()))
                            })
                    })),
            ));
            items.push(KeyedView::new(
                "open",
                Button::new()
                    .is_enabled(!members.is_empty())
                    .on_click(ctx.callback(move |_| {
                        action(Action::Open {
                            name: offered.0.clone(),
                            members: offered.1.clone(),
                        })
                    }))
                    .content("Open group"),
            ));
            items.push(KeyedView::new(
                "edit",
                Button::new()
                    .on_click(ctx.callback(move |_| action(Action::Edit(edit.clone()))))
                    .content("Edit group"),
            ));
        }
        items.push(KeyedView::new(
            "new",
            Button::new()
                .on_click(ctx.callback(move |_| action(Action::New)))
                .content("New group"),
        ));
        items.push(KeyedView::new("import", import::State::button(id, ctx)));
        let members = members
            .map(|members| {
                if members.is_empty() {
                    "This group has no SSH profiles.".into()
                } else {
                    members.join(", ")
                }
            })
            .unwrap_or_default();
        let mut details = vec![KeyedView::new(
            "actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .keyed_children(items),
        )];
        if !members.is_empty() {
            details.push(KeyedView::new(
                "members",
                TextBlock::new()
                    .text(members)
                    .text_trimming(TextTrimming::CharacterEllipsis),
            ));
        }
        if !saved.feedback.is_empty() {
            details.push(KeyedView::new(
                "feedback",
                TextBlock::new()
                    .text(&saved.feedback)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if !page.import.feedback.is_empty() {
            details.push(KeyedView::new(
                "import-feedback",
                TextBlock::new()
                    .text(&page.import.feedback)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        StackPanel::new()
            .grid_row(1)
            .spacing(4.0)
            .keyed_children(details)
    }

    pub(super) fn saved_host_group_editor(
        &self,
        id: TabId,
        saved: &SavedUi,
        ctx: &ViewContext<Self>,
    ) -> Option<View> {
        let draft = saved.editor.as_ref()?;
        let editor = draft.id;
        let action =
            move |change| Message::HostGroup(HostAction::Saved(id, Action::Draft(editor, change)));
        let mut choices: Vec<_> = self
            .catalog
            .profile_order
            .iter()
            .filter(|name| self.catalog.ssh_host_group_member(name).is_some())
            .collect();
        choices.extend(
            draft
                .members
                .iter()
                .filter(|name| self.catalog.ssh_host_group_member(name).is_none()),
        );
        let mut unique = BTreeSet::new();
        let rows: Vec<_> = choices
            .into_iter()
            .filter(|name| unique.insert(name.as_str()))
            .map(|name| {
                let label = if self.catalog.ssh_host_group_member(name).is_some() {
                    name.clone()
                } else {
                    format!("{name} · unavailable")
                };
                let item = name.clone();
                KeyedView::new(
                    name.as_str(),
                    CheckBox::new()
                        .automation_name(format!("Group member {name}"))
                        .is_checked(draft.members.contains(name))
                        .on_is_checked_changed(ctx.callback(move |checked| {
                            action(DraftAction::Member(item.clone(), checked))
                        }))
                        .content(label),
                )
            })
            .collect();
        let mut buttons = vec![
            KeyedView::new(
                "save",
                Button::new()
                    .style(ButtonStyle::Accent)
                    .is_enabled(!draft.name.trim().is_empty() && !draft.members.is_empty())
                    .on_click(ctx.callback(move |_| action(DraftAction::Save)))
                    .content("Save group"),
            ),
            KeyedView::new(
                "cancel",
                Button::new()
                    .on_click(ctx.callback(move |_| action(DraftAction::Cancel)))
                    .content("Cancel"),
            ),
        ];
        if draft.original.is_some() {
            let confirm = draft.confirm_delete;
            buttons.push(KeyedView::new(
                "delete",
                Button::new()
                    .on_click(ctx.callback(move |_| {
                        action(if confirm {
                            DraftAction::ConfirmDelete
                        } else {
                            DraftAction::Delete
                        })
                    }))
                    .content(if confirm {
                        "Confirm delete group"
                    } else {
                        "Delete group"
                    }),
            ));
        }
        let choices = if rows.is_empty() {
            TextBlock::new()
                .text("Save a direct SSH profile to use it in a group.")
                .into()
        } else {
            StackPanel::new().spacing(4.0).keyed_children(rows)
        };
        let selection_notice = match draft.without_profile {
            0 => View::empty(),
            count => {
                let pane = if count == 1 { "pane has" } else { "panes have" };
                TextBlock::new()
                    .text(format!(
                        "{count} selected {pane} no available saved SSH profile."
                    ))
                    .text_wrapping(TextWrapping::Wrap)
                    .into()
            }
        };
        Some(
            Grid::new()
                .margin(Thickness::uniform(16.0))
                .rows([
                    GridLength::Auto,
                    GridLength::Auto,
                    GridLength::STAR,
                    GridLength::Auto,
                ])
                .children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text(if draft.original.is_some() {
                            "Edit SSH group"
                        } else {
                            "New SSH group"
                        }),
                    TextBox::new()
                        .grid_row(1)
                        .automation_name("SSH group name")
                        .placeholder_text("Group name")
                        .text(&draft.name)
                        .on_text_changed(ctx.callback(move |name| action(DraftAction::Name(name)))),
                    ScrollViewer::new().grid_row(2).content(choices),
                    StackPanel::new().grid_row(3).spacing(5.0).children((
                        selection_notice,
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .keyed_children(buttons),
                        TextBlock::new()
                            .text(&saved.feedback)
                            .text_wrapping(TextWrapping::Wrap),
                    )),
                )),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_selection_uses_roster_order_and_deduplicates_available_profiles() {
        let target = |pane, profile: Option<&str>| Target {
            pane,
            name: format!("Pane {pane}"),
            names: Default::default(),
            profile: profile.map(str::to_owned),
            connected: true,
            state: TargetState::InUse,
        };
        let roster = [
            target(19, Some("West")),
            target(2, Some("East")),
            target(5, Some("West")),
            target(7, None),
            target(8, Some("Old")),
            target(9, Some("Hidden")),
        ];
        let shown_selected = BTreeSet::from([19, 2, 5, 7, 8]);
        let selection = Selection::capture(&roster, &shown_selected, |name| name != "Old");
        assert_eq!(selection.members, ["West", "East"]);
        assert_eq!(selection.without_profile, 2);
    }
}
