use super::*;

#[derive(Default)]
pub(super) struct State {
    origin: Option<Origin>,
    review: Option<Proposal>,
    revision: u64,
    feedback: String,
}

struct Origin {
    name: String,
    members: Vec<String>,
    full_roster: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct Proposal {
    members: Vec<String>,
    added: Vec<String>,
    removed: Vec<String>,
    without_profile: usize,
    unavailable: usize,
}

impl Proposal {
    fn from_selection(original: &[String], selection: Selection, count: usize) -> Self {
        let (mut members, removed): (Vec<_>, _) = original
            .iter()
            .cloned()
            .partition(|member| selection.members.contains(member));
        let added: Vec<_> = selection
            .members
            .into_iter()
            .filter(|member| !original.contains(member))
            .collect();
        members.extend(added.iter().cloned());
        Self {
            members,
            added,
            removed,
            without_profile: selection.without_profile,
            unavailable: count - selection.matched_panes,
        }
    }

    fn can_save(&self) -> bool {
        !self.members.is_empty() && (!self.added.is_empty() || !self.removed.is_empty())
    }
}

pub enum Action {
    Open,
    Apply(u64),
    Cancel(u64),
}

impl State {
    pub(super) fn remember(&mut self, name: &str, members: &[String], full_roster: bool) {
        self.origin = Some(Origin {
            name: name.into(),
            members: members.to_vec(),
            full_roster,
        });
        self.review = None;
    }

    pub(super) fn can_update(&self) -> bool {
        self.origin
            .as_ref()
            .is_some_and(|origin| origin.full_roster)
    }

    pub(in crate::app::host_groups) fn origin(&self) -> Option<(&str, &[String])> {
        self.origin
            .as_ref()
            .map(|origin| (origin.name.as_str(), origin.members.as_slice()))
    }

    pub(super) fn is_open(&self) -> bool {
        self.review.is_some()
    }

    fn set_review(&mut self, proposal: Proposal, feedback: &str) {
        self.revision += 1;
        self.review = Some(proposal);
        self.feedback = feedback.into();
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> Option<View> {
        let origin = self.origin.as_ref()?;
        let review = self.review.as_ref()?;
        let revision = self.revision;
        let status = if !self.feedback.is_empty() {
            self.feedback.as_str()
        } else if review.members.is_empty() {
            "A saved group needs at least one available SSH profile."
        } else if !review.can_save() {
            "The live roster already matches this saved group."
        } else {
            ""
        };
        let mut notes = Vec::new();
        if review.without_profile > 0 {
            notes.push(format!(
                "Panes without an available saved SSH profile: {}",
                review.without_profile
            ));
        }
        if review.unavailable > 0 {
            notes.push(format!(
                "Roster panes no longer available in this window: {}",
                review.unavailable
            ));
        }
        let changes = [("Add", &review.added), ("Remove", &review.removed)]
            .into_iter()
            .filter(|(_, profiles)| !profiles.is_empty())
            .map(|(label, profiles)| {
                KeyedView::new(
                    label,
                    StackPanel::new().spacing(4.0).keyed_children(
                        std::iter::once(KeyedView::new("label", TextBlock::new().text(label)))
                            .chain(profiles.iter().map(|profile| {
                                KeyedView::new(
                                    format!("profile-{profile}"),
                                    TextBlock::new()
                                        .automation_name(format!("{label} profile {profile}"))
                                        .text(profile)
                                        .text_wrapping(TextWrapping::Wrap),
                                )
                            })),
                    ),
                )
            });
        Some(
            Grid::new()
                .margin(16.0)
                .row_spacing(8.0)
                .rows([GridLength::Auto, GridLength::STAR, GridLength::Auto])
                .children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text(format!("Update saved SSH group · {}", origin.name)),
                    ScrollViewer::new()
                        .grid_row(1)
                        .content(StackPanel::new().spacing(10.0).keyed_children(changes)),
                    StackPanel::new().grid_row(2).spacing(6.0).children((
                        TextBlock::new()
                            .text(notes.join("\n"))
                            .text_wrapping(TextWrapping::Wrap),
                        TextBlock::new()
                            .text(status)
                            .text_wrapping(TextWrapping::Wrap),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(8.0)
                            .children((
                                Button::new()
                                    .style(ButtonStyle::Accent)
                                    .is_enabled(review.can_save())
                                    .on_click(
                                        ctx.callback(move |_| action(id, Action::Apply(revision))),
                                    )
                                    .content("Save roster changes"),
                                Button::new()
                                    .on_click(
                                        ctx.callback(move |_| action(id, Action::Cancel(revision))),
                                    )
                                    .content("Cancel"),
                            )),
                    )),
                )),
        )
    }
}

fn action(id: TabId, action: Action) -> Message {
    Message::HostGroup(HostAction::Saved(id, super::Action::UpdateRoster(action)))
}

impl TerminalApp {
    pub(super) fn handle_roster_update(&mut self, id: TabId, action: Action) {
        let opening = matches!(action, Action::Open);
        if opening && self.host_group_command_target() != Some(id) {
            return;
        }
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let state = &mut page.saved.roster_update;
        if !state.can_update() {
            return;
        }
        match action {
            Action::Cancel(revision) => {
                if state.revision == revision {
                    state.review = None;
                }
                return;
            }
            Action::Apply(revision)
                if state.revision != revision
                    || !state.review.as_ref().is_some_and(Proposal::can_save) =>
            {
                return;
            }
            _ => {}
        }
        if let Err(error) = self.review_roster_update(id, opening)
            && let Some(page) = self.host_group_mut(id)
        {
            let feedback = format!("{error:#}");
            if opening {
                page.saved.feedback = feedback;
            } else {
                page.saved.roster_update.feedback = feedback;
            }
        }
    }

    fn review_roster_update(&mut self, id: TabId, opening: bool) -> Result<()> {
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page) else {
            return Ok(());
        };
        let state = &page.saved.roster_update;
        let Some((roster, origin)) = page.roster.as_ref().zip(state.origin.as_ref()) else {
            return Ok(());
        };
        let catalog = self
            .store
            .catalog()
            .context("Could not read SSH profiles")?;
        ensure!(
            catalog.ssh_host_groups.get(&origin.name) == Some(&origin.members),
            "The SSH group changed on disk. Reopen it before saving."
        );
        let selection = Selection::capture(&self.host_group_targets(), &roster.panes, |name| {
            catalog.ssh_host_group_member(name).is_some()
        });
        let proposal = Proposal::from_selection(&origin.members, selection, roster.panes.len());
        if opening || state.review.as_ref() != Some(&proposal) {
            let feedback = if opening {
                ""
            } else {
                "Live panes changed. Review the new membership before saving."
            };
            if let Some(page) = self.host_group_mut(id) {
                page.saved.roster_update.set_review(proposal, feedback);
            }
            return Ok(());
        }
        let name = origin.name.clone();
        let catalog = self.store.save_ssh_host_group(
            &name,
            &proposal.members,
            Some((&name, &origin.members)),
        )?;
        self.use_group_catalog(catalog);
        if let Some(page) = self.host_group_mut(id) {
            page.saved
                .roster_update
                .remember(&name, &proposal.members, true);
            page.saved.feedback = format!("Saved live membership for {name}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_keeps_saved_order_and_records_additions_removals_and_unavailable_panes() {
        let original = ["East".into(), "Old".into(), "Central".into()];
        let selection = Selection {
            members: vec![
                "West".into(),
                "Central".into(),
                "East".into(),
                "North".into(),
            ],
            without_profile: 1,
            matched_panes: 5,
        };
        let proposal = Proposal::from_selection(&original, selection, 7);
        assert_eq!(proposal.members, ["East", "Central", "West", "North"]);
        assert_eq!(proposal.added, ["West", "North"]);
        assert_eq!(proposal.removed, ["Old"]);
        assert_eq!(proposal.without_profile, 1);
        assert_eq!(proposal.unavailable, 2);
        assert!(proposal.can_save());
        assert!(!Proposal::from_selection(&original, Selection::default(), 2).can_save());
        let reordered = Selection {
            members: original.iter().rev().cloned().collect(),
            matched_panes: 3,
            ..Default::default()
        };
        assert!(!Proposal::from_selection(&original, reordered, 3).can_save());
    }
}
