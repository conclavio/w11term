use super::*;
use anyhow::Context;
use w11term::{options::SshOptions, profiles::ImportedGroupMember};

const PAGE_SIZE: usize = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    New,
    Reuse,
}

pub enum Change {
    Name(String),
    Filter(String),
    Page(usize),
    Check(usize, Choice, bool),
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
    error: String,
}

fn choice(alias: &ImportedAlias, catalog: &Profiles) -> Result<Choice, &'static str> {
    let Some(options) = &alias.options else {
        return Err("This alias cannot be imported");
    };
    if catalog.ssh_host_group_member(&alias.name) == Some(options) {
        Ok(Choice::Reuse)
    } else if catalog.profiles.contains_key(&alias.name) || catalog.local.contains_key(&alias.name)
    {
        Err("Conflicts with a saved profile")
    } else {
        Ok(Choice::New)
    }
}

impl Editor {
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
            .filter_map(|&index| Some((index, choice(report.aliases.get(index)?, catalog).ok()?)))
            .collect()
    }

    fn entries<'a>(
        &self,
        report: &'a ImportReport,
    ) -> anyhow::Result<Vec<ImportedGroupMember<'a>>> {
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
                Ok(match selected {
                    Choice::New => ImportedGroupMember::New {
                        name: &alias.name,
                        options,
                    },
                    Choice::Reuse => ImportedGroupMember::Existing {
                        name: &alias.name,
                        reviewed: options,
                    },
                })
            })
            .collect()
    }

    fn edit(&mut self, change: Change, report: &ImportReport, catalog: &Profiles) {
        match change {
            Change::Name(name) => self.name = name,
            Change::Filter(query) if query != self.query => {
                self.query = query;
                self.page = 0;
            }
            Change::Page(page) if page < self.matches(report).len().div_ceil(PAGE_SIZE) => {
                self.page = page
            }
            Change::Check(index, selected, true)
                if self.shown(&self.matches(report)).contains(&index)
                    && report
                        .aliases
                        .get(index)
                        .is_some_and(|alias| choice(alias, catalog) == Ok(selected)) =>
            {
                self.checked.insert(index, selected);
            }
            Change::Check(index, _, false)
                if self.shown(&self.matches(report)).contains(&index) =>
            {
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
                    let (state, check) = match choice(alias, catalog) {
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
                        .is_enabled(check.is_some())
                        .is_checked(self.checked.contains_key(&index))
                        .on_is_checked_changed(ctx.callback(move |checked| {
                            check.map_or(Message::ControlIdle, |choice| {
                                action(Change::Check(index, choice, checked))
                            })
                        }))
                        .content(alias.name.as_str());
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
                                .is_enabled(
                                    !self.name.trim().is_empty() && !self.checked.is_empty(),
                                )
                                .on_click(ctx.callback(move |_| action(Change::Save)))
                                .content("Save SSH group"),
                            Button::new()
                                .on_click(ctx.callback(move |_| action(Change::Back)))
                                .content("Back to alias preview"),
                        )),
                    if self.error.is_empty() {
                        View::empty()
                    } else {
                        TextBlock::new()
                            .text(&self.error)
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
                let result = editor.entries(report).and_then(|entries| {
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
                    Err(error) => editor.error = format!("{error:#}"),
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
        assert_eq!(
            choice(&report.aliases[1], &catalog),
            Err("Conflicts with a saved profile")
        );
        assert_eq!(
            choice(&report.aliases[3], &catalog),
            Err("This alias cannot be imported")
        );

        let mut editor = Editor::default();
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
        editor.edit(Change::Check(0, Choice::Reuse, true), &report, &catalog);
        assert!(matches!(
            editor.entries(&report).unwrap().as_slice(),
            [
                ImportedGroupMember::Existing {
                    name: "gateway",
                    ..
                },
                ImportedGroupMember::New { name: "web", .. },
            ]
        ));
    }
}
