use super::*;
use anyhow::{Result, ensure};

pub(super) const LABEL: &str = "Review connection reuse…";
const FRESH: &str = "Open a new SSH pane";

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    pane: u64,
    generation: u64,
}

struct Candidate {
    connection: Connection,
    title: String,
}

struct Member {
    profile: String,
    options: SshOptions,
    candidates: Vec<Candidate>,
    choice: Option<Connection>,
}

impl Member {
    fn new(profile: String, options: SshOptions, candidates: Vec<Candidate>) -> Self {
        let choice = (candidates.len() == 1).then(|| candidates[0].connection);
        Self {
            profile,
            options,
            candidates,
            choice,
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
}

struct Review {
    name: String,
    members: Vec<Member>,
    feedback: String,
}

impl Review {
    fn profiles(&self) -> Vec<String> {
        self.members
            .iter()
            .map(|member| member.profile.clone())
            .collect()
    }

    fn choices(&self) -> Vec<Option<Connection>> {
        self.members.iter().map(|member| member.choice).collect()
    }

    fn validate(
        &self,
        current: &[SshOptions],
        connection: impl Fn(Connection, &str, &SshOptions) -> bool,
    ) -> Result<()> {
        ensure!(
            self.members
                .iter()
                .map(|member| &member.options)
                .eq(current),
            "SSH profiles changed. Refresh the review before opening this group."
        );
        for member in &self.members {
            if let Some(approved) = member.choice {
                ensure!(
                    connection(approved, &member.profile, &member.options),
                    "The reviewed SSH connection for {} changed. Refresh the review or choose a new pane.",
                    member.profile
                );
            }
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct State {
    revision: u64,
    review: Option<Box<Review>>,
}

pub enum Action {
    Open { name: String, profiles: Vec<String> },
    Form(u64, Change),
}

pub enum Change {
    Choose(usize, Option<Connection>),
    Approve(Vec<Option<Connection>>),
    Refresh,
    Back,
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.review.is_some()
    }

    fn replace(&mut self, review: Review) {
        self.revision += 1;
        self.review = Some(Box::new(review));
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> Option<View> {
        let review = self.review.as_deref()?;
        let revision = self.revision;
        let action = move |change| {
            Message::HostGroup(super::Action::Reuse(id, Action::Form(revision, change)))
        };
        let offered = review.choices();
        let reuse = offered.iter().flatten().count();
        let new = offered.len() - reuse;
        let rows = review.members.iter().enumerate().map(|(index, member)| {
            let mut labels = vec![FRESH.to_owned()];
            labels.extend(
                member
                    .candidates
                    .iter()
                    .map(|candidate| format!("Reuse · {}", candidate.title)),
            );
            let options: Vec<_> = std::iter::once(None)
                .chain(
                    member
                        .candidates
                        .iter()
                        .map(|candidate| Some(candidate.connection)),
                )
                .collect();
            let selected = options.iter().position(|&choice| choice == member.choice);
            KeyedView::new(
                member.profile.as_str(),
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new().text(&member.profile),
                    ComboBox::new()
                        .automation_name(format!("Connection for {}", member.profile))
                        .items_source(labels)
                        .selected_index(selected)
                        .on_selection_changed(ctx.callback(move |choice: Option<usize>| {
                            choice
                                .and_then(|choice| options.get(choice).copied())
                                .map_or(Message::ControlIdle, |choice| {
                                    action(Change::Choose(index, choice))
                                })
                        })),
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
                        TextBlock::new().text(format!("{reuse} to reuse · {new} to open")),
                        StackPanel::new()
                            .spacing(8.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                Button::new()
                                    .style(ButtonStyle::Accent)
                                    .on_click(ctx.callback(move |_| {
                                        action(Change::Approve(offered.clone()))
                                    }))
                                    .content("Open reviewed group"),
                                Button::new()
                                    .on_click(ctx.callback(move |_| action(Change::Refresh)))
                                    .content("Refresh review"),
                                Button::new()
                                    .on_click(ctx.callback(move |_| action(Change::Back)))
                                    .content("Cancel"),
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
                        .content(StackPanel::new().spacing(8.0).keyed_children(rows)),
                )),
        )
    }
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
                                page.saved.reuse.replace(review);
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
        let state = &page.saved.reuse;
        let Some(review) = state
            .review
            .as_deref()
            .filter(|_| state.revision == revision)
        else {
            return;
        };
        match change {
            Change::Back => {
                if let Some(page) = self.host_group_mut(id) {
                    page.saved.reuse.review = None;
                }
            }
            Change::Choose(index, choice) => {
                if let Some(page) = self.host_group_mut(id)
                    && let Some(review) = page.saved.reuse.review.as_mut()
                    && let Some(member) = review.members.get_mut(index)
                {
                    member.select(choice);
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
            Change::Approve(offered) if review.choices() == offered => {
                if let Err(error) = self.open_group_reusing_connections(id, ctx)
                    && let Some(page) = self.host_group_mut(id)
                    && let Some(review) = page.saved.reuse.review.as_mut()
                {
                    review.feedback = format!("{error:#}");
                }
            }
            _ => {}
        }
    }

    fn saved_group_reuse_review(
        &self,
        name: String,
        profiles: Vec<String>,
        previous: Option<&Review>,
    ) -> Result<Review> {
        let (_, options) = self.resolve_saved_host_group(&name, &profiles)?;
        let targets = self.host_group_targets();
        let members = profiles
            .into_iter()
            .zip(options)
            .enumerate()
            .map(|(index, (profile, options))| {
                let candidates = targets
                    .iter()
                    .filter_map(|target| {
                        (target.profile.as_deref() == Some(profile.as_str())).then_some(())?;
                        let connection =
                            self.group_reuse_connection(target.pane, &profile, &options)?;
                        Some(Candidate {
                            connection,
                            title: target.name.clone(),
                        })
                    })
                    .collect();
                let mut member = Member::new(profile, options, candidates);
                if let Some(previous) = previous {
                    member.choice = previous.members[index]
                        .choice
                        .filter(|&connection| member.contains(connection));
                }
                member
            })
            .collect();
        Ok(Review {
            name,
            members,
            feedback: String::new(),
        })
    }

    fn group_reuse_connection(
        &self,
        pane: u64,
        profile: &str,
        expected: &SshOptions,
    ) -> Option<Connection> {
        let Page::Terminal(live) = &self.tabs.get(self.tab_for_pane(pane)?)?.page else {
            return None;
        };
        if live.profile_name.as_deref() != Some(profile)
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
        ctx: &ComponentContext<Self>,
    ) -> Result<()> {
        let Some(Page::HostGroup(page)) = self.tabs.get(source_tab).map(|tab| &tab.page) else {
            return Ok(());
        };
        let Some(review) = page.saved.reuse.review.as_deref() else {
            return Ok(());
        };
        let name = review.name.clone();
        let (catalog, current) = self.resolve_saved_host_group(&name, &review.profiles())?;
        review.validate(&current, |approved, profile, expected| {
            self.group_reuse_connection(approved.pane, profile, expected) == Some(approved)
        })?;
        let mut reused = BTreeSet::new();
        let mut fresh = Vec::new();
        for (member, options) in review.members.iter().zip(current) {
            if let Some(connection) = member.choice {
                reused.insert(connection.pane);
            } else {
                fresh.push((Some(member.profile.clone()), options));
            }
        }
        let feedback = format!(
            "Reused {} · Opened {} for {name}",
            reused.len(),
            fresh.len()
        );
        self.use_group_catalog(catalog);
        if let Some(page) = self.host_group_mut(source_tab) {
            page.saved.reuse.review = None;
        }
        let id = self.new_host_group_tab(Some(&name));
        self.start_host_group_panes(id, reused, fresh, ctx);
        self.remember_group_launch(id, &name, feedback);
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
    fn a_unique_match_is_suggested_and_multiple_matches_require_a_choice() {
        let existing = candidate(2, 1);
        let id = existing.connection;
        let stale = Connection {
            pane: id.pane,
            generation: id.generation + 1,
        };
        let single = Member::new("East".into(), options("east"), vec![existing]);
        assert!(single.choice == Some(id));
        let mut multiple = Member::new(
            "East".into(),
            options("east"),
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
    fn confirmation_rejects_profile_edits_and_replaced_ssh_generations() {
        let review = Review {
            name: "Fleet".into(),
            members: vec![
                Member::new("East".into(), options("east"), vec![candidate(2, 1)]),
                Member::new("West".into(), options("west"), Vec::new()),
            ],
            feedback: String::new(),
        };
        let current = [options("east"), options("west")];
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
        let changed = [options("east"), options("new-west")];
        let error = review.validate(&changed, |_, _, _| true).unwrap_err();
        assert!(error.to_string().starts_with("SSH profiles changed"));
        let error = review
            .validate(&current, |approved, _, _| approved.generation == 2)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("reviewed SSH connection for East changed")
        );
    }
}
