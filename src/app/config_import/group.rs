use super::*;
use anyhow::{Context, ensure};
use std::collections::BTreeSet;
use w11term::{options::SshOptions, profiles::ImportedGroupMember};

const PAGE_SIZE: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    New,
    Reuse,
}

pub enum Change {
    Name(String),
    Profile(usize, String),
    Filter(String),
    Page(usize),
    Check(usize, Choice),
    Uncheck(usize),
    CheckPage(Vec<(usize, Choice)>),
    Clear,
    Save,
    Back,
}

#[derive(Default)]
pub(super) struct Editor {
    name: String,
    query: String,
    page: usize,
    checked: BTreeMap<usize, Choice>,
    profile_names: BTreeMap<usize, String>,
    error: String,
}

impl Editor {
    fn profile_name<'a>(&'a self, index: usize, alias: &'a ImportedAlias) -> &'a str {
        self.profile_names
            .get(&index)
            .map_or(&alias.name, String::as_str)
    }

    fn choice(
        &self,
        index: usize,
        alias: &ImportedAlias,
        catalog: &Profiles,
    ) -> Result<Choice, &'static str> {
        let Some(options) = &alias.options else {
            return Err("This alias cannot be imported");
        };
        if !self.profile_names.contains_key(&index)
            && self.checked.get(&index) != Some(&Choice::New)
            && catalog.ssh_host_group_member(&alias.name) == Some(options)
        {
            return Ok(Choice::Reuse);
        }
        let name = self.profile_name(index, alias).trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            Err("Enter a profile name on one line")
        } else if catalog.profiles.contains_key(name) || catalog.local.contains_key(name) {
            Err("Profile name already exists. Enter a different name.")
        } else {
            Ok(Choice::New)
        }
    }

    fn matches(&self, report: &ImportReport) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        report
            .aliases
            .iter()
            .enumerate()
            .filter_map(|(index, alias)| {
                (query.is_empty() || alias.name.to_lowercase().contains(&query)).then_some(index)
            })
            .collect()
    }

    fn shown<'a>(&self, matches: &'a [usize]) -> &'a [usize] {
        let start = (self.page * PAGE_SIZE).min(matches.len());
        &matches[start..(start + PAGE_SIZE).min(matches.len())]
    }

    fn available(
        &self,
        shown: &[usize],
        report: &ImportReport,
        catalog: &Profiles,
    ) -> Vec<(usize, Choice)> {
        shown
            .iter()
            .filter_map(|&index| {
                Some((
                    index,
                    self.choice(index, report.aliases.get(index)?, catalog)
                        .ok()?,
                ))
            })
            .collect()
    }

    fn entries<'a>(
        &'a self,
        report: &'a ImportReport,
        catalog: &Profiles,
    ) -> anyhow::Result<Vec<ImportedGroupMember<'a>>> {
        let mut names = BTreeSet::new();
        self.checked
            .iter()
            .map(|(&index, &selected)| {
                let alias = report
                    .aliases
                    .get(index)
                    .context("The reviewed aliases changed")?;
                let options = alias
                    .options
                    .as_ref()
                    .context("The selected alias is unavailable")?;
                let current = self
                    .choice(index, alias, catalog)
                    .map_err(|error| anyhow::anyhow!("OpenSSH alias {}. {error}", alias.name))?;
                ensure!(
                    current == selected,
                    "Saved profile {} changed. Uncheck and review the alias again.",
                    alias.name
                );
                let name = match selected {
                    Choice::New => self.profile_name(index, alias).trim(),
                    Choice::Reuse => &alias.name,
                };
                ensure!(
                    names.insert(name),
                    "Profile {name} is selected more than once"
                );
                Ok(match selected {
                    Choice::New => ImportedGroupMember::New { name, options },
                    Choice::Reuse => ImportedGroupMember::Existing {
                        name,
                        reviewed: options,
                    },
                })
            })
            .collect()
    }

    fn edit(&mut self, change: Change, report: &ImportReport, catalog: &Profiles) {
        match change {
            Change::Name(name) => self.name = name,
            Change::Profile(index, name)
                if self.shown(&self.matches(report)).contains(&index)
                    && report.aliases.get(index).is_some_and(|alias| {
                        alias.options.is_some()
                            && self.choice(index, alias, catalog) != Ok(Choice::Reuse)
                    }) =>
            {
                self.profile_names.insert(index, name);
            }
            Change::Filter(query) if query != self.query => {
                self.query = query;
                self.page = 0;
            }
            Change::Page(page) if page < self.matches(report).len().div_ceil(PAGE_SIZE) => {
                self.page = page
            }
            Change::Check(index, selected)
                if self.shown(&self.matches(report)).contains(&index)
                    && report.aliases.get(index).is_some_and(|alias| {
                        self.choice(index, alias, catalog) == Ok(selected)
                    }) =>
            {
                self.checked.insert(index, selected);
            }
            Change::Uncheck(index) if self.shown(&self.matches(report)).contains(&index) => {
                self.checked.remove(&index);
            }
            Change::CheckPage(available)
                if available
                    == self.available(self.shown(&self.matches(report)), report, catalog) =>
            {
                self.checked.extend(available);
            }
            Change::Clear => self.checked.clear(),
            _ => return,
        }
        self.error.clear();
    }

    pub(super) fn view(
        &self,
        id: TabId,
        revision: u64,
        report: &ImportReport,
        catalog: &Profiles,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let action = move |change| Message::ImportConfig(Action::Group(id, revision, change));
        let matches = self.matches(report);
        let shown = self.shown(&matches);
        let available = self.available(shown, report, catalog);
        let entries = self.entries(report, catalog);
        let valid =
            !self.name.trim().is_empty() && entries.as_ref().is_ok_and(|items| !items.is_empty());
        let selection_error = entries.err().map(|error| error.to_string());
        let error = if self.error.is_empty() {
            selection_error.as_deref().unwrap_or("")
        } else {
            &self.error
        };
        let pages = matches.len().div_ceil(PAGE_SIZE).max(1);
        let page = self.page;
        let new = self
            .checked
            .values()
            .filter(|&&entry| entry == Choice::New)
            .count();
        let content = if shown.is_empty() {
            TextBlock::new().text("No aliases match the filter.").into()
        } else {
            StackPanel::new()
                .spacing(12.0)
                .keyed_children(shown.iter().map(|&index| {
                    let alias = &report.aliases[index];
                    let (state, check) = match self.choice(index, alias, catalog) {
                        Ok(available) => {
                            let label = match available {
                                Choice::New => "Create profile",
                                Choice::Reuse => "Use saved profile",
                            };
                            (label, Some(available))
                        }
                        Err(error) => (error, None),
                    };
                    let checkbox = CheckBox::new()
                        .automation_name(format!("Include OpenSSH alias {}", alias.name))
                        .is_enabled(check.is_some() || self.checked.contains_key(&index))
                        .is_checked(self.checked.contains_key(&index))
                        .on_is_checked_changed(ctx.callback(move |checked| {
                            if checked {
                                check.map_or(Message::ControlIdle, |choice| {
                                    action(Change::Check(index, choice))
                                })
                            } else {
                                action(Change::Uncheck(index))
                            }
                        }))
                        .content(alias.name.as_str());
                    let profile_name = if alias.options.is_some() && check != Some(Choice::Reuse) {
                        TextBox::new()
                            .automation_name(format!(
                                "Profile name for OpenSSH alias {}",
                                alias.name
                            ))
                            .placeholder_text("Profile name")
                            .text(self.profile_name(index, alias))
                            .on_text_changed(
                                ctx.callback(move |name| action(Change::Profile(index, name))),
                            )
                            .into()
                    } else {
                        View::empty()
                    };
                    let details = alias.options.as_ref().map_or_else(View::empty, |options| {
                        TextBlock::new()
                            .text(details(options))
                            .text_wrapping(TextWrapping::Wrap)
                            .into()
                    });
                    KeyedView::new(
                        index,
                        StackPanel::new().spacing(4.0).children((
                            checkbox,
                            TextBlock::new().text(state),
                            profile_name,
                            details,
                            optional_lines(&alias.warnings),
                        )),
                    )
                }))
        };
        Grid::new()
            .margin(16.0)
            .row_spacing(8.0)
            .rows([GridLength::Auto, GridLength::STAR])
            .children((
                StackPanel::new().spacing(6.0).children((
                    TextBlock::new()
                        .font_size(24.0)
                        .text("Create SSH group from OpenSSH aliases"),
                    TextBox::new()
                        .automation_name("OpenSSH group name")
                        .placeholder_text("Group name")
                        .text(&self.name)
                        .on_text_changed(ctx.callback(move |name| action(Change::Name(name)))),
                    TextBox::new()
                        .automation_name("Filter OpenSSH aliases")
                        .placeholder_text("Filter aliases")
                        .text(&self.query)
                        .on_text_changed(ctx.callback(move |query| action(Change::Filter(query)))),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .is_enabled(!available.is_empty())
                                .on_click(ctx.callback(move |_| {
                                    action(Change::CheckPage(available.clone()))
                                }))
                                .content("Check shown aliases"),
                            Button::new()
                                .is_enabled(!self.checked.is_empty())
                                .on_click(ctx.callback(move |_| action(Change::Clear)))
                                .content("Clear checked aliases"),
                            Button::new()
                                .is_enabled(page > 0)
                                .on_click(ctx.callback(move |_| {
                                    action(Change::Page(page.saturating_sub(1)))
                                }))
                                .content("Previous aliases"),
                            Button::new()
                                .is_enabled(page + 1 < pages)
                                .on_click(ctx.callback(move |_| action(Change::Page(page + 1))))
                                .content("Next aliases"),
                        )),
                    TextBlock::new().text(format!(
                        "{} checked · {new} new · {} existing · Page {} of {pages}",
                        self.checked.len(),
                        self.checked.len() - new,
                        page + 1,
                    )),
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(8.0)
                        .children((
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .is_enabled(valid)
                                .on_click(ctx.callback(move |_| action(Change::Save)))
                                .content("Save SSH group"),
                            Button::new()
                                .on_click(ctx.callback(move |_| action(Change::Back)))
                                .content("Back to alias preview"),
                        )),
                    if error.is_empty() {
                        View::empty()
                    } else {
                        TextBlock::new()
                            .text(error)
                            .text_wrapping(TextWrapping::Wrap)
                            .into()
                    },
                )),
                ScrollViewer::new().grid_row(1).content(
                    StackPanel::new()
                        .spacing(12.0)
                        .children((optional_lines(&report.warnings), content)),
                ),
            ))
    }
}

fn details(options: &SshOptions) -> String {
    let mut text = endpoint(
        &options.host,
        options.port,
        &options.user,
        &options.authentication,
        options.host_key_alias.as_deref(),
        options.agent_pipe.as_ref(),
    );
    if !options.proxy_jump.is_empty() {
        text.push('\n');
        text.push_str(&jump_summary(&options.proxy_jump));
    }
    for &preset in &options.local_forwards {
        text.push('\n');
        text.push_str(&local_forwards::preset_label(preset));
    }
    text
}

fn optional_lines(lines: &[String]) -> View {
    if lines.is_empty() {
        View::empty()
    } else {
        TextBlock::new()
            .text(lines.join("\n"))
            .text_wrapping(TextWrapping::Wrap)
            .into()
    }
}

impl TerminalApp {
    pub(super) fn edit_config_group(&mut self, id: TabId, revision: u64, change: Change) {
        if self.tabs.active_id() != Some(id) {
            return;
        }
        let Some(Page::ImportConfig(page)) = self.tabs.get_mut(id).map(|tab| &mut tab.page) else {
            return;
        };
        if page.revision != revision {
            return;
        }
        let (Some(editor), Some(report)) = (&mut page.group, &page.report) else {
            return;
        };
        match change {
            Change::Back => {
                page.group = None;
                page.revision += 1;
            }
            Change::Save => {
                let name = editor.name.trim().to_owned();
                let result = editor.entries(report, &self.catalog).and_then(|entries| {
                    self.store
                        .save_imported_ssh_group(&name, &entries, |_| Ok(()))
                });
                match result {
                    Ok(catalog) => {
                        self.catalog = catalog;
                        page.group = None;
                        page.revision += 1;
                        page.status = format!("Saved SSH group {name}");
                    }
                    Err(error) => {
                        editor.error = format!("{error:#}");
                        self.refresh_profiles();
                    }
                }
            }
            _ => editor.edit(change, report, &self.catalog),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(name: &str) -> ImportedAlias {
        ImportedAlias {
            name: name.into(),
            options: Some(
                serde_json::from_value(serde_json::json!({
                    "host": format!("{name}.test"),
                    "user": "ops",
                    "auth": "agent",
                }))
                .unwrap(),
            ),
            warnings: vec![],
        }
    }

    #[test]
    fn filtered_group_review_preserves_selection_and_distinguishes_profile_conflicts() {
        let mut aliases = vec![alias("gateway"), alias("via"), alias("web")];
        aliases.push(ImportedAlias {
            options: None,
            ..alias("blocked")
        });
        let report = ImportReport {
            aliases,
            warnings: vec![],
        };
        let mut catalog = Profiles::default();
        catalog
            .profiles
            .insert("gateway".into(), report.aliases[0].options.clone().unwrap());
        catalog
            .profiles
            .insert("via".into(), alias("other").options.unwrap());
        let mut editor = Editor::default();
        assert_eq!(
            editor.choice(1, &report.aliases[1], &catalog),
            Err("Profile name already exists. Enter a different name.")
        );
        assert_eq!(
            editor.choice(3, &report.aliases[3], &catalog),
            Err("This alias cannot be imported")
        );

        assert_eq!(
            editor.available(editor.shown(&editor.matches(&report)), &report, &catalog),
            vec![(0, Choice::Reuse), (2, Choice::New)]
        );
        editor.edit(Change::Filter("WEB".into()), &report, &catalog);
        editor.edit(
            Change::CheckPage(vec![(0, Choice::Reuse), (2, Choice::New)]),
            &report,
            &catalog,
        );
        assert!(editor.checked.is_empty());
        editor.edit(Change::CheckPage(vec![(2, Choice::New)]), &report, &catalog);
        editor.edit(Change::Filter("gateway".into()), &report, &catalog);
        editor.edit(Change::Check(0, Choice::Reuse), &report, &catalog);
        assert!(matches!(
            editor.entries(&report, &catalog).unwrap().as_slice(),
            [
                ImportedGroupMember::Existing {
                    name: "gateway",
                    ..
                },
                ImportedGroupMember::New { name: "web", .. },
            ]
        ));
    }

    #[test]
    fn changing_a_selected_alias_name_preserves_scope_and_checks_hidden_names() {
        let report = ImportReport {
            aliases: vec![alias("gateway"), alias("via"), alias("web")],
            warnings: vec![],
        };
        let mut catalog = Profiles::default();
        catalog
            .profiles
            .insert("via".into(), alias("different").options.unwrap());
        let mut editor = Editor::default();
        editor.edit(Change::Check(2, Choice::New), &report, &catalog);
        editor.edit(Change::Filter("via".into()), &report, &catalog);
        editor.edit(Change::Profile(1, "web".into()), &report, &catalog);
        editor.edit(Change::Check(1, Choice::New), &report, &catalog);
        assert!(
            editor
                .entries(&report, &catalog)
                .err()
                .unwrap()
                .to_string()
                .contains("Profile web is selected more than once")
        );
        editor.edit(
            Change::Profile(1, "  Via with jump  ".into()),
            &report,
            &catalog,
        );
        assert!(matches!(
            editor.entries(&report, &catalog).unwrap().as_slice(),
            [
                ImportedGroupMember::New {
                    name: "Via with jump",
                    ..
                },
                ImportedGroupMember::New { name: "web", .. },
            ]
        ));
        catalog.profiles.insert(
            "Via with jump".into(),
            report.aliases[1].options.clone().unwrap(),
        );
        assert!(editor.entries(&report, &catalog).is_err());
        editor.edit(Change::Uncheck(1), &report, &catalog);
        assert_eq!(editor.entries(&report, &catalog).unwrap().len(), 1);
    }
}
