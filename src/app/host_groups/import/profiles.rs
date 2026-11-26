use super::*;
use crate::app::host_groups::filter::PROFILE_PAGE_SIZE;

pub enum Change {
    Filter(String),
    Page(usize),
    Check(usize, bool),
    CheckMatches(Vec<usize>, bool),
}

pub(super) struct Picker {
    entries: Vec<Entry>,
    checked: BTreeSet<usize>,
    filter: filter::Filter,
    page: usize,
}

struct Entry {
    name: String,
    ssh: SshOptions,
    address: String,
}

impl Picker {
    pub(super) fn new(catalog: &Profiles) -> Self {
        Self {
            entries: Self::entries(catalog),
            checked: BTreeSet::new(),
            filter: filter::Filter::default(),
            page: 0,
        }
    }

    fn entries(catalog: &Profiles) -> Vec<Entry> {
        catalog
            .profile_order
            .iter()
            .filter_map(|name| {
                let ssh = catalog.ssh_host_group_member(name)?;
                Some(Entry {
                    address: list::address(ssh),
                    name: name.clone(),
                    ssh: ssh.clone(),
                })
            })
            .collect()
    }

    pub(super) fn refresh(&mut self, catalog: &Profiles) {
        let previous = std::mem::replace(&mut self.entries, Self::entries(catalog));
        let selected: std::collections::BTreeMap<_, _> = self
            .checked
            .iter()
            .map(|&index| {
                let entry = &previous[index];
                (&entry.name, &entry.ssh)
            })
            .collect();
        self.checked = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                (selected.get(&entry.name).copied() == Some(&entry.ssh)).then_some(index)
            })
            .collect();
        self.page = 0;
    }

    pub(super) fn rows(&self, catalog: &Profiles) -> Result<Vec<list::Row>> {
        ensure!(
            !self.checked.is_empty(),
            "Check at least one saved SSH profile."
        );
        self.checked
            .iter()
            .enumerate()
            .map(|(line, &index)| {
                let entry = &self.entries[index];
                ensure!(
                    catalog.ssh_host_group_member(&entry.name) == Some(&entry.ssh),
                    "SSH profile {} changed. Refresh saved profiles and review again.",
                    entry.name
                );
                Ok(list::Row {
                    line: line + 1,
                    target: Ok(list::Target {
                        profile: Some(entry.name.clone()),
                        ssh: entry.ssh.clone(),
                    }),
                })
            })
            .collect()
    }

    fn matches(&self) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                self.filter
                    .matches_profile(&entry.name, Some(&entry.address))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn visible(&self, matches: &[usize]) -> std::ops::Range<usize> {
        let start = self.page * PROFILE_PAGE_SIZE;
        start..(start + PROFILE_PAGE_SIZE).min(matches.len())
    }

    pub(super) fn edit(&mut self, change: Change) -> Result<()> {
        match change {
            Change::Filter(query) if query != self.filter.query() => {
                self.filter.set_query(query);
                self.page = 0;
            }
            Change::Page(page) if page < self.matches().len().div_ceil(PROFILE_PAGE_SIZE) => {
                self.page = page;
            }
            Change::Check(index, checked) => {
                let matches = self.matches();
                if matches[self.visible(&matches)].contains(&index) {
                    if checked {
                        ensure!(
                            self.checked.contains(&index) || self.checked.len() < list::MAX_TARGETS,
                            "Check at most {} saved SSH profiles at a time.",
                            list::MAX_TARGETS
                        );
                        self.checked.insert(index);
                    } else {
                        self.checked.remove(&index);
                    }
                }
            }
            Change::CheckMatches(matches, checked) if matches == self.matches() => {
                if checked {
                    let total = self.checked.len()
                        + matches
                            .iter()
                            .filter(|index| !self.checked.contains(index))
                            .count();
                    ensure!(
                        total <= list::MAX_TARGETS,
                        "Check at most {} saved SSH profiles at a time.",
                        list::MAX_TARGETS
                    );
                    self.checked.extend(matches);
                } else {
                    self.checked
                        .retain(|index| matches.binary_search(index).is_err());
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn view(
        &self,
        send: impl Fn(super::Change) -> Message + Copy + 'static,
        cancel: View,
        error: View,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let action = move |change| send(super::Change::Picker(change));
        let matches = self.matches();
        let pages = matches.len().div_ceil(PROFILE_PAGE_SIZE).max(1);
        let page = self.page;
        let rows = matches[self.visible(&matches)].iter().map(|&index| {
            let entry = &self.entries[index];
            let label = format!("{} · {}", entry.name, entry.address);
            KeyedView::new(
                index,
                CheckBox::new()
                    .automation_name(format!("Add SSH profile {}", entry.name))
                    .is_checked(self.checked.contains(&index))
                    .on_is_checked_changed(
                        ctx.callback(move |checked| action(Change::Check(index, checked))),
                    )
                    .content(label),
            )
        });
        let check_matches = |label, checked| {
            let enabled = matches
                .iter()
                .any(|index| self.checked.contains(index) != checked);
            let matches = matches.clone();
            Button::new()
                .is_enabled(enabled)
                .on_click(
                    ctx.callback(move |_| action(Change::CheckMatches(matches.clone(), checked))),
                )
                .content(label)
        };
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(8.0).children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text("Add saved SSH profiles"),
                    TextBox::new()
                        .automation_name("Find saved SSH profiles")
                        .placeholder_text("Filter by profile or SSH address")
                        .text(self.filter.query())
                        .on_text_changed(ctx.callback(move |query| action(Change::Filter(query)))),
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
                                    "{} checked · {} matches · Page {} of {pages}",
                                    self.checked.len(),
                                    matches.len(),
                                    page + 1
                                )),
                        )),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(6.0)
                        .children((
                            check_matches("Check matches", true),
                            check_matches("Uncheck matches", false),
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .is_enabled(!self.checked.is_empty())
                                .on_click(ctx.callback(move |_| send(super::Change::Preview)))
                                .content("Review selected profiles"),
                            Button::new()
                                .on_click(
                                    ctx.callback(move |_| send(super::Change::RefreshProfiles)),
                                )
                                .content("Refresh saved profiles"),
                            cancel,
                        )),
                    error,
                )),
                ScrollViewer::new()
                    .grid_row(1)
                    .content(if matches.is_empty() {
                        TextBlock::new().text("No saved SSH profiles match.").into()
                    } else {
                        StackPanel::new().spacing(6.0).keyed_children(rows)
                    }),
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_rechecks_saved_identities_and_keeps_unchanged_checks() {
        let mut catalog = Profiles::default();
        let ssh: SshOptions = serde_json::from_value(serde_json::json!({
            "host": "fleet.example", "user": "ops", "auth": "none"
        }))
        .unwrap();
        catalog.profiles.insert("First".into(), ssh.clone());
        catalog.profiles.insert("Second".into(), ssh);
        catalog.profile_order = vec!["First".into(), "Second".into()];
        let mut picker = Picker::new(&catalog);
        picker.edit(Change::CheckMatches(vec![0, 1], true)).unwrap();
        assert_eq!(picker.rows(&catalog).unwrap().len(), 2);

        catalog.profiles.get_mut("Second").unwrap().port = 2222;
        assert!(picker.rows(&catalog).is_err());
        picker.refresh(&catalog);
        let rows = picker.rows(&catalog).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].target.as_ref().unwrap().profile.as_deref(),
            Some("First")
        );
    }
}
