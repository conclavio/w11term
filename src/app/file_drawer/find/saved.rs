use super::*;
use std::collections::BTreeMap;
use w11term::profiles::{BookmarkPeer, SavedSftpSearch};

#[derive(Clone)]
pub enum Action {
    Show,
    Close,
    Name(String),
    Select(String),
    Save,
    Run(String),
    Remove(String),
}

pub(super) struct List {
    pub(super) profile: String,
    entries: BTreeMap<String, SavedSftpSearch>,
    selection: Option<String>,
    name: String,
}

impl List {
    pub(super) fn view(
        &self,
        panel: &Panel,
        pane: u64,
        generation: u64,
        ready: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let serial = panel.serial;
        let dispatch = move |action| {
            Message::Files(FileAction::Search(
                pane,
                generation,
                super::Action::Saved(serial, action),
            ))
        };
        let button = |label: &str, action: Action, enabled| {
            Button::new()
                .automation_name(label)
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| dispatch(action.clone())))
                .content(label)
        };
        let name = TextBox::new()
            .grid_column(0)
            .min_width(0.0)
            .automation_name("Saved search name")
            .placeholder_text("Saved search name")
            .is_enabled(ready)
            .text(&self.name)
            .on_text_changed(ctx.callback(move |name| dispatch(Action::Name(name))));
        let save = Border::new().grid_column(1).content(button(
            "Save current search",
            Action::Save,
            ready && !self.name.trim().is_empty() && panel.can_search(),
        ));
        let mut rows = vec![
            KeyedView::new("hide", button("Hide saved searches", Action::Close, true)),
            KeyedView::new(
                "editor",
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto])
                    .column_spacing(6.0)
                    .children((name, save)),
            ),
        ];
        let names: Vec<_> = self.entries.keys().cloned().collect();
        if names.is_empty() {
            rows.push(KeyedView::new(
                "empty",
                TextBlock::new().text("No saved searches"),
            ));
        } else {
            let selected_index = self
                .selection
                .as_ref()
                .and_then(|name| names.iter().position(|item| item == name));
            rows.push(KeyedView::new(
                "picker",
                ComboBox::new()
                    .automation_name("Saved SFTP searches")
                    .is_enabled(ready)
                    .items_source(names.clone())
                    .selected_index(selected_index)
                    .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                        index
                            .and_then(|i| names.get(i))
                            .map_or(Message::ControlIdle, |name| {
                                dispatch(Action::Select(name.clone()))
                            })
                    })),
            ));
        }
        if let Some((name, search)) = self
            .selection
            .as_ref()
            .and_then(|name| self.entries.get_key_value(name))
        {
            let kind = RESULT_TYPES
                .iter()
                .find(|(kind, _)| *kind == search.filter.kind)
                .map_or("All types", |(_, label)| *label);
            let sort = RESULT_SORTS
                .iter()
                .find(|(sort, _)| *sort == search.sort)
                .map_or("Sort by folder", |(_, label)| *label);
            let query = if search.query.is_empty() {
                "Any name".into()
            } else {
                let mode = if search.filter.regex {
                    "Regex"
                } else {
                    "Contains"
                };
                let case = if search.filter.match_case {
                    "match case"
                } else {
                    "ignore case"
                };
                format!("{mode} “{}” · {case}", search.query)
            };
            let scope = if search.filter.match_path {
                " · Relative path"
            } else {
                ""
            };
            rows.push(KeyedView::new(
                "root",
                TextBlock::new()
                    .text(&search.root)
                    .text_wrapping(TextWrapping::Wrap),
            ));
            rows.push(KeyedView::new(
                "criteria",
                TextBlock::new()
                    .text(format!("{query}{scope} · {kind} · {sort}"))
                    .text_wrapping(TextWrapping::Wrap),
            ));
            if !search.filter.exclude_dir_regex.is_empty() {
                rows.push(KeyedView::new(
                    "exclusions",
                    TextBlock::new()
                        .text(format!(
                            "Exclude folders “{}”",
                            search.filter.exclude_dir_regex
                        ))
                        .text_wrapping(TextWrapping::Wrap),
                ));
            }
            let size = match (
                search.filter.file_size.minimum,
                search.filter.file_size.maximum,
            ) {
                (Some(min), Some(max)) => Some(format!("{min}–{max} bytes")),
                (Some(min), None) => Some(format!("At least {min} bytes")),
                (None, Some(max)) => Some(format!("At most {max} bytes")),
                (None, None) => None,
            };
            if let Some(size) = size {
                rows.push(KeyedView::new("size", TextBlock::new().text(size)));
            }
            let dates = &search.filter.modified_dates;
            let date_summary = match (dates.from.trim(), dates.through.trim()) {
                ("", "") => None,
                ("", through) => Some(format!("Modified through {through} UTC")),
                (from, "") => Some(format!("Modified from {from} UTC")),
                (from, through) => Some(format!("Modified {from} through {through} UTC")),
            };
            if let Some(summary) = date_summary {
                rows.push(KeyedView::new(
                    "dates",
                    TextBlock::new()
                        .text(summary)
                        .text_wrapping(TextWrapping::Wrap),
                ));
            }
            rows.push(KeyedView::new(
                "actions",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        button("Run saved search", Action::Run(name.clone()), ready),
                        button("Remove saved search", Action::Remove(name.clone()), ready),
                    )),
            ));
        }
        StackPanel::new().spacing(6.0).keyed_children(rows)
    }
}

impl FileDrawer {
    pub(super) fn saved_search_action(
        &mut self,
        pane: u64,
        session: &Session,
        serial: u64,
        action: Action,
        profile: Option<(&str, &ProfileStore)>,
        ctx: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        let busy = self.busy();
        let Some(panel) = self.search.as_mut().filter(|panel| panel.serial == serial) else {
            return Ok(());
        };
        if matches!(action, Action::Close) {
            panel.saved = None;
            return Ok(());
        }
        if busy {
            return Ok(());
        }
        let (profile, store) = profile.context("Saved searches require a saved SSH profile")?;
        if matches!(action, Action::Show) {
            let connection = session
                .ssh_connection_at(self.generation)
                .context("The SSH connection has changed")?;
            let entries =
                store.list_sftp_searches(profile, &BookmarkPeer::from_connection(&connection))?;
            panel.saved = Some(List {
                profile: profile.into(),
                selection: entries.keys().next().cloned(),
                entries,
                name: String::new(),
            });
            return Ok(());
        }
        let Some(list) = panel.saved.as_mut().filter(|list| list.profile == profile) else {
            return Ok(());
        };
        let action = match action {
            Action::Name(name) => {
                list.name = name;
                return Ok(());
            }
            Action::Select(name) => {
                if list.entries.contains_key(&name) {
                    list.selection = Some(name);
                }
                return Ok(());
            }
            action => action,
        };
        let connection = session
            .ssh_connection_at(self.generation)
            .context("The SSH connection has changed")?;
        let peer = BookmarkPeer::from_connection(&connection);
        match action {
            Action::Save => {
                let name = list.name.trim().to_owned();
                let search = SavedSftpSearch {
                    root: panel.root.clone(),
                    query: panel.query.clone(),
                    filter: panel.size.apply(&panel.filter)?,
                    sort: panel.sort,
                };
                list.entries = store.save_sftp_search(profile, &peer, &name, search)?;
                list.selection = Some(name.clone());
                list.name.clear();
                self.status = format!("Saved search {name}");
            }
            Action::Remove(name) => {
                let expected = list
                    .entries
                    .get(&name)
                    .filter(|_| list.selection.as_ref() == Some(&name))
                    .context("Choose the saved search again")?;
                list.entries = store.remove_sftp_search(profile, &peer, &name, expected)?;
                list.selection = list.entries.keys().next().cloned();
                self.status = format!("Removed saved search {name}");
            }
            Action::Run(name) => {
                let expected = list
                    .entries
                    .get(&name)
                    .filter(|_| list.selection.as_ref() == Some(&name))
                    .context("Choose the saved search again")?;
                let entries = store.list_sftp_searches(profile, &peer)?;
                let search = entries
                    .get(&name)
                    .filter(|search| *search == expected)
                    .context("The saved search changed. Reopen saved searches.")?;
                panel.root.clone_from(&search.root);
                panel.query.clone_from(&search.query);
                panel.filter.clone_from(&search.filter);
                panel.size = search.filter.file_size.into();
                panel.sort = search.sort;
                panel.saved = None;
                panel.clear_results();
                self.start_filename_search(pane, session, ctx, None);
            }
            _ => unreachable!("handled when opening or editing saved searches"),
        }
        Ok(())
    }
}
