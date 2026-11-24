use super::{filter::PROFILE_PAGE_SIZE, *};
use anyhow::{Context, Result, ensure};
use w11term::options::SshRecipientSets;

pub(super) const LABEL: &str = "Review group launch…";
const FRESH: &str = "Open a new SSH pane";

#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub group: String,
    pub profiles: Vec<String>,
    pub recipient_set: Option<(String, Vec<String>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    pub(super) pane: u64,
    generation: u64,
}

pub(super) struct Candidate {
    pub(super) connection: Connection,
    title: String,
}

pub(super) fn connection_picker(
    label: &str,
    candidates: &[Candidate],
    selected: Option<Connection>,
    enabled: bool,
    choose: impl Fn(Option<Connection>) -> Message + 'static,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let labels = std::iter::once(FRESH.to_owned())
        .chain(
            candidates
                .iter()
                .map(|candidate| format!("Reuse · {}", candidate.title)),
        )
        .collect::<Vec<_>>();
    let choices: Vec<_> = std::iter::once(None)
        .chain(
            candidates
                .iter()
                .map(|candidate| Some(candidate.connection)),
        )
        .collect();
    let selected = choices.iter().position(|&choice| choice == selected);
    ComboBox::new()
        .automation_name(format!("Connection for {label}"))
        .items_source(labels)
        .selected_index(selected)
        .is_enabled(enabled)
        .on_selection_changed(ctx.callback(move |choice: Option<usize>| {
            choice
                .and_then(|choice| choices.get(choice).copied())
                .map_or(Message::ControlIdle, &choose)
        }))
        .into()
}

struct Member {
    profile: String,
    address: Option<String>,
    options: Option<SshOptions>,
    candidates: Vec<Candidate>,
    choice: Option<Connection>,
    included: bool,
}

impl Member {
    fn new(profile: String, options: Option<SshOptions>, candidates: Vec<Candidate>) -> Self {
        let choice = unique_connection(&candidates);
        let included = options.is_some();
        let address = options
            .as_ref()
            .map(|ssh| address::display(&ssh.user, &ssh.host, ssh.port));
        Self {
            profile,
            address,
            options,
            candidates,
            choice,
            included,
        }
    }

    fn contains(&self, connection: Connection) -> bool {
        self.candidates
            .iter()
            .any(|candidate| candidate.connection == connection)
    }

    fn select(&mut self, choice: Option<Connection>) {
        if choice.is_none_or(|connection| self.contains(connection)) {
            self.choice = choice;
        }
    }

    fn launch(&self) -> Launch {
        if self.included {
            self.choice.map_or(Launch::Fresh, Launch::Reuse)
        } else {
            Launch::Skip
        }
    }
}

fn unique_connection(candidates: &[Candidate]) -> Option<Connection> {
    match candidates {
        [candidate] => Some(candidate.connection),
        _ => None,
    }
}

struct Review {
    name: String,
    members: Vec<Member>,
    sets: SshRecipientSets,
    roster_defaults: SshRosterDefaults,
    selected_set: Option<String>,
    feedback: String,
}

impl Review {
    fn profiles(&self) -> Vec<String> {
        self.members
            .iter()
            .map(|member| member.profile.clone())
            .collect()
    }

    fn included_profiles(&self) -> impl Iterator<Item = &str> {
        self.members
            .iter()
            .filter(|member| member.included)
            .map(|member| member.profile.as_str())
    }

    fn choices(&self) -> Vec<Launch> {
        self.members.iter().map(Member::launch).collect()
    }

    fn include(&mut self, index: usize, included: bool) {
        if let Some(member) = self.members.get_mut(index)
            && member.options.is_some()
            && member.included != included
        {
            member.included = included;
            self.selected_set = None;
            self.feedback.clear();
        }
    }

    fn select_set(&mut self, name: String) -> Result<()> {
        let profiles = self
            .sets
            .get(&name)
            .with_context(|| format!("Recipient set {name} is unavailable."))?;
        ensure!(
            self.set_is_available(profiles),
            "Recipient set {name} contains an unavailable SSH profile."
        );
        for member in &mut self.members {
            member.included = profiles.contains(&member.profile);
        }
        self.selected_set = Some(name);
        self.feedback.clear();
        Ok(())
    }

    fn set_is_available(&self, profiles: &[String]) -> bool {
        let mut seen = BTreeSet::new();
        !profiles.is_empty()
            && profiles.iter().all(|profile| {
                seen.insert(profile)
                    && self
                        .members
                        .iter()
                        .any(|member| &member.profile == profile && member.options.is_some())
            })
    }

    fn validate_profiles(&self, catalog: &Profiles) -> Result<()> {
        ensure!(
            self.members.iter().any(|member| member.included),
            "Include at least one SSH host."
        );
        if let Some(name) = &self.selected_set {
            ensure!(
                catalog
                    .ssh_recipient_sets
                    .get(&self.name)
                    .and_then(|sets| sets.get(name))
                    == self.sets.get(name),
                "Recipient set {name} changed. Refresh the review before continuing."
            );
        }
        for member in self.members.iter().filter(|member| member.included) {
            ensure!(
                member.options.as_ref().is_some_and(|expected| {
                    catalog.ssh_host_group_member(&member.profile) == Some(expected)
                }),
                "SSH profiles changed. Refresh the review before continuing."
            );
        }
        Ok(())
    }

    fn validate(
        &self,
        catalog: &Profiles,
        connection: impl Fn(Connection, &str, &SshOptions) -> bool,
    ) -> Result<()> {
        self.validate_profiles(catalog)?;
        for member in self.members.iter().filter(|member| member.included) {
            if let (Some(approved), Some(expected)) = (member.choice, &member.options) {
                ensure!(
                    connection(approved, &member.profile, expected),
                    "The reviewed SSH connection for {} changed. Refresh the review or choose a new pane.",
                    member.profile
                );
            }
        }
        Ok(())
    }

    fn window_members(&self, catalog: &Profiles) -> Result<Vec<(String, SshOptions)>> {
        self.validate_profiles(catalog)?;
        self.members
            .iter()
            .filter(|member| member.included)
            .map(|member| {
                ensure!(
                    member.choice.is_none(),
                    "Choose new panes for all included hosts to open a new window."
                );
                let options = member
                    .options
                    .clone()
                    .context("The reviewed SSH profile is unavailable")?;
                Ok((member.profile.clone(), options))
            })
            .collect()
    }
}

#[derive(Default)]
pub(super) struct State {
    revision: u64,
    review: Option<Box<Review>>,
    return_to: Option<TabId>,
    filter: filter::Filter,
    page: usize,
    set_editor: Option<SetEditor>,
    login_limit: SshGroupParallelLimit,
}

enum SetEditor {
    New(String),
    Update(Option<String>),
    Manage(ManageSet),
}

#[derive(Default)]
struct ManageSet {
    selected: Option<String>,
    name: String,
    confirm_delete: bool,
}

impl ManageSet {
    fn rename_target(&self, review: &Review) -> Option<&str> {
        let name = self.name.trim();
        (self.selected.is_some()
            && !self.confirm_delete
            && !name.is_empty()
            && !review.sets.contains_key(name))
        .then_some(name)
    }
}

impl SetEditor {
    fn target<'a>(&'a self, review: &'a Review) -> Option<(&'a str, Option<&'a [String]>)> {
        match self {
            Self::New(name)
                if !name.trim().is_empty() && !review.sets.contains_key(name.trim()) =>
            {
                Some((name.trim(), None))
            }
            Self::Update(Some(name)) => {
                let previous = review.sets.get(name)?;
                (!previous
                    .iter()
                    .map(String::as_str)
                    .eq(review.included_profiles()))
                .then_some((name.as_str(), Some(previous.as_slice())))
            }
            _ => None,
        }
    }
}

pub enum Action {
    Open { name: String, profiles: Vec<String> },
    Form(u64, Change),
}

pub enum Change {
    Choose(usize, Option<Connection>),
    Include(usize, bool),
    IncludeAll(bool),
    Filter(String),
    Page(usize),
    IncludeMatches(Vec<usize>, MatchSelection),
    ChooseMatches(Vec<usize>, ConnectionSelection),
    RecipientSet(String),
    LoginLimit(SshGroupParallelLimit),
    SaveLoginLimit(SshGroupParallelLimit),
    Approve(
        Vec<Launch>,
        Option<String>,
        LaunchLocation,
        SshGroupParallelLimit,
    ),
    Export(Vec<Launch>, Option<String>),
    BeginSaveSet,
    BeginUpdateSet,
    BeginManageSets,
    SetName(String),
    SetTarget(String),
    SaveSet(String),
    RenameSet {
        from: String,
        to: String,
    },
    ConfirmDeleteSet(String),
    DeleteSet(String),
    BackFromSet,
    Refresh,
    Back,
}

#[derive(Clone, Copy)]
pub enum LaunchLocation {
    Tab,
    Window,
}

#[derive(Clone, Copy)]
pub enum MatchSelection {
    Include,
    Only,
    Skip,
}

#[derive(Clone, Copy)]
pub enum ConnectionSelection {
    Fresh,
    ReuseUnique,
}

impl ConnectionSelection {
    fn choice(self, member: &Member) -> Option<Connection> {
        match self {
            Self::Fresh => None,
            Self::ReuseUnique => unique_connection(&member.candidates).or(member.choice),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    Skip,
    Fresh,
    Reuse(Connection),
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.review.is_some()
    }

    fn replace(&mut self, review: Review) {
        if self
            .review
            .as_ref()
            .is_some_and(|previous| self.login_limit == previous.roster_defaults.login_limit)
        {
            self.login_limit = review.roster_defaults.login_limit;
        }
        self.revision += 1;
        self.review = Some(Box::new(review));
    }

    fn open(&mut self, review: Review) {
        self.filter.clear();
        self.page = 0;
        self.set_editor = None;
        self.login_limit = review.roster_defaults.login_limit;
        self.replace(review);
    }

    fn matching(&self, review: &Review) -> Vec<usize> {
        review
            .members
            .iter()
            .enumerate()
            .filter_map(|(index, member)| {
                self.filter
                    .matches_profile(&member.profile, member.address.as_deref())
                    .then_some(index)
            })
            .collect()
    }

    fn include_matches(&mut self, offered: Vec<usize>, selection: MatchSelection) {
        let Some(review) = self.review.as_deref() else {
            return;
        };
        if self.filter.query().trim().is_empty()
            || self.matching(review) != offered
            || !offered
                .iter()
                .any(|&index| review.members[index].options.is_some())
        {
            return;
        }
        let matches: BTreeSet<_> = offered.into_iter().collect();
        let review = self.review.as_mut().unwrap();
        let mut changed = false;
        for (index, member) in review.members.iter_mut().enumerate() {
            let matched = matches.contains(&index);
            let include = match selection {
                MatchSelection::Include => member.included || matched,
                MatchSelection::Only => matched,
                MatchSelection::Skip => member.included && !matched,
            } && member.options.is_some();
            changed |= member.included != include;
            member.included = include;
        }
        if changed {
            review.selected_set = None;
            review.feedback.clear();
        }
    }

    fn choose_matches(&mut self, offered: Vec<usize>, selection: ConnectionSelection) {
        let Some(review) = self.review.as_deref() else {
            return;
        };
        if !self
            .matching(review)
            .into_iter()
            .filter(|&index| review.members[index].included)
            .eq(offered.iter().copied())
        {
            return;
        }
        let review = self.review.as_mut().unwrap();
        for index in offered {
            let member = &mut review.members[index];
            let choice = selection.choice(member);
            if member.choice != choice {
                member.choice = choice;
                review.feedback.clear();
            }
        }
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> Option<View> {
        let review = self.review.as_deref()?;
        let revision = self.revision;
        let action = move |change| {
            Message::HostGroup(super::Action::Reuse(id, Action::Form(revision, change)))
        };
        if let Some(editor) = &self.set_editor {
            return Some(save_set_view(review, editor, action, ctx));
        }
        let offered = review.choices();
        let reuse = offered
            .iter()
            .filter(|choice| matches!(choice, Launch::Reuse(_)))
            .count();
        let new = offered
            .iter()
            .filter(|choice| matches!(choice, Launch::Fresh))
            .count();
        let skipped = offered.len() - reuse - new;
        let selected_set = review.selected_set.clone();
        let export_offer = offered.clone();
        let export_set = selected_set.clone();
        let window_offer = offered.clone();
        let window_set = selected_set.clone();
        let login_limit = self.login_limit;
        let set_names: Vec<_> = review
            .sets
            .iter()
            .filter(|(_, profiles)| review.set_is_available(profiles))
            .map(|(name, _)| name.clone())
            .collect();
        let counts = format!("{reuse} to reuse · {new} to open");
        let counts = if skipped == 0 {
            counts
        } else {
            format!("{counts} · {skipped} skipped")
        };
        let matches = Rc::new(self.matching(review));
        let pages = matches.len().div_ceil(PROFILE_PAGE_SIZE).max(1);
        let page = self.page.min(pages - 1);
        let filtered = !self.filter.query().trim().is_empty();
        let scope_button = |label, selection, enabled| {
            let matches = Rc::clone(&matches);
            Button::new()
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| {
                    action(Change::IncludeMatches((*matches).clone(), selection))
                }))
                .content(label)
        };
        let can_include = matches.iter().any(|&i| review.members[i].options.is_some());
        let can_skip = matches.iter().any(|&i| review.members[i].included);
        let connection_scope: Rc<Vec<_>> = Rc::new(
            matches
                .iter()
                .copied()
                .filter(|&index| review.members[index].included)
                .collect(),
        );
        let connection_button =
            |label, selection: ConnectionSelection| {
                let scope = Rc::clone(&connection_scope);
                let enabled = scope.iter().any(|&index| {
                    let member = &review.members[index];
                    selection.choice(member) != member.choice
                });
                Button::new()
                    .is_enabled(enabled)
                    .on_click(ctx.callback(move |_| {
                        action(Change::ChooseMatches((*scope).clone(), selection))
                    }))
                    .content(label)
            };
        let rows =
            matches
                .iter()
                .copied()
                .skip(page * PROFILE_PAGE_SIZE)
                .take(PROFILE_PAGE_SIZE)
                .map(|index| {
                    let member = &review.members[index];
                    KeyedView::new(
                        member.profile.as_str(),
                        StackPanel::new().spacing(4.0).children((
                            CheckBox::new()
                                .automation_name(format!("Include profile {}", member.profile))
                                .is_checked(member.included)
                                .is_enabled(member.options.is_some())
                                .on_is_checked_changed(ctx.callback(move |included| {
                                    action(Change::Include(index, included))
                                }))
                                .content(member.profile.as_str()),
                            TextBlock::new()
                                .text(
                                    member
                                        .address
                                        .as_deref()
                                        .unwrap_or("SSH profile unavailable"),
                                )
                                .text_wrapping(TextWrapping::Wrap),
                            connection_picker(
                                &member.profile,
                                &member.candidates,
                                member.choice,
                                member.included,
                                move |choice| action(Change::Choose(index, choice)),
                                ctx,
                            ),
                        )),
                    )
                });
        Some(
            Grid::new()
                .margin(16.0)
                .row_spacing(8.0)
                .rows([GridLength::Auto, GridLength::STAR])
                .children((
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new()
                            .font_size(24.0)
                            .text(format!("Review SSH group · {}", review.name)),
                        TextBlock::new().text(counts),
                        StackPanel::new()
                            .spacing(8.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                parallel_limit::picker("Parallel SSH logins", login_limit, true, move |limit| action(Change::LoginLimit(limit)), ctx),
                                Button::new()
                                    .automation_name("Save login limit")
                                    .is_enabled(login_limit != review.roster_defaults.login_limit)
                                    .on_click(ctx.callback(move |_| action(Change::SaveLoginLimit(login_limit))))
                                    .content("Save as default"),
                            )),
                        TextBox::new()
                            .automation_name("Find launch profiles")
                            .placeholder_text("Filter by profile name or SSH address")
                            .text(self.filter.query())
                            .on_text_changed(
                                ctx.callback(move |query| action(Change::Filter(query))),
                            ),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(6.0)
                            .children((
                                Button::new()
                                    .is_enabled(page > 0)
                                    .on_click(ctx.callback(move |_| {
                                        action(Change::Page(page.saturating_sub(1)))
                                    }))
                                    .content("Previous profiles"),
                                Button::new()
                                    .is_enabled(page + 1 < pages)
                                    .on_click(ctx.callback(move |_| action(Change::Page(page + 1))))
                                    .content("Next profiles"),
                                TextBlock::new()
                                    .vertical_alignment(VerticalAlignment::Center)
                                    .text(format!(
                                        "{} matches · Page {} of {pages}",
                                        matches.len(),
                                        page + 1
                                    )),
                            )),
                        StackPanel::new()
                            .spacing(8.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                if set_names.is_empty() {
                                    View::empty()
                                } else {
                                    let selected = set_names.iter().position(|name| {
                                        Some(name) == review.selected_set.as_ref()
                                    });
                                    ComboBox::new()
                                        .automation_name("Launch recipient set")
                                        .placeholder_text("Choose a recipient set")
                                        .max_width(240.0)
                                        .items_source(set_names.clone())
                                        .selected_index(selected)
                                        .on_selection_changed(ctx.callback(
                                            move |index: Option<usize>| {
                                                index.and_then(|index| set_names.get(index)).map_or(
                                                    Message::ControlIdle,
                                                    |name| {
                                                        action(Change::RecipientSet(name.clone()))
                                                    },
                                                )
                                            },
                                        ))
                                        .into()
                                },
                                Button::new()
                                    .is_enabled(reuse + new > 0)
                                    .on_click(ctx.callback(move |_| action(Change::BeginSaveSet)))
                                    .content("Save as recipient set…"),
                                if review.sets.is_empty() {
                                    View::empty()
                                } else {
                                    Button::new()
                                        .is_enabled(reuse + new > 0)
                                        .on_click(
                                            ctx.callback(move |_| action(Change::BeginUpdateSet)),
                                        )
                                        .content("Update recipient set…")
                                },
                                if review.sets.is_empty() {
                                    View::empty()
                                } else {
                                    Button::new()
                                        .on_click(
                                            ctx.callback(move |_| action(Change::BeginManageSets)),
                                        )
                                        .content("Manage sets…")
                                },
                            )),
                        StackPanel::new()
                            .spacing(8.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                Button::new()
                                    .style(ButtonStyle::Accent)
                                    .is_enabled(reuse + new > 0)
                                    .on_click(ctx.callback(move |_| {
                                        action(Change::Approve(
                                            offered.clone(),
                                            selected_set.clone(),
                                            LaunchLocation::Tab,
                                            login_limit,
                                        ))
                                    }))
                                    .content("Open reviewed group"),
                                Button::new()
                                    .is_enabled(reuse + new > 0)
                                    .on_click(ctx.callback(move |_| {
                                        action(Change::Export(
                                            export_offer.clone(),
                                            export_set.clone(),
                                        ))
                                    }))
                                    .content("Export included hosts…"),
                                Button::new()
                                    .on_click(ctx.callback(move |_| action(Change::Refresh)))
                                    .content("Refresh review"),
                                Button::new()
                                    .on_click(ctx.callback(move |_| action(Change::Back)))
                                    .content("Cancel"),
                            )),
                        StackPanel::new()
                            .spacing(8.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                Button::new()
                                    .is_enabled(reuse == 0 && new > 0)
                                    .on_click(ctx.callback(move |_| {
                                        action(Change::Approve(
                                            window_offer.clone(),
                                            window_set.clone(),
                                            LaunchLocation::Window,
                                            login_limit,
                                        ))
                                    }))
                                    .content("Open reviewed group in new window"),
                                if reuse == 0 {
                                    View::empty()
                                } else {
                                    TextBlock::new()
                                        .vertical_alignment(VerticalAlignment::Center)
                                        .text("Choose new panes for all included hosts to open a new window.")
                                        .text_wrapping(TextWrapping::Wrap)
                                        .into()
                                },
                            )),
                        if filtered {
                            StackPanel::new()
                                .spacing(8.0)
                                .orientation(Orientation::Horizontal)
                                .children((
                                    scope_button(
                                        "Include matches",
                                        MatchSelection::Include,
                                        can_include,
                                    ),
                                    scope_button(
                                        "Include only matches",
                                        MatchSelection::Only,
                                        can_include,
                                    ),
                                    scope_button("Skip matches", MatchSelection::Skip, can_skip),
                                ))
                        } else {
                            StackPanel::new()
                                .spacing(8.0)
                                .orientation(Orientation::Horizontal)
                                .children((
                                    Button::new()
                                        .on_click(
                                            ctx.callback(move |_| action(Change::IncludeAll(true))),
                                        )
                                        .content("Include all available"),
                                    Button::new()
                                        .on_click(
                                            ctx.callback(move |_| {
                                                action(Change::IncludeAll(false))
                                            }),
                                        )
                                        .content("Clear included hosts"),
                                ))
                        },
                        StackPanel::new()
                            .spacing(8.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                connection_button(
                                    "Use new panes for matches",
                                    ConnectionSelection::Fresh,
                                ),
                                connection_button(
                                    "Reuse unique matches",
                                    ConnectionSelection::ReuseUnique,
                                ),
                            )),
                        if review.feedback.is_empty() {
                            View::empty()
                        } else {
                            TextBlock::new()
                                .text(&review.feedback)
                                .text_wrapping(TextWrapping::Wrap)
                                .into()
                        },
                    )),
                    ScrollViewer::new()
                        .grid_row(1)
                        .content(if matches.is_empty() {
                            TextBlock::new()
                                .text("No SSH profiles match this search.")
                                .into()
                        } else {
                            StackPanel::new().spacing(8.0).keyed_children(rows)
                        }),
                )),
        )
    }
}

fn existing_set_picker(
    review: &Review,
    selected: Option<&str>,
    action: impl Fn(Change) -> Message + 'static,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let names: Vec<_> = review.sets.keys().cloned().collect();
    let index = names
        .iter()
        .position(|name| Some(name.as_str()) == selected);
    ComboBox::new()
        .automation_name("Existing recipient set")
        .placeholder_text("Choose a recipient set")
        .items_source(names.clone())
        .selected_index(index)
        .on_selection_changed(ctx.callback(move |index: Option<usize>| {
            index
                .and_then(|index| names.get(index))
                .map_or(Message::ControlIdle, |name| {
                    action(Change::SetTarget(name.clone()))
                })
        }))
        .into()
}

fn save_set_view(
    review: &Review,
    editor: &SetEditor,
    action: impl Fn(Change) -> Message + Copy + 'static,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let (label, field, previous, hint): (_, View, _, _) = match editor {
        SetEditor::New(name) => (
            "Save recipient set",
            TextBox::new()
                .automation_name("Launch recipient set name")
                .placeholder_text("Recipient set name")
                .text(name)
                .on_text_changed(ctx.callback(move |name| action(Change::SetName(name))))
                .into(),
            None,
            if review.sets.contains_key(name.trim()) {
                "A recipient set with this name already exists."
            } else {
                ""
            },
        ),
        SetEditor::Update(selected) => {
            let previous = selected.as_ref().and_then(|name| review.sets.get(name));
            (
                "Update recipient set",
                existing_set_picker(review, selected.as_deref(), action, ctx),
                previous,
                if previous.is_some() && editor.target(review).is_none() {
                    "The included profiles already match this set."
                } else {
                    ""
                },
            )
        }
        SetEditor::Manage(editor) => return manage_set_view(review, editor, action, ctx),
    };
    let feedback = if review.feedback.is_empty() {
        hint
    } else {
        &review.feedback
    };
    let target = editor.target(review).map(|(name, _)| name.to_owned());
    let valid = target.is_some();
    let profiles: Vec<_> = review.included_profiles().collect();
    let changes = previous.map_or_else(View::empty, |previous| {
        let included: BTreeSet<_> = profiles.iter().copied().collect();
        let saved: BTreeSet<_> = previous.iter().map(String::as_str).collect();
        let added: Vec<_> = profiles
            .iter()
            .copied()
            .filter(|name| !saved.contains(name))
            .collect();
        let removed: Vec<_> = previous
            .iter()
            .map(String::as_str)
            .filter(|name| !included.contains(name))
            .collect();
        let rows = [("Add", added), ("Remove", removed)]
            .into_iter()
            .filter(|(_, names)| !names.is_empty())
            .map(|(label, names)| {
                KeyedView::new(
                    label,
                    TextBlock::new()
                        .text(format!("{label} {}", names.join(", ")))
                        .text_wrapping(TextWrapping::Wrap),
                )
            });
        if included == saved && valid {
            TextBlock::new()
                .text("Update the saved profile order.")
                .into()
        } else {
            StackPanel::new().spacing(6.0).keyed_children(rows)
        }
    });
    Grid::new()
        .margin(16.0)
        .row_spacing(8.0)
        .rows([GridLength::Auto, GridLength::STAR])
        .children((
            StackPanel::new().spacing(8.0).children((
                TextBlock::new()
                    .font_size(24.0)
                    .text(format!("{label} · {}", review.name)),
                field,
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .style(ButtonStyle::Accent)
                            .is_enabled(valid)
                            .on_click(ctx.callback(move |_| {
                                target.as_ref().map_or(Message::ControlIdle, |name| {
                                    action(Change::SaveSet(name.clone()))
                                })
                            }))
                            .content(label),
                        Button::new()
                            .on_click(ctx.callback(move |_| action(Change::BackFromSet)))
                            .content("Back to launch review"),
                    )),
                TextBlock::new()
                    .text(feedback)
                    .text_wrapping(TextWrapping::Wrap),
            )),
            ScrollViewer::new().grid_row(1).content(
                StackPanel::new().spacing(8.0).children((
                    changes,
                    TextBlock::new()
                        .automation_name("Recipient set profiles")
                        .text(profiles.join("\n"))
                        .is_text_selection_enabled(true)
                        .text_wrapping(TextWrapping::Wrap),
                )),
            ),
        ))
}

fn manage_set_view(
    review: &Review,
    editor: &ManageSet,
    action: impl Fn(Change) -> Message + Copy + 'static,
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let selected = editor.selected.as_deref();
    let rename = selected.zip(editor.rename_target(review));
    let rename = rename.map(|(from, to)| (from.to_owned(), to.to_owned()));
    let can_rename = rename.is_some();
    let delete = editor.selected.clone();
    let confirm_delete = editor.confirm_delete;
    let feedback = if !review.feedback.is_empty() {
        review.feedback.clone()
    } else if let Some(name) = selected.filter(|_| confirm_delete) {
        format!("Delete recipient set {name}? Your current launch choices will stay selected.")
    } else if review.sets.contains_key(editor.name.trim()) {
        "A recipient set with this name already exists.".to_owned()
    } else {
        String::new()
    };
    let profiles = selected
        .and_then(|name| review.sets.get(name))
        .map(|names| names.join("\n"))
        .unwrap_or_default();
    Grid::new()
        .margin(16.0)
        .row_spacing(8.0)
        .rows([GridLength::Auto, GridLength::STAR])
        .children((
            StackPanel::new().spacing(8.0).children((
                TextBlock::new()
                    .font_size(24.0)
                    .text(format!("Manage recipient sets · {}", review.name)),
                existing_set_picker(review, selected, action, ctx),
                TextBox::new()
                    .automation_name("New recipient set name")
                    .placeholder_text("New recipient set name")
                    .is_enabled(selected.is_some())
                    .text(&editor.name)
                    .on_text_changed(ctx.callback(move |name| action(Change::SetName(name)))),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .is_enabled(can_rename)
                            .on_click(ctx.callback(move |_| {
                                rename.as_ref().map_or(Message::ControlIdle, |(from, to)| {
                                    action(Change::RenameSet {
                                        from: from.clone(),
                                        to: to.clone(),
                                    })
                                })
                            }))
                            .content("Rename recipient set"),
                        Button::new()
                            .is_enabled(selected.is_some())
                            .on_click(ctx.callback(move |_| {
                                delete.as_ref().map_or(Message::ControlIdle, |name| {
                                    action(if confirm_delete {
                                        Change::DeleteSet(name.clone())
                                    } else {
                                        Change::ConfirmDeleteSet(name.clone())
                                    })
                                })
                            }))
                            .content(if confirm_delete {
                                "Confirm delete recipient set"
                            } else {
                                "Delete recipient set"
                            }),
                        Button::new()
                            .on_click(ctx.callback(move |_| action(Change::BackFromSet)))
                            .content("Back to launch review"),
                    )),
                TextBlock::new()
                    .text(feedback)
                    .text_wrapping(TextWrapping::Wrap),
            )),
            ScrollViewer::new().grid_row(1).content(
                TextBlock::new()
                    .automation_name("Recipient set profiles")
                    .text(profiles)
                    .is_text_selection_enabled(true)
                    .text_wrapping(TextWrapping::Wrap),
            ),
        ))
}

pub(super) fn button(
    id: TabId,
    name: &str,
    profiles: &[String],
    ctx: &ViewContext<TerminalApp>,
) -> View {
    let name = name.to_owned();
    let profiles = profiles.to_vec();
    Button::new()
        .is_enabled(!profiles.is_empty())
        .on_click(ctx.callback(move |_| {
            Message::HostGroup(super::Action::Reuse(
                id,
                Action::Open {
                    name: name.clone(),
                    profiles: profiles.clone(),
                },
            ))
        }))
        .content(LABEL)
}

impl TerminalApp {
    pub(super) fn review_host_group_from_palette(
        &mut self,
        offer: Offer,
        ctx: &ComponentContext<Self>,
    ) {
        if let Err(error) = self.open_launch_review(offer, ctx) {
            self.refresh_profiles();
            self.window_error = Some(format!("{error:#}"));
        }
    }

    fn open_launch_review(&mut self, offer: Offer, ctx: &ComponentContext<Self>) -> Result<()> {
        self.check_saved_group_launch()?;
        let previous = self.tabs.active_id().context("There is no active tab")?;
        let mut review = self.saved_group_reuse_review(offer.group, offer.profiles, None)?;
        if let Some((name, members)) = offer.recipient_set {
            ensure!(
                review.sets.get(&name) == Some(&members),
                "Recipient set {name} changed. Open the command palette again."
            );
            review.select_set(name)?;
        }
        let id = self.new_host_group_tab(Some(&review.name));
        if let Some(page) = self.host_group_mut(id) {
            page.saved.reuse.return_to = Some(previous);
            page.saved.reuse.open(review);
        }
        self.tabs.select(id);
        self.sync_selection(ctx);
        self.activate(ctx);
        Ok(())
    }

    pub(super) fn handle_host_group_reuse(
        &mut self,
        id: TabId,
        action: Action,
        ctx: &ComponentContext<Self>,
    ) {
        let (revision, change) = match action {
            Action::Open { name, profiles } => {
                if self.host_group_command_target() == Some(id) {
                    match self.saved_group_reuse_review(name, profiles, None) {
                        Ok(review) => {
                            if let Some(page) = self.host_group_mut(id) {
                                page.saved.reuse.open(review);
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
                return;
            }
            Action::Form(revision, change) => (revision, change),
        };
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page) else {
            return;
        };
        if page.host_list_export.is_open() {
            return;
        }
        let state = &page.saved.reuse;
        let Some(review) = state
            .review
            .as_deref()
            .filter(|_| state.revision == revision)
        else {
            return;
        };
        if state.set_editor.is_some() {
            self.edit_launch_set(id, change);
            return;
        }
        match change {
            Change::Back => {
                if let Some(previous) = state.return_to {
                    self.tabs.select(previous);
                    self.close_tab(id, ctx);
                } else if let Some(page) = self.host_group_mut(id) {
                    page.saved.reuse.review = None;
                }
            }
            Change::Choose(index, choice) => {
                if let Some(page) = self.host_group_mut(id)
                    && let Some(review) = page.saved.reuse.review.as_mut()
                    && let Some(member) = review.members.get_mut(index)
                    && member.included
                {
                    member.select(choice);
                }
            }
            Change::Include(index, included) => {
                if let Some(page) = self.host_group_mut(id)
                    && let Some(review) = page.saved.reuse.review.as_mut()
                {
                    review.include(index, included);
                }
            }
            Change::IncludeAll(included) => {
                if let Some(page) = self.host_group_mut(id)
                    && let Some(review) = page.saved.reuse.review.as_mut()
                {
                    for member in &mut review.members {
                        member.included = included && member.options.is_some();
                    }
                    review.selected_set = None;
                    review.feedback.clear();
                }
            }
            Change::Filter(query) => {
                if state.filter.query() != query
                    && let Some(page) = self.host_group_mut(id)
                {
                    page.saved.reuse.filter.set_query(query);
                    page.saved.reuse.page = 0;
                }
            }
            Change::Page(page) => {
                if page < state.matching(review).len().div_ceil(PROFILE_PAGE_SIZE)
                    && let Some(host_group) = self.host_group_mut(id)
                {
                    host_group.saved.reuse.page = page;
                }
            }
            Change::IncludeMatches(offered, selection) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.saved.reuse.include_matches(offered, selection);
                }
            }
            Change::ChooseMatches(offered, selection) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.saved.reuse.choose_matches(offered, selection);
                }
            }
            Change::RecipientSet(name) => {
                if let Some(page) = self.host_group_mut(id)
                    && let Some(review) = page.saved.reuse.review.as_mut()
                    && let Err(error) = review.select_set(name)
                {
                    review.feedback = format!("{error:#}");
                }
            }
            Change::LoginLimit(limit) => {
                if let Some(page) = self.host_group_mut(id) {
                    page.saved.reuse.login_limit = limit;
                }
            }
            Change::SaveLoginLimit(limit)
                if state.login_limit == limit && review.roster_defaults.login_limit != limit =>
            {
                let name = review.name.clone();
                let members = review.profiles();
                let expected = review.roster_defaults;
                let result =
                    self.save_saved_group_login_limit(id, &name, &members, expected, limit);
                if let Some(page) = self.host_group_mut(id)
                    && let Some(review) = page.saved.reuse.review.as_mut()
                {
                    review.feedback = match result {
                        Ok(()) => {
                            review.roster_defaults.login_limit = limit;
                            "Saved the parallel SSH login limit".into()
                        }
                        Err(error) => {
                            format!("{error:#} Refresh the launch review before saving again.")
                        }
                    };
                }
            }
            Change::Refresh => {
                let updated = self.saved_group_reuse_review(
                    review.name.clone(),
                    review.profiles(),
                    Some(review),
                );
                if let Some(page) = self.host_group_mut(id) {
                    match updated {
                        Ok(review) => page.saved.reuse.replace(review),
                        Err(error) => {
                            if let Some(review) = page.saved.reuse.review.as_mut() {
                                review.feedback = format!("{error:#}");
                            }
                        }
                    }
                }
            }
            Change::Approve(offered, set, location, limit)
                if review.choices() == offered
                    && review.selected_set == set
                    && state.login_limit == limit =>
            {
                let result = match location {
                    LaunchLocation::Tab => {
                        self.open_group_reusing_connections(id, limit.concurrency(), ctx)
                    }
                    LaunchLocation::Window => {
                        self.open_reviewed_group_window(id, limit.concurrency(), ctx)
                    }
                };
                if let Err(error) = result
                    && let Some(page) = self.host_group_mut(id)
                    && let Some(review) = page.saved.reuse.review.as_mut()
                {
                    review.feedback = format!("{error:#}");
                }
            }
            Change::Export(offered, set)
                if review.choices() == offered && review.selected_set == set =>
            {
                let snapshot = self.capture_review_host_list(review);
                if let Some(page) = self.host_group_mut(id) {
                    match snapshot {
                        Ok(snapshot) => page.host_list_export.open(snapshot, true),
                        Err(error) => {
                            if let Some(review) = page.saved.reuse.review.as_mut() {
                                review.feedback = format!("{error:#}");
                            }
                        }
                    }
                }
            }
            change @ (Change::BeginSaveSet | Change::BeginUpdateSet | Change::BeginManageSets) => {
                let has_included = review.included_profiles().next().is_some();
                let editor = match change {
                    Change::BeginSaveSet if has_included => SetEditor::New(String::new()),
                    Change::BeginUpdateSet if has_included && !review.sets.is_empty() => {
                        SetEditor::Update(review.selected_set.clone())
                    }
                    Change::BeginManageSets if !review.sets.is_empty() => {
                        SetEditor::Manage(ManageSet {
                            selected: review.selected_set.clone(),
                            ..Default::default()
                        })
                    }
                    _ => return,
                };
                if let Some(page) = self.host_group_mut(id) {
                    let state = &mut page.saved.reuse;
                    if let Some(review) = &mut state.review {
                        review.feedback.clear();
                        state.set_editor = Some(editor);
                        state.revision += 1;
                    }
                }
            }
            _ => {}
        }
    }

    fn edit_launch_set(&mut self, id: TabId, change: Change) {
        let Some(Page::HostGroup(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
            return;
        };
        let state = &mut page.saved.reuse;
        let (Some(review), Some(editor)) = (&mut state.review, &mut state.set_editor) else {
            return;
        };
        match change {
            Change::SetName(text) => match editor {
                SetEditor::New(name) => {
                    *name = text;
                    review.feedback.clear();
                }
                SetEditor::Manage(edit) if edit.name != text => {
                    edit.name = text;
                    edit.confirm_delete = false;
                    review.feedback.clear();
                }
                _ => {}
            },
            Change::SetTarget(name) if review.sets.contains_key(&name) => match editor {
                SetEditor::Update(selected) => {
                    *selected = Some(name);
                    review.feedback.clear();
                }
                SetEditor::Manage(edit) if edit.selected.as_ref() != Some(&name) => {
                    *edit = ManageSet {
                        selected: Some(name),
                        ..Default::default()
                    };
                    review.feedback.clear();
                }
                _ => {}
            },
            Change::ConfirmDeleteSet(name)
                if let SetEditor::Manage(edit) = editor
                    && edit.selected.as_ref() == Some(&name) =>
            {
                edit.confirm_delete = true;
                review.feedback.clear();
            }
            change @ (Change::RenameSet { .. } | Change::DeleteSet(_))
                if let SetEditor::Manage(edit) = editor =>
            {
                let (name, rename) = match change {
                    Change::RenameSet { from, to } if edit.rename_target(review) == Some(&to) => {
                        (from, Some(to))
                    }
                    Change::DeleteSet(name) if edit.confirm_delete => (name, None),
                    _ => return,
                };
                let Some(expected) = review.sets.get(&name) else {
                    return;
                };
                if edit.selected.as_ref() != Some(&name) {
                    return;
                }
                let group = &review.name;
                let group_members = review.profiles();
                let saved = if let Some(new_name) = &rename {
                    self.store.rename_ssh_recipient_set(
                        group,
                        &group_members,
                        &name,
                        expected,
                        new_name,
                    )
                } else {
                    self.store.change_ssh_recipient_set(
                        group,
                        &group_members,
                        &name,
                        Some(expected),
                        None,
                    )
                };
                match saved {
                    Ok(catalog) => {
                        if review.selected_set.as_ref() == Some(&name) {
                            review.selected_set = rename.clone();
                        }
                        // Keep the reviewed membership of every other set for stale checks.
                        let members = review.sets.remove(&name);
                        review.feedback = if let Some((new_name, members)) = rename.zip(members) {
                            let feedback = format!("Renamed recipient set {name} to {new_name}");
                            review.sets.insert(new_name, members);
                            feedback
                        } else {
                            format!("Deleted recipient set {name}")
                        };
                        state.set_editor = None;
                        state.revision += 1;
                        self.use_group_catalog(catalog);
                    }
                    Err(error) => review.feedback = format!("{error:#}"),
                }
            }
            Change::BackFromSet => {
                state.set_editor = None;
                state.revision += 1;
                review.feedback.clear();
            }
            Change::SaveSet(name) => {
                let Some((_, expected)) =
                    editor.target(review).filter(|(target, _)| *target == name)
                else {
                    return;
                };
                let updating = expected.is_some();
                let members: Vec<_> = review.included_profiles().map(str::to_owned).collect();
                match self.store.save_reviewed_ssh_recipient_set(
                    &review.name,
                    &review.profiles(),
                    &name,
                    expected,
                    &members,
                    |catalog| review.validate_profiles(catalog),
                ) {
                    Ok(catalog) => {
                        review.sets = catalog
                            .ssh_recipient_sets
                            .get(&review.name)
                            .cloned()
                            .unwrap_or_default();
                        let verb = if updating { "Updated" } else { "Saved" };
                        review.feedback = format!("{verb} recipient set {name}");
                        review.selected_set = Some(name);
                        state.set_editor = None;
                        state.revision += 1;
                        self.use_group_catalog(catalog);
                    }
                    Err(error) => review.feedback = format!("{error:#}"),
                }
            }
            _ => {}
        }
    }

    fn capture_review_host_list(&self, review: &Review) -> Result<host_list_export::Snapshot> {
        let catalog = self.load_expected_host_group(&review.name, &review.profiles())?;
        review.validate_profiles(&catalog)?;
        host_list_export::Snapshot::profiles(
            review
                .members
                .iter()
                .filter(|member| member.included)
                .filter_map(|member| Some((member.profile.as_str(), member.options.as_ref()?))),
        )
    }

    fn saved_group_reuse_review(
        &self,
        name: String,
        profiles: Vec<String>,
        previous: Option<&Review>,
    ) -> Result<Review> {
        let catalog = self.load_expected_host_group(&name, &profiles)?;
        let targets = self.host_group_targets();
        let roster_defaults = catalog.ssh_roster_defaults(&name);
        let sets = catalog
            .ssh_recipient_sets
            .get(&name)
            .cloned()
            .unwrap_or_default();
        let members = profiles
            .into_iter()
            .enumerate()
            .map(|(index, profile)| {
                let options = catalog.ssh_host_group_member(&profile).cloned();
                let candidates = options.as_ref().map_or_else(Vec::new, |expected| {
                    self.group_reuse_candidates(&targets, Some(&profile), expected)
                });
                let mut member = Member::new(profile, options, candidates);
                if let Some(previous) = previous {
                    member.included &= previous.members[index].included;
                    member.choice = previous.members[index]
                        .choice
                        .filter(|&connection| member.contains(connection));
                }
                member
            })
            .collect();
        let mut review = Review {
            name,
            members,
            sets,
            roster_defaults,
            selected_set: None,
            feedback: String::new(),
        };
        if let Some(name) = previous.and_then(|review| review.selected_set.clone())
            && let Err(error) = review.select_set(name)
        {
            review.feedback =
                format!("{error:#} Review the checked hosts before opening the group.");
        }
        Ok(review)
    }

    pub(super) fn group_reuse_candidates(
        &self,
        targets: &[Target],
        profile: Option<&str>,
        options: &SshOptions,
    ) -> Vec<Candidate> {
        targets
            .iter()
            .filter_map(|target| {
                if target.profile.as_deref() != profile {
                    return None;
                }
                Some(Candidate {
                    connection: self.group_reuse_connection(target.pane, profile, options)?,
                    title: target.name.clone(),
                })
            })
            .collect()
    }

    pub(super) fn group_reuse_connection(
        &self,
        pane: u64,
        profile: Option<&str>,
        expected: &SshOptions,
    ) -> Option<Connection> {
        let Page::Terminal(live) = &self.tabs.get(self.tab_for_pane(pane)?)?.page else {
            return None;
        };
        if live.profile_name.as_deref() != profile
            || !matches!(&live.source, Source::Ssh(options) if options == expected)
            || !live.session.is_direct_ssh_shell()
            || live.read_only.get()
            || live.device_busy
            || live.session.prompts.current().is_some()
        {
            return None;
        }
        let generation = live.session.frames.load().connection_generation;
        live.session.ssh_connection_at(generation)?;
        Some(Connection { pane, generation })
    }

    fn open_group_reusing_connections(
        &mut self,
        source_tab: TabId,
        login_limit: Option<usize>,
        ctx: &ComponentContext<Self>,
    ) -> Result<()> {
        let Some(Page::HostGroup(page)) = self.tabs.get(source_tab).map(|tab| &tab.page) else {
            return Ok(());
        };
        let Some(review) = page.saved.reuse.review.as_deref() else {
            return Ok(());
        };
        let standalone = page.saved.reuse.return_to.is_some();
        let name = review.name.clone();
        let profiles = review.profiles();
        let catalog = self.load_expected_host_group(&name, &profiles)?;
        review.validate(&catalog, |approved, profile, expected| {
            self.group_reuse_connection(approved.pane, Some(profile), expected) == Some(approved)
        })?;
        let mut reused = BTreeSet::new();
        let mut fresh = Vec::new();
        let mut skipped = 0;
        for member in &review.members {
            match member.launch() {
                Launch::Reuse(connection) => {
                    reused.insert(connection.pane);
                }
                Launch::Fresh => {
                    let options = member
                        .options
                        .clone()
                        .context("The reviewed SSH profile is unavailable")?;
                    fresh.push((Some(member.profile.clone()), options));
                }
                Launch::Skip => skipped += 1,
            }
        }
        let feedback = format!(
            "Reused {} · Opened {} for {name}",
            reused.len(),
            fresh.len()
        );
        let feedback = if skipped == 0 {
            feedback
        } else {
            format!("{feedback} · Skipped {skipped}")
        };
        self.use_group_catalog(catalog);
        if let Some(page) = self.host_group_mut(source_tab) {
            page.saved.reuse.review = None;
            page.saved.reuse.return_to = None;
        }
        let id = if standalone {
            source_tab
        } else {
            self.new_host_group_tab(Some(&name))
        };
        self.start_host_group_panes(id, reused, fresh, PaneLaunch::Select, login_limit, ctx);
        self.remember_group_launch(id, &name, &profiles, skipped == 0, feedback);
        Ok(())
    }

    fn open_reviewed_group_window(
        &mut self,
        source_tab: TabId,
        login_limit: Option<usize>,
        ctx: &ComponentContext<Self>,
    ) -> Result<()> {
        let Some(Page::HostGroup(page)) = self.tabs.get(source_tab).map(|tab| &tab.page) else {
            return Ok(());
        };
        let Some(review) = page.saved.reuse.review.as_deref() else {
            return Ok(());
        };
        let previous = page.saved.reuse.return_to;
        let name = review.name.clone();
        let profiles = review.profiles();
        let catalog = self.load_expected_host_group(&name, &profiles)?;
        let members = review.window_members(&catalog)?;
        self.open_group_child_window(
            WindowGroup {
                name,
                profiles,
                members,
                login_limit,
            },
            ctx,
        )?;
        self.use_group_catalog(catalog);
        if let Some(previous) = previous {
            self.tabs.select(previous);
            self.close_tab(source_tab, ctx);
        } else if let Some(page) = self.host_group_mut(source_tab) {
            page.saved.reuse.review = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn options(host: &str) -> SshOptions {
        serde_json::from_value(json!({"host": host, "user": "test", "auth": "none"})).unwrap()
    }

    fn candidate(pane: u64, generation: u64) -> Candidate {
        Candidate {
            connection: Connection { pane, generation },
            title: format!("Pane {pane}"),
        }
    }

    #[test]
    fn filtered_scope_ignores_a_stale_offer_and_skips_unavailable_matches() {
        let mut state = State::default();
        state.open(Review {
            name: "Fleet".into(),
            members: vec![
                Member::new("East".into(), Some(options("east")), Vec::new()),
                Member::new("West".into(), Some(options("west")), Vec::new()),
                Member::new("East retired".into(), None, Vec::new()),
            ],
            sets: SshRecipientSets::new(),
            roster_defaults: SshRosterDefaults::default(),
            selected_set: Some("Fleet".into()),
            feedback: String::new(),
        });
        state.filter.set_query("EAST".into());
        state.include_matches(vec![1], MatchSelection::Only);
        let review = state.review.as_deref().unwrap();
        assert!(matches!(
            review.choices().as_slice(),
            [Launch::Fresh, Launch::Fresh, Launch::Skip]
        ));
        assert_eq!(review.selected_set.as_deref(), Some("Fleet"));
        state.include_matches(vec![0, 2], MatchSelection::Only);
        let review = state.review.as_deref().unwrap();
        assert!(matches!(
            review.choices().as_slice(),
            [Launch::Fresh, Launch::Skip, Launch::Skip]
        ));
        assert!(review.selected_set.is_none());
    }

    #[test]
    fn a_unique_match_is_suggested_and_multiple_matches_require_a_choice() {
        let existing = candidate(2, 1);
        let id = existing.connection;
        let stale = Connection {
            pane: id.pane,
            generation: id.generation + 1,
        };
        let single = Member::new("East".into(), Some(options("east")), vec![existing]);
        assert!(single.choice == Some(id));
        let mut multiple = Member::new(
            "East".into(),
            Some(options("east")),
            vec![candidate(2, 1), candidate(3, 1)],
        );
        assert!(multiple.choice.is_none());
        multiple.select(Some(stale));
        assert!(multiple.choice.is_none());
        multiple.select(Some(id));
        assert!(multiple.choice == Some(id));
        multiple.select(None);
        assert!(multiple.choice.is_none());
    }

    #[test]
    fn bulk_connection_choices_respect_the_included_filter_and_keep_ambiguous_reuse() {
        let mut ambiguous = Member::new(
            "East backup".into(),
            Some(options("east-backup")),
            vec![candidate(4, 1), candidate(5, 1)],
        );
        let manual = ambiguous.candidates[1].connection;
        ambiguous.select(Some(manual));
        let mut skipped = Member::new(
            "East skipped".into(),
            Some(options("east-skipped")),
            vec![candidate(6, 1)],
        );
        skipped.included = false;
        let mut state = State::default();
        state.open(Review {
            name: "Fleet".into(),
            members: vec![
                Member::new("East".into(), Some(options("east")), vec![candidate(2, 1)]),
                Member::new("West".into(), Some(options("west")), vec![candidate(3, 1)]),
                ambiguous,
                skipped,
            ],
            sets: SshRecipientSets::new(),
            roster_defaults: SshRosterDefaults::default(),
            selected_set: Some("Fleet".into()),
            feedback: String::new(),
        });
        state.filter.set_query("east".into());
        state.choose_matches(vec![0, 1, 2], ConnectionSelection::Fresh);
        assert!(state.review.as_ref().unwrap().members[0].choice.is_some());
        state.choose_matches(vec![0, 2], ConnectionSelection::Fresh);
        let review = state.review.as_mut().unwrap();
        assert!(review.members[0].choice.is_none());
        assert!(review.members[1].choice.is_some());
        assert!(review.members[2].choice.is_none());
        assert!(review.members[3].choice.is_some() && !review.members[3].included);
        review.members[2].select(Some(manual));
        state.choose_matches(vec![0, 2], ConnectionSelection::ReuseUnique);
        let review = state.review.as_ref().unwrap();
        assert!(review.members[0].choice.is_some());
        assert!(review.members[2].choice == Some(manual));
        assert_eq!(review.selected_set.as_deref(), Some("Fleet"));
    }

    #[test]
    fn confirmation_rejects_profile_edits_and_replaced_ssh_generations() {
        let review = Review {
            name: "Fleet".into(),
            members: vec![
                Member::new("East".into(), Some(options("east")), vec![candidate(2, 1)]),
                Member::new("West".into(), Some(options("west")), Vec::new()),
            ],
            sets: SshRecipientSets::new(),
            roster_defaults: SshRosterDefaults::default(),
            selected_set: None,
            feedback: String::new(),
        };
        let mut current = Profiles::default();
        current.profiles.insert("East".into(), options("east"));
        current.profiles.insert("West".into(), options("west"));
        assert!(
            review
                .validate(&current, |approved, profile, options| {
                    approved
                        == Connection {
                            pane: 2,
                            generation: 1,
                        }
                        && profile == "East"
                        && options.host == "east"
                })
                .is_ok()
        );
        let error = review
            .validate(&current, |approved, _, _| approved.generation == 2)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("reviewed SSH connection for East changed")
        );
        current.profiles.insert("West".into(), options("new-west"));
        let error = review.validate(&current, |_, _, _| true).unwrap_err();
        assert!(error.to_string().starts_with("SSH profiles changed"));
    }

    #[test]
    fn a_launch_set_only_validates_the_reviewed_members_and_its_saved_definition() {
        let sets = SshRecipientSets::from([
            ("Canaries".into(), vec!["East".into()]),
            ("Unavailable".into(), vec!["Offline".into()]),
        ]);
        let mut review = Review {
            name: "Fleet".into(),
            members: vec![
                Member::new("East".into(), Some(options("east")), Vec::new()),
                Member::new("West".into(), Some(options("west")), vec![candidate(2, 1)]),
                Member::new("Offline".into(), None, Vec::new()),
            ],
            sets: sets.clone(),
            roster_defaults: SshRosterDefaults::default(),
            selected_set: None,
            feedback: String::new(),
        };
        review.select_set("Canaries".into()).unwrap();
        assert!(matches!(
            review.choices().as_slice(),
            [Launch::Fresh, Launch::Skip, Launch::Skip]
        ));
        let error = review.select_set("Unavailable".into()).unwrap_err();
        assert!(error.to_string().contains("unavailable SSH profile"));

        let mut catalog = Profiles::default();
        catalog.profiles.insert("East".into(), options("east"));
        catalog.ssh_recipient_sets.insert("Fleet".into(), sets);
        assert!(
            review
                .validate(&catalog, |_, _, _| panic!(
                    "skipped connection was validated"
                ))
                .is_ok()
        );

        catalog
            .ssh_recipient_sets
            .get_mut("Fleet")
            .unwrap()
            .insert("Canaries".into(), vec!["West".into()]);
        let error = review.validate(&catalog, |_, _, _| true).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("Recipient set Canaries changed")
        );
        review.include(0, false);
        review.include(2, true);
        let error = review.validate(&catalog, |_, _, _| true).unwrap_err();
        assert_eq!(error.to_string(), "Include at least one SSH host.");
    }
}
