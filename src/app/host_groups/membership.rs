use super::*;

pub(super) const LABEL: &str = "Manage live panes…";

#[derive(Default)]
pub(super) struct State {
    revision: u64,
    draft: Option<Draft>,
}

struct Draft {
    members: BTreeSet<u64>,
    filter: filter::Filter,
    feedback: &'static str,
}

pub enum Action {
    Open,
    Form(u64, Change),
}

pub enum Change {
    Query(String),
    Check(u64, bool),
    Apply(BTreeSet<u64>),
    Cancel,
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.draft.is_some()
    }

    fn open(&mut self, members: BTreeSet<u64>) {
        self.revision += 1;
        self.draft = Some(Draft {
            members,
            filter: filter::Filter::default(),
            feedback: "",
        });
    }

    pub(super) fn view(
        &self,
        id: TabId,
        original: &BTreeSet<u64>,
        targets: &[Target],
        ctx: &ViewContext<TerminalApp>,
    ) -> Option<View> {
        let draft = self.draft.as_ref()?;
        let revision = self.revision;
        let action = move |change| {
            Message::HostGroup(super::Action::Membership(
                id,
                Action::Form(revision, change),
            ))
        };
        let matches: Vec<_> = targets
            .iter()
            .filter(|target| draft.filter.matches(target))
            .collect();
        let rows = matches.iter().map(|target| {
            let pane = target.pane;
            KeyedView::new(
                pane.to_string(),
                StackPanel::new().spacing(2.0).children((
                    CheckBox::new()
                        .automation_name(format!("Include {}", target.name))
                        .is_checked(draft.members.contains(&pane))
                        .on_is_checked_changed(
                            ctx.callback(move |checked| action(Change::Check(pane, checked))),
                        )
                        .content(target.name.as_str()),
                    TextBlock::new().text(target.state.label()),
                )),
            )
        });
        let changes = draft.members != *original;
        let offered = draft.members.clone();
        let feedback = if !draft.feedback.is_empty() {
            draft.feedback
        } else if matches.is_empty() {
            if targets.is_empty() {
                "Open an SSH pane in this window to add it to the group."
            } else {
                "No SSH panes match this search."
            }
        } else {
            ""
        };
        Some(
            Grid::new()
                .margin(16.0)
                .row_spacing(8.0)
                .rows([GridLength::Auto, GridLength::STAR])
                .children((
                    StackPanel::new().spacing(8.0).children((
                        TextBlock::new().font_size(24.0).text("Manage live SSH panes"),
                        TextBlock::new()
                            .text("Choose the panes that belong to this tab. Select send targets in the roster.")
                            .text_wrapping(TextWrapping::Wrap),
                        TextBox::new()
                            .automation_name("Find SSH panes")
                            .placeholder_text("Filter by profile, pane, or SSH address")
                            .text(draft.filter.query())
                            .on_text_changed(ctx.callback(move |query| action(Change::Query(query)))),
                        StackPanel::new()
                            .spacing(8.0)
                            .orientation(Orientation::Horizontal)
                            .children((
                                Button::new()
                                    .style(ButtonStyle::Accent)
                                    .is_enabled(changes)
                                    .on_click(ctx.callback(move |_| action(Change::Apply(offered.clone()))))
                                    .content("Apply membership"),
                                Button::new()
                                    .on_click(ctx.callback(move |_| action(Change::Cancel)))
                                    .content("Cancel"),
                            )),
                        if feedback.is_empty() {
                            View::empty()
                        } else {
                            TextBlock::new().text(feedback).text_wrapping(TextWrapping::Wrap).into()
                        },
                    )),
                    ScrollViewer::new().grid_row(1).content(
                        StackPanel::new().spacing(8.0).keyed_children(rows)
                    ),
                )),
        )
    }
}

impl Draft {
    fn approve(
        &mut self,
        offered: BTreeSet<u64>,
        original: &BTreeSet<u64>,
        candidates: &BTreeSet<u64>,
    ) -> Option<BTreeSet<u64>> {
        if self.members != offered || offered == *original {
            return None;
        }
        let count = self.members.len();
        self.members
            .retain(|pane| original.contains(pane) || candidates.contains(pane));
        if self.members.len() != count {
            self.feedback =
                "A pane left this window. Review the remaining changes and apply again.";
            return None;
        }
        Some(offered)
    }
}

impl HostGroup {
    pub(super) fn edit_membership_open(&self) -> bool {
        self.roster
            .as_ref()
            .is_some_and(|roster| roster.membership.is_open())
    }

    fn finish_membership(&mut self, members: BTreeSet<u64>) {
        let Some(roster) = &mut self.roster else {
            return;
        };
        let unchanged = |pane: &u64| roster.panes.contains(pane) == members.contains(pane);
        self.selected.retain(unchanged);
        if let Some(scope) = self
            .previous_draft
            .as_mut()
            .and_then(|backup| backup.scope.as_mut())
        {
            scope.selected.retain(unchanged);
        }
        roster.panes = members;
        roster.membership.draft = None;
    }
}

impl TerminalApp {
    pub(super) fn handle_group_membership(&mut self, id: TabId, action: Action) {
        let (revision, change) = match action {
            Action::Open => {
                if self.host_group_command_target() == Some(id)
                    && let Some(roster) = self
                        .host_group_mut(id)
                        .and_then(|page| page.roster.as_mut())
                {
                    roster.membership.open(roster.panes.clone());
                }
                return;
            }
            Action::Form(revision, change) => (revision, change),
        };
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let candidates = if matches!(change, Change::Apply(_) | Change::Check(_, true)) {
            self.host_group_targets()
                .into_iter()
                .map(|target| target.pane)
                .collect()
        } else {
            BTreeSet::new()
        };
        let Some(page) = self.host_group_mut(id) else {
            return;
        };
        let Some(roster) = &mut page.roster else {
            return;
        };
        let state = &mut roster.membership;
        let Some(draft) = state.draft.as_mut().filter(|_| revision == state.revision) else {
            return;
        };
        match change {
            Change::Query(query) => draft.filter.set_query(query),
            Change::Check(pane, true) if candidates.contains(&pane) => {
                draft.members.insert(pane);
                draft.feedback = "";
            }
            Change::Check(pane, false) => {
                draft.members.remove(&pane);
                draft.feedback = "";
            }
            Change::Apply(offered) => {
                if let Some(members) = draft.approve(offered, &roster.panes, &candidates) {
                    page.finish_membership(members);
                }
            }
            Change::Cancel => state.draft = None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disappeared_pane_requires_a_second_review_before_applying_other_changes() {
        let original = BTreeSet::from([1, 2]);
        let live = BTreeSet::from([1, 2, 4]);
        let offered = BTreeSet::from([1, 3, 4]);
        let mut draft = Draft {
            members: offered.clone(),
            filter: filter::Filter::default(),
            feedback: "",
        };
        assert!(draft.approve(offered.clone(), &original, &live).is_none());
        assert!(!draft.feedback.is_empty());
        assert_eq!(draft.members, BTreeSet::from([1, 4]));
        assert!(draft.approve(offered, &original, &live).is_none());
        assert_eq!(
            draft.approve(draft.members.clone(), &original, &live),
            Some(BTreeSet::from([1, 4]))
        );
    }

    #[test]
    fn applying_membership_clears_only_changed_send_selections_and_retry_backups() {
        let mut group = HostGroup {
            roster: Some(Box::new(MemberRoster {
                panes: BTreeSet::from([1, 2]),
                show_window: true,
                ..Default::default()
            })),
            selected: BTreeSet::from([1, 2, 3, 4]),
            draft: "still editing".into(),
            previous_draft: Some(DraftBackup {
                text: "previous".into(),
                scope: Some(retry::Scope {
                    selected: BTreeSet::from([2, 3, 4]),
                    filter: Default::default(),
                }),
            }),
            ..Default::default()
        };
        group.finish_membership(BTreeSet::from([1, 3]));
        assert_eq!(group.roster.as_ref().unwrap().panes, BTreeSet::from([1, 3]));
        assert_eq!(group.selected, BTreeSet::from([1, 4]));
        assert_eq!(group.draft, "still editing");
        assert!(group.restore_draft());
        assert_eq!(group.selected, BTreeSet::from([4]));
    }
}
