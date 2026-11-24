use super::{Action as HostAction, filter::PROFILE_PAGE_SIZE, *};
use anyhow::{Result, ensure};

mod definition;
mod duplicate;
mod roster_update;
pub use roster_update::Action as RosterUpdateAction;
pub(super) use roster_update::update_button;

#[derive(Default)]
pub(super) struct SavedUi {
    selected: Option<String>,
    editor: Option<Editor>,
    duplicate: Option<Box<duplicate::Draft>>,
    definition: Option<definition::State>,
    next_editor: u64,
    pub(super) feedback: String,
    pub(super) reuse: reuse::State,
    roster_update: roster_update::State,
    roster_defaults: SshRosterDefaults,
    pub(super) recipient_sets: super::recipient_sets::State,
}

struct Editor {
    id: u64,
    original: Option<(String, Vec<String>)>,
    name: String,
    members: Vec<String>,
    filter: filter::Filter,
    page: usize,
    confirm_delete: bool,
    without_profile: usize,
}

struct Candidate<'a> {
    name: &'a str,
    address: Option<String>,
    position: Option<usize>,
}

impl Editor {
    fn matching<'a>(&'a self, catalog: &'a Profiles) -> Vec<Candidate<'a>> {
        let selected = self
            .members
            .iter()
            .enumerate()
            .map(|(index, name)| (name, Some(index)));
        let available = catalog
            .profile_order
            .iter()
            .filter(|name| catalog.ssh_host_group_member(name).is_some())
            .map(|name| (name, None));
        let mut seen = BTreeSet::new();
        selected
            .chain(available)
            .filter_map(|(name, position)| {
                if !seen.insert(name) {
                    return None;
                }
                let address = catalog
                    .ssh_host_group_member(name)
                    .map(|ssh| address::display(&ssh.user, &ssh.host, ssh.port));
                self.filter
                    .matches_profile(name, address.as_deref())
                    .then_some(Candidate {
                        name,
                        address,
                        position,
                    })
            })
            .collect()
    }

    fn check_matches(&mut self, offered: &[String], checked: bool, catalog: &Profiles) {
        if !self
            .matching(catalog)
            .iter()
            .map(|candidate| candidate.name)
            .eq(offered.iter().map(String::as_str))
        {
            return;
        }
        if checked {
            let selected: BTreeSet<_> = self.members.iter().map(String::as_str).collect();
            let added: Vec<_> = offered
                .iter()
                .filter(|name| {
                    !selected.contains(name.as_str())
                        && catalog.ssh_host_group_member(name).is_some()
                })
                .cloned()
                .collect();
            self.members.extend(added);
        } else {
            let names: BTreeSet<_> = offered.iter().collect();
            self.members.retain(|name| !names.contains(name));
        }
    }

    fn move_adjacent(&mut self, earlier: &str, later: &str) {
        if let Some(index) = self
            .members
            .windows(2)
            .position(|pair| pair[0] == earlier && pair[1] == later)
        {
            self.members.swap(index, index + 1);
        }
    }
}

#[derive(Default)]
struct Selection {
    members: Vec<String>,
    without_profile: usize,
    matched_panes: usize,
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
            selection.matched_panes += 1;
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
    pub(super) fn select_imported_group(&mut self, name: String) {
        self.selected = Some(name);
        self.feedback.clear();
    }

    pub(super) fn is_editing(&self) -> bool {
        self.editor.is_some()
            || self.duplicate.is_some()
            || self.definition.is_some()
            || self.roster_update.is_open()
            || self.recipient_sets.is_open()
    }

    pub(super) fn can_update_roster(&self) -> bool {
        self.roster_update.can_update()
    }

    pub(super) fn origin(&self) -> Option<(&str, &[String])> {
        self.roster_update.origin()
    }

    pub(super) fn name<'a>(&'a self, catalog: &'a Profiles) -> Option<&'a str> {
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
            filter: filter::Filter::default(),
            page: 0,
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
    Open {
        name: String,
        members: Vec<String>,
        new_tab: bool,
    },
    OpenWindow {
        name: String,
        members: Vec<String>,
    },
    Draft(u64, DraftAction),
    Duplicate(duplicate::Action),
    Definition(definition::Action),
    UpdateRoster(RosterUpdateAction),
    SaveRosterDefaults {
        group: String,
        defaults: SshRosterDefaults,
    },
}

pub(super) fn roster_defaults_button(
    id: TabId,
    page: &HostGroup,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let Some((name, _)) = page.saved.origin() else {
        return View::empty();
    };
    let group = name.to_owned();
    let defaults = page.roster_preferences;
    Button::new()
        .is_enabled(defaults != page.saved.roster_defaults)
        .on_click(ctx.callback(move |_| {
            Message::HostGroup(HostAction::Saved(
                id,
                Action::SaveRosterDefaults {
                    group: group.clone(),
                    defaults,
                },
            ))
        }))
        .content("Save roster defaults")
}

pub enum DraftAction {
    Name(String),
    Filter(String),
    Page(usize),
    Member(String, bool),
    CheckMatches(Vec<String>, bool),
    Move(String, String),
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
        if matches!(action, Action::New | Action::Edit(_))
            && self.host_group_command_target() != Some(id)
        {
            return;
        }
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
            Action::SaveRosterDefaults { group, defaults } => {
                self.save_host_group_roster_defaults(id, &group, defaults);
            }
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
            Action::Open {
                name,
                members,
                new_tab,
            } => {
                if self.host_group_command_target() == Some(id)
                    && let Err(error) =
                        self.open_saved_host_group((!new_tab).then_some(id), &name, &members, ctx)
                {
                    self.refresh_profiles();
                    if let Some(page) = self.host_group_mut(id) {
                        page.saved.feedback = format!("{error:#}");
                    }
                }
            }
            Action::OpenWindow { name, members } => {
                if self.host_group_command_target() != Some(id) {
                    return;
                }
                if let Err(error) = self.launch_saved_group_window(&name, &members, ctx) {
                    self.refresh_profiles();
                    if let Some(page) = self.host_group_mut(id) {
                        page.saved.feedback = format!("{error:#}");
                    }
                }
            }
            Action::Draft(editor, action) => self.edit_saved_host_group(id, editor, action),
            Action::Duplicate(action) => self.handle_duplicate_group(id, action),
            Action::Definition(action) => self.handle_group_definition(id, action, ctx),
            Action::UpdateRoster(action) => self.handle_roster_update(id, action),
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
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
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
            DraftAction::Filter(query) => {
                if draft.filter.query() != query {
                    draft.filter.set_query(query);
                    draft.page = 0;
                    draft.confirm_delete = false;
                }
                return;
            }
            DraftAction::Page(page) => {
                if page
                    < draft
                        .matching(&self.catalog)
                        .len()
                        .div_ceil(PROFILE_PAGE_SIZE)
                {
                    draft.page = page;
                }
                return;
            }
            DraftAction::Member(name, checked) => {
                if checked {
                    if self.catalog.ssh_host_group_member(&name).is_some()
                        && !draft.members.contains(&name)
                    {
                        draft.members.push(name);
                    }
                } else {
                    draft.members.retain(|member| member != &name);
                }
                draft.confirm_delete = false;
                return;
            }
            DraftAction::CheckMatches(names, checked) => {
                draft.check_matches(&names, checked, &self.catalog);
                draft.confirm_delete = false;
                return;
            }
            DraftAction::Move(earlier, later) => {
                draft.move_adjacent(&earlier, &later);
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

    pub(super) fn save_saved_group_login_limit(
        &mut self,
        id: TabId,
        name: &str,
        members: &[String],
        expected: SshRosterDefaults,
        login_limit: SshGroupParallelLimit,
    ) -> Result<()> {
        let updated = SshRosterDefaults {
            login_limit,
            ..expected
        };
        let catalog = self
            .store
            .save_ssh_roster_defaults(name, members, expected, updated)?;
        self.use_group_catalog(catalog);
        if let Some(page) = self.host_group_mut(id)
            && page.saved.origin() == Some((name, members))
        {
            page.saved.roster_defaults.login_limit = login_limit;
            page.roster_preferences.login_limit = login_limit;
        }
        Ok(())
    }

    fn save_host_group_roster_defaults(
        &mut self,
        id: TabId,
        offered_group: &str,
        offered_defaults: SshRosterDefaults,
    ) {
        if self.host_group_command_target() != Some(id) {
            return;
        }
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page) else {
            return;
        };
        let Some((name, members)) = page.saved.origin() else {
            return;
        };
        let expected = page.saved.roster_defaults;
        if name != offered_group
            || page.roster_preferences != offered_defaults
            || expected == offered_defaults
        {
            return;
        }
        let result = self
            .store
            .save_ssh_roster_defaults(name, members, expected, offered_defaults);
        let feedback = match result {
            Ok(catalog) => {
                self.use_group_catalog(catalog);
                if let Some(page) = self.host_group_mut(id) {
                    page.saved.roster_defaults = offered_defaults;
                }
                "Saved roster defaults".into()
            }
            Err(error) => format!("{error:#}"),
        };
        if let Some(page) = self.host_group_mut(id) {
            page.saved.feedback = feedback;
        }
    }

    pub(super) fn load_expected_host_group(
        &self,
        name: &str,
        expected: &[String],
    ) -> Result<Profiles> {
        let catalog = self.store.catalog()?;
        ensure!(
            catalog.ssh_host_groups.get(name).map(Vec::as_slice) == Some(expected),
            "The SSH group changed on disk. Select it again."
        );
        ensure!(!expected.is_empty(), "Choose at least one SSH profile");
        let mut seen = BTreeSet::new();
        for profile in expected {
            ensure!(
                seen.insert(profile),
                "SSH profile {profile} appears more than once"
            );
        }
        Ok(catalog)
    }

    pub(super) fn remember_group_launch(
        &mut self,
        id: TabId,
        name: &str,
        members: &[String],
        full_roster: bool,
        feedback: String,
    ) {
        let defaults = self.catalog.ssh_roster_defaults(name);
        if let Some(page) = self.host_group_by_id_mut(id) {
            if page.roster.is_some() {
                page.saved
                    .roster_update
                    .remember(name, members, full_roster);
                page.saved.roster_defaults = defaults;
                page.roster_preferences = defaults;
            }
            page.saved.selected = Some(name.into());
            page.saved.feedback = feedback;
        }
    }

    pub(in crate::app) fn check_saved_group_launch(&self) -> Result<()> {
        if let Some(Page::HostGroup(page)) = self.tabs.active().map(|tab| &tab.page) {
            ensure!(
                !page.saved.is_editing(),
                "Finish editing the SSH group before opening another group."
            );
            ensure!(
                !page.import.is_open(),
                "Finish reviewing the host list before opening another group."
            );
            ensure!(
                !page.host_list_export.is_open(),
                "Close the host list export before opening another group."
            );
            ensure!(
                !page.edit_membership_open(),
                "Finish editing live pane membership before opening another group."
            );
            ensure!(
                !page.saved.reuse.is_open(),
                "Finish reviewing the SSH group launch before opening another group."
            );
            ensure!(
                !page.prompt_setup.is_open(),
                "Finish prompt setup before opening another group."
            );
            ensure!(
                !page.transfer.is_open(),
                "Return from the group transfer before opening another group."
            );
            ensure!(
                !page.comparison.is_open(),
                "Return from the comparison before opening another group."
            );
        }
        Ok(())
    }

    fn open_saved_host_group(
        &mut self,
        id: Option<TabId>,
        name: &str,
        expected: &[String],
        ctx: &ComponentContext<Self>,
    ) -> Result<()> {
        self.check_saved_group_launch()?;
        let catalog = self.load_expected_host_group(name, expected)?;
        let members = catalog
            .resolve_ssh_host_group_members(expected)?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let login_limit = catalog.ssh_roster_defaults(name).login_limit.concurrency();
        self.use_group_catalog(catalog);
        let id = id.unwrap_or_else(|| self.new_host_group_tab(Some(name)));
        if self
            .tabs
            .get(id)
            .is_some_and(|tab| matches!(&tab.page, Page::HostGroup(page) if page.roster.is_some()))
        {
            let label = self.unique_host_group_label(&format!("SSH group · {name}"), Some(id));
            if let Some(tab) = self.tabs.get_mut(id)
                && let Page::HostGroup(page) = &mut tab.page
                && let Some(roster) = &mut page.roster
            {
                tab.label = label;
                roster.panes.clear();
                roster.show_window = false;
            }
        }
        self.start_host_group_panes(
            id,
            BTreeSet::new(),
            expected.iter().cloned().map(Some).zip(members),
            PaneLaunch::Select,
            login_limit,
            ctx,
        );
        self.remember_group_launch(
            id,
            name,
            expected,
            true,
            format!("Opened {} SSH panes for {name}", expected.len()),
        );
        Ok(())
    }

    fn launch_saved_group_window(
        &mut self,
        name: &str,
        expected: &[String],
        ctx: &ComponentContext<Self>,
    ) -> Result<()> {
        self.check_saved_group_launch()?;
        let catalog = self.load_expected_host_group(name, expected)?;
        let members = expected
            .iter()
            .cloned()
            .zip(
                catalog
                    .resolve_ssh_host_group_members(expected)?
                    .into_iter()
                    .cloned(),
            )
            .collect();
        let login_limit = catalog.ssh_roster_defaults(name).login_limit.concurrency();
        self.use_group_catalog(catalog);
        self.open_group_child_window(
            WindowGroup {
                name: name.into(),
                profiles: expected.to_vec(),
                members,
                login_limit,
            },
            ctx,
        )
    }

    pub(in crate::app) fn control_open_saved_host_group(
        &mut self,
        name: &str,
        ctx: &ComponentContext<Self>,
    ) -> Result<serde_json::Value> {
        use anyhow::Context as _;
        use serde_json::json;

        let catalog = self.store.catalog()?;
        let profiles = catalog
            .ssh_host_groups
            .get(name)
            .with_context(|| format!("Unknown saved SSH group {name:?}"))?
            .clone();
        let members = catalog
            .resolve_ssh_host_group_members(&profiles)?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let login_limit = catalog.ssh_roster_defaults(name).login_limit.concurrency();
        if self.catalog != catalog || self.profile_error.is_some() {
            self.use_group_catalog(catalog);
        }

        let previous = self.tabs.active_id();
        let id = self.new_host_group_tab(Some(name));
        let launched = self.install_host_group_panes(
            id,
            BTreeSet::new(),
            profiles.iter().cloned().map(Some).zip(members),
            PaneLaunch::Select,
            login_limit,
        );
        self.remember_group_launch(
            id,
            name,
            &profiles,
            true,
            format!("Opened {} SSH panes for {name}", launched.len()),
        );
        self.tabs.select(previous.unwrap_or(id));
        self.sync_selection(ctx);
        if previous.is_none() {
            self.activate(ctx);
        } else {
            self.publish_panes();
        }

        let members: Vec<_> = launched
            .into_iter()
            .zip(profiles)
            .map(|(pane, profile)| {
                json!({
                    "profile": profile,
                    "pane_id": pane.pane_id,
                    "tab_id": pane.tab_id,
                })
            })
            .collect();
        Ok(json!({
            "window_id": self.registry.window_id(),
            "tab_id": id,
            "name": name,
            "members": members,
        }))
    }

    pub(in crate::app) fn start_group_window(
        &mut self,
        group: &WindowGroup,
        ctx: &ComponentContext<Self>,
    ) {
        let id = self.new_host_group_tab(Some(&group.name));
        self.start_host_group_panes(
            id,
            BTreeSet::new(),
            group
                .members
                .iter()
                .cloned()
                .map(|(name, ssh)| (Some(name), ssh)),
            PaneLaunch::Select,
            group.login_limit,
            ctx,
        );
        self.remember_group_launch(
            id,
            &group.name,
            &group.profiles,
            group.members.len() == group.profiles.len(),
            format!(
                "Opened {} SSH panes for {}",
                group.members.len(),
                group.name
            ),
        );
    }

    pub(super) fn open_host_group_from_palette(
        &mut self,
        name: &str,
        members: &[String],
        ctx: &ComponentContext<Self>,
    ) {
        if let Err(error) = self.open_saved_host_group(None, name, members, ctx) {
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
        let review_actions = selected
            .zip(members)
            .map_or_else(View::empty, |(name, members)| {
                let launch = reuse::button(id, name, members, ctx);
                let duplicate = duplicate::button(id, name, ctx);
                let group_window = name.to_owned();
                let group_window_members = members.clone();
                let name = name.to_owned();
                let members = members.clone();
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        launch,
                        Button::new()
                            .is_enabled(!members.is_empty())
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(HostAction::HostListExport(
                                    id,
                                    host_list_export::Action::OpenSaved {
                                        name: name.clone(),
                                        members: members.clone(),
                                    },
                                ))
                            }))
                            .content("Export saved group hosts…"),
                        duplicate,
                        Button::new()
                            .is_enabled(!group_window_members.is_empty())
                            .on_click(ctx.callback(move |_| {
                                Message::HostGroup(HostAction::Saved(
                                    id,
                                    Action::OpenWindow {
                                        name: group_window.clone(),
                                        members: group_window_members.clone(),
                                    },
                                ))
                            }))
                            .content("Open group in new window"),
                    ))
            });
        let action = move |action| Message::HostGroup(HostAction::Saved(id, action));
        let mut items = Vec::new();
        if let Some((name, members)) = selected.zip(members) {
            let choices = names.clone();
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
            for (key, label, new_tab) in [
                ("open", "Open group", false),
                ("open-new-tab", "Open in new tab", true),
            ] {
                let name = name.to_owned();
                let members = members.clone();
                items.push(KeyedView::new(
                    key,
                    Button::new()
                        .is_enabled(!members.is_empty())
                        .on_click(ctx.callback(move |_| {
                            action(Action::Open {
                                name: name.clone(),
                                members: members.clone(),
                                new_tab,
                            })
                        }))
                        .content(label),
                ));
            }
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
        details.push(KeyedView::new(
            "review-group-actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((review_actions, import::saved_profiles_button(id, ctx))),
        ));
        details.push(KeyedView::new(
            "group-definition-actions",
            definition::buttons(id, selected, self.export.pending.is_some(), ctx),
        ));
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
        if let Some(definition) = &saved.definition {
            return Some(definition.view(id, &saved.feedback, ctx));
        }
        if let Some(draft) = &saved.duplicate {
            return Some(draft.view(id, &saved.feedback, ctx));
        }
        if let Some(review) = saved.roster_update.view(id, ctx) {
            return Some(review);
        }
        let draft = saved.editor.as_ref()?;
        let editor = draft.id;
        let action =
            move |change| Message::HostGroup(HostAction::Saved(id, Action::Draft(editor, change)));
        let matching = draft.matching(&self.catalog);
        let pages = matching.len().div_ceil(PROFILE_PAGE_SIZE).max(1);
        let page = draft.page.min(pages - 1);
        let offered: Rc<Vec<_>> = Rc::new(
            matching
                .iter()
                .map(|candidate| candidate.name.to_owned())
                .collect(),
        );
        let can_add = matching
            .iter()
            .any(|candidate| candidate.position.is_none() && candidate.address.is_some());
        let can_remove = matching
            .iter()
            .any(|candidate| candidate.position.is_some());
        let rows = matching
            .iter()
            .skip(page * PROFILE_PAGE_SIZE)
            .take(PROFILE_PAGE_SIZE)
            .map(|candidate| {
                let name = candidate.name;
                let item = name.to_owned();
                let position = candidate.position;
                let order = if let Some(position) = position {
                    let before = position
                        .checked_sub(1)
                        .and_then(|index| draft.members.get(index))
                        .cloned();
                    let after = draft.members.get(position + 1).cloned();
                    let up = name.to_owned();
                    let down = name.to_owned();
                    StackPanel::new()
                        .grid_column(1)
                        .spacing(6.0)
                        .orientation(Orientation::Horizontal)
                        .children((
                            TextBlock::new()
                                .vertical_alignment(VerticalAlignment::Center)
                                .text(format!("Position {}", position + 1)),
                            Button::new()
                                .automation_name(format!("Move {name} up"))
                                .is_enabled(before.is_some())
                                .on_click(ctx.callback(move |_| {
                                    before.as_ref().map_or(Message::ControlIdle, |before| {
                                        action(DraftAction::Move(before.clone(), up.clone()))
                                    })
                                }))
                                .content("Move up"),
                            Button::new()
                                .automation_name(format!("Move {name} down"))
                                .is_enabled(after.is_some())
                                .on_click(ctx.callback(move |_| {
                                    after.as_ref().map_or(Message::ControlIdle, |after| {
                                        action(DraftAction::Move(down.clone(), after.clone()))
                                    })
                                }))
                                .content("Move down"),
                        ))
                } else {
                    View::empty()
                };
                KeyedView::new(
                    name,
                    Grid::new()
                        .columns([GridLength::STAR, GridLength::Auto])
                        .children((
                            StackPanel::new().spacing(2.0).children((
                                CheckBox::new()
                                    .automation_name(format!("Group member {name}"))
                                    .is_checked(position.is_some())
                                    .on_is_checked_changed(ctx.callback(move |checked| {
                                        action(DraftAction::Member(item.clone(), checked))
                                    }))
                                    .content(name),
                                TextBlock::new()
                                    .text(
                                        candidate
                                            .address
                                            .as_deref()
                                            .unwrap_or("SSH profile unavailable"),
                                    )
                                    .text_wrapping(TextWrapping::Wrap),
                            )),
                            order,
                        )),
                )
            });
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
        let choices = if matching.is_empty() {
            TextBlock::new()
                .text(if draft.filter.query().trim().is_empty() {
                    "Save a direct SSH profile to use it in a group."
                } else {
                    "No SSH profiles match this search."
                })
                .into()
        } else {
            StackPanel::new().spacing(8.0).keyed_children(rows)
        };
        let add_matches = Rc::clone(&offered);
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
                .row_spacing(8.0)
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
                    StackPanel::new().grid_row(1).spacing(6.0).children((
                        TextBox::new()
                            .automation_name("SSH group name")
                            .placeholder_text("Group name")
                            .text(&draft.name)
                            .on_text_changed(
                                ctx.callback(move |name| action(DraftAction::Name(name))),
                            ),
                        TextBox::new()
                            .automation_name("Find group profiles")
                            .placeholder_text("Filter by profile name or SSH address")
                            .text(draft.filter.query())
                            .on_text_changed(
                                ctx.callback(move |query| action(DraftAction::Filter(query))),
                            ),
                        StackPanel::new()
                            .spacing(6.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                Button::new()
                                    .is_enabled(can_add)
                                    .on_click(ctx.callback(move |_| {
                                        action(DraftAction::CheckMatches(
                                            (*add_matches).clone(),
                                            true,
                                        ))
                                    }))
                                    .content("Check matches"),
                                Button::new()
                                    .is_enabled(can_remove)
                                    .on_click(ctx.callback(move |_| {
                                        action(DraftAction::CheckMatches((*offered).clone(), false))
                                    }))
                                    .content("Uncheck matches"),
                                Button::new()
                                    .is_enabled(page > 0)
                                    .on_click(ctx.callback(move |_| {
                                        action(DraftAction::Page(page.saturating_sub(1)))
                                    }))
                                    .content("Previous profiles"),
                                Button::new()
                                    .is_enabled(page + 1 < pages)
                                    .on_click(
                                        ctx.callback(move |_| action(DraftAction::Page(page + 1))),
                                    )
                                    .content("Next profiles"),
                            )),
                        TextBlock::new().text(format!(
                            "{} selected · {} matches · Page {} of {pages}",
                            draft.members.len(),
                            matching.len(),
                            page + 1
                        )),
                    )),
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
