use super::{Action as HostAction, *};
use anyhow::{Result, bail, ensure};
use std::collections::BTreeMap;
use w11term::options::SshRecipientSets;

type Candidates = BTreeMap<String, Vec<u64>>;

#[derive(Default)]
pub(super) struct State {
    editor: Option<Editor>,
    revision: u64,
    feedback: String,
}

struct Editor {
    group: String,
    sets: SshRecipientSets,
    captured: Result<Vec<String>, String>,
    name: String,
    confirm_delete: Option<String>,
}

struct RecipientGroup<'a> {
    page: &'a HostGroup,
    roster: &'a MemberRoster,
    name: &'a str,
    members: &'a [String],
    catalog: Profiles,
}

pub enum Action {
    Open,
    Edit(u64, Change),
}

pub enum Change {
    Name(String),
    Create,
    Apply(String),
    Replace(String),
    ConfirmDelete(String),
    Delete(String),
    Back,
}

fn resolve(members: &[String], group: &[String], candidates: &Candidates) -> Result<BTreeSet<u64>> {
    ensure!(!members.is_empty(), "This recipient set is empty.");
    let selected = members
        .iter()
        .map(|profile| {
            ensure!(
                group.contains(profile),
                "{profile} no longer belongs to this saved SSH group."
            );
            match candidates.get(profile).map(Vec::as_slice) {
                Some([pane]) => Ok(*pane),
                Some(_) => bail!("{profile} matches more than one live pane in this roster."),
                None => bail!("{profile} has no matching live pane in this roster."),
            }
        })
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        selected.len() == members.len(),
        "This recipient set has duplicate profiles."
    );
    Ok(selected)
}

fn capture(
    shown_checked: &BTreeSet<u64>,
    group: &[String],
    candidates: &Candidates,
) -> Result<Vec<String>> {
    ensure!(
        !shown_checked.is_empty(),
        "Check the shown recipients you want to save, then reopen Recipient sets."
    );
    let profiles: Vec<_> = group
        .iter()
        .filter(|profile| {
            candidates
                .get(*profile)
                .is_some_and(|panes| panes.iter().any(|pane| shown_checked.contains(pane)))
        })
        .cloned()
        .collect();
    ensure!(
        !profiles.is_empty() && resolve(&profiles, group, candidates)? == *shown_checked,
        "Each shown checked pane must match one saved profile in this group's roster."
    );
    Ok(profiles)
}

pub(super) fn button(id: TabId, ctx: &ViewContext<TerminalApp>) -> View {
    Button::new()
        .on_click(
            ctx.callback(move |_| Message::HostGroup(HostAction::RecipientSets(id, Action::Open))),
        )
        .content("Recipient sets…")
}

impl State {
    pub(super) fn is_open(&self) -> bool {
        self.editor.is_some()
    }

    pub(super) fn view(&self, id: TabId, ctx: &ViewContext<TerminalApp>) -> Option<View> {
        let editor = self.editor.as_ref()?;
        let revision = self.revision;
        let action = move |change| {
            Message::HostGroup(HostAction::RecipientSets(
                id,
                Action::Edit(revision, change),
            ))
        };
        let can_capture = editor.captured.is_ok();
        let rows = editor.sets.iter().map(|(name, members)| {
            let apply = name.clone();
            let replace = name.clone();
            let remove = name.clone();
            let confirm = editor.confirm_delete.as_ref() == Some(name);
            let label = if confirm { "Confirm delete" } else { "Delete" };
            KeyedView::new(
                name.as_str(),
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new()
                        .text(format!("{name} · {}", members.join(", ")))
                        .text_wrapping(TextWrapping::Wrap),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .automation_name(format!("Apply recipient set {name}"))
                                .on_click(
                                    ctx.callback(move |_| action(Change::Apply(apply.clone()))),
                                )
                                .content("Apply"),
                            Button::new()
                                .automation_name(format!("Replace recipient set {name}"))
                                .is_enabled(can_capture)
                                .on_click(
                                    ctx.callback(move |_| action(Change::Replace(replace.clone()))),
                                )
                                .content("Replace with selection"),
                            Button::new()
                                .automation_name(format!("{label} recipient set {name}"))
                                .on_click(ctx.callback(move |_| {
                                    action(if confirm {
                                        Change::Delete(remove.clone())
                                    } else {
                                        Change::ConfirmDelete(remove.clone())
                                    })
                                }))
                                .content(label),
                        )),
                )),
            )
        });
        let capture_text = match &editor.captured {
            Ok(members) => format!("Current selection · {}", members.join(", ")),
            Err(error) => error.clone(),
        };
        let status = if self.feedback.is_empty() {
            View::empty()
        } else {
            TextBlock::new()
                .text(&self.feedback)
                .text_wrapping(TextWrapping::Wrap)
                .into()
        };
        Some(
            Grid::new()
                .margin(16.0)
                .row_spacing(8.0)
                .rows([GridLength::Auto, GridLength::STAR, GridLength::Auto])
                .children((
                    StackPanel::new().spacing(6.0).children((
                        TextBlock::new()
                            .font_size(24.0)
                            .text(format!("Recipient sets · {}", editor.group)),
                        TextBlock::new()
                            .text(capture_text)
                            .text_wrapping(TextWrapping::Wrap),
                        Grid::new()
                            .column_spacing(8.0)
                            .columns([GridLength::STAR, GridLength::Auto])
                            .children((
                                TextBox::new()
                                    .automation_name("Recipient set name")
                                    .placeholder_text("Recipient set name")
                                    .text(&editor.name)
                                    .on_text_changed(
                                        ctx.callback(move |name| action(Change::Name(name))),
                                    ),
                                Button::new()
                                    .grid_column(1)
                                    .is_enabled(can_capture && !editor.name.trim().is_empty())
                                    .on_click(ctx.callback(move |_| action(Change::Create)))
                                    .content("Save recipient set"),
                            )),
                    )),
                    ScrollViewer::new()
                        .grid_row(1)
                        .content(StackPanel::new().spacing(12.0).keyed_children(rows)),
                    StackPanel::new().grid_row(2).spacing(6.0).children((
                        status,
                        Button::new()
                            .on_click(ctx.callback(move |_| action(Change::Back)))
                            .content("Back to SSH group"),
                    )),
                )),
        )
    }
}

impl TerminalApp {
    fn recipient_candidates(&self, roster: &MemberRoster, catalog: &Profiles) -> Candidates {
        let mut matches = Candidates::new();
        for entry in self.tabs.entries() {
            let Page::Terminal(live) = &entry.value.page else {
                continue;
            };
            let pane = live.session.pane_id;
            let Some(profile) = live.profile_name.as_ref() else {
                continue;
            };
            if roster.panes.contains(&pane)
                && live.session.is_direct_ssh_shell()
                && matches!(&live.source, Source::Ssh(options)
                    if catalog.ssh_host_group_member(profile) == Some(options))
            {
                matches.entry(profile.clone()).or_default().push(pane);
            }
        }
        matches
    }

    pub(super) fn handle_recipient_sets(&mut self, id: TabId, action: Action) {
        let result = match action {
            Action::Open if self.host_group_command_target() == Some(id) => {
                self.open_recipient_sets(id)
            }
            Action::Edit(revision, change) if self.tabs.active_id() == Some(id) => {
                self.edit_recipient_sets(id, revision, change)
            }
            _ => return,
        };
        if let Err(error) = result
            && let Some(page) = self.host_group_mut(id)
        {
            let feedback = format!("{error:#}");
            if page.saved.recipient_sets.is_open() {
                page.saved.recipient_sets.feedback = feedback;
            } else {
                page.saved.feedback = feedback;
            }
        }
    }

    fn current_recipient_group(&self, id: TabId) -> Result<RecipientGroup<'_>> {
        let Some(Page::HostGroup(page)) = self.tabs.get(id).map(|tab| &tab.page) else {
            bail!("This SSH group tab has closed.");
        };
        let Some((roster, (group, members))) = page.roster.as_deref().zip(page.saved.origin())
        else {
            bail!("Open a saved SSH group in a named tab to use recipient sets.");
        };
        let catalog = self.store.catalog()?;
        ensure!(
            catalog.ssh_host_groups.get(group).map(Vec::as_slice) == Some(members),
            "The SSH group changed on disk. Reopen the group before using recipient sets."
        );
        Ok(RecipientGroup {
            page,
            roster,
            name: group,
            members,
            catalog,
        })
    }

    fn open_recipient_sets(&mut self, id: TabId) -> Result<()> {
        let RecipientGroup {
            page,
            roster,
            name: group,
            members,
            catalog,
        } = self.current_recipient_group(id)?;
        let sets = catalog
            .ssh_recipient_sets
            .get(group)
            .cloned()
            .unwrap_or_default();
        let captured = capture(
            &page.selected_shown(&self.host_group_targets()),
            members,
            &self.recipient_candidates(roster, &catalog),
        )
        .map_err(|error| error.to_string());
        let editor = Editor {
            group: group.into(),
            sets,
            captured,
            name: String::new(),
            confirm_delete: None,
        };
        if let Some(page) = self.host_group_mut(id) {
            let state = &mut page.saved.recipient_sets;
            state.revision += 1;
            state.editor = Some(editor);
            state.feedback.clear();
        }
        Ok(())
    }

    fn edit_recipient_sets(&mut self, id: TabId, revision: u64, change: Change) -> Result<()> {
        let Some(page) = self.host_group_mut(id) else {
            return Ok(());
        };
        let state = &mut page.saved.recipient_sets;
        let Some(editor) = state.editor.as_mut().filter(|_| state.revision == revision) else {
            return Ok(());
        };
        match change {
            Change::Name(name) => editor.name = name,
            Change::Back => state.editor = None,
            Change::ConfirmDelete(name) => editor.confirm_delete = Some(name),
            Change::Apply(name) => {
                if let Some(previous) = editor.sets.get(&name).cloned() {
                    self.apply_recipient_set(id, &name, &previous)?;
                }
            }
            Change::Create | Change::Replace(_) | Change::Delete(_) => {
                let (name, previous) = match &change {
                    Change::Create => (editor.name.trim().to_owned(), None),
                    Change::Replace(name) | Change::Delete(name) => {
                        let Some(previous) = editor.sets.get(name).cloned() else {
                            return Ok(());
                        };
                        (name.clone(), Some(previous))
                    }
                    _ => unreachable!(),
                };
                let delete = matches!(change, Change::Delete(_));
                if delete && editor.confirm_delete.as_ref() != Some(&name) {
                    return Ok(());
                }
                let reviewed = editor.captured.clone();
                self.save_recipient_set(id, &name, previous.as_deref(), reviewed, delete)?;
            }
        }
        Ok(())
    }

    fn apply_recipient_set(&mut self, id: TabId, name: &str, previous: &[String]) -> Result<()> {
        let RecipientGroup {
            roster,
            name: group,
            members,
            catalog,
            ..
        } = self.current_recipient_group(id)?;
        ensure!(
            catalog
                .ssh_recipient_sets
                .get(group)
                .and_then(|sets| sets.get(name))
                .map(Vec::as_slice)
                == Some(previous),
            "The recipient set changed on disk. Reopen Recipient sets."
        );
        let selected = resolve(
            previous,
            members,
            &self.recipient_candidates(roster, &catalog),
        )?;
        if let Some(page) = self.host_group_mut(id) {
            page.show_window_roster(false);
            page.filter.clear();
            page.selected = selected;
            page.saved.recipient_sets.editor = None;
            page.saved.feedback = format!("Applied recipient set {name}");
        }
        Ok(())
    }

    fn save_recipient_set(
        &mut self,
        id: TabId,
        name: &str,
        previous: Option<&[String]>,
        reviewed: Result<Vec<String>, String>,
        delete: bool,
    ) -> Result<()> {
        let RecipientGroup {
            page,
            roster,
            name: group,
            members,
            catalog,
        } = self.current_recipient_group(id)?;
        let replacement = if delete {
            None
        } else {
            let current = capture(
                &page.selected_shown(&self.host_group_targets()),
                members,
                &self.recipient_candidates(roster, &catalog),
            )?;
            ensure!(
                reviewed.as_ref().ok() == Some(&current),
                "The selected recipients changed. Reopen Recipient sets."
            );
            Some(current)
        };
        let catalog = self.store.change_ssh_recipient_set(
            group,
            members,
            name,
            previous,
            replacement.as_deref(),
        )?;
        let sets = catalog
            .ssh_recipient_sets
            .get(group)
            .cloned()
            .unwrap_or_default();
        self.use_group_catalog(catalog);
        if let Some(page) = self.host_group_mut(id) {
            let state = &mut page.saved.recipient_sets;
            if let Some(editor) = &mut state.editor {
                editor.sets = sets;
                editor.name.clear();
                editor.confirm_delete = None;
                state.revision += 1;
            }
            state.feedback = if delete {
                format!("Deleted recipient set {name}")
            } else {
                format!("Saved recipient set {name}")
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_members_capture_in_group_order_and_resolve_all_or_fail() {
        let group = ["East".into(), "West".into(), "North".into()];
        let mut candidates = Candidates::from([
            ("East".into(), vec![8]),
            ("West".into(), vec![3]),
            ("Other".into(), vec![4]),
        ]);
        let selected = BTreeSet::from([3, 8]);
        let names = capture(&selected, &group, &candidates).unwrap();
        assert_eq!(names, ["East", "West"]);
        assert_eq!(resolve(&names, &group, &candidates).unwrap(), selected);
        assert!(capture(&BTreeSet::from([3, 4]), &group, &candidates).is_err());
        assert!(capture(&BTreeSet::new(), &group, &candidates).is_err());
        assert!(resolve(&group, &group, &candidates).is_err());
        assert!(resolve(&["East".into(), "East".into()], &group, &candidates).is_err());
        assert!(resolve(&names, &group[..1], &candidates).is_err());
        candidates.insert("West".into(), vec![3, 5]);
        assert!(capture(&selected, &group, &candidates).is_err());
        assert!(resolve(&names, &group, &candidates).is_err());
    }
}
