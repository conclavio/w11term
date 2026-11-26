use super::*;
use anyhow::ensure;
use std::path::Path;
use w11term::{profiles::ProfileStore, workspace::MissingProfile};

pub(super) struct Draft {
    missing: Vec<MissingProfile>,
    profiles: BTreeMap<String, SshOptions>,
    selected: BTreeMap<String, String>,
}

impl Draft {
    pub(super) fn load(layout: &Workspace, path: &Path) -> anyhow::Result<Option<Self>> {
        let profiles = ProfileStore::new(path.to_owned())?.load()?;
        let missing = layout.missing_profiles(&profiles)?;
        Ok((!missing.is_empty()).then_some(Self {
            missing,
            profiles,
            selected: BTreeMap::new(),
        }))
    }

    pub(super) fn select(&mut self, name: String, index: Option<usize>) {
        let replacement = self
            .missing
            .iter()
            .find(|row| row.name == name)
            .and_then(|row| index.and_then(|i| row.replacements.get(i)));
        if let Some(replacement) = replacement {
            self.selected.insert(name, replacement.clone());
        } else {
            self.selected.remove(&name);
        }
    }

    pub(super) fn apply(
        &self,
        workspace: &str,
        expected: &WorkspaceEntry,
        path: &Path,
    ) -> anyhow::Result<usize> {
        let profiles = ProfileStore::new(path.to_owned())?.load()?;
        for name in self.selected.values() {
            ensure!(
                profiles.get(name) == self.profiles.get(name),
                "SSH profile {name} changed. Reopen the repair form to review the current profile."
            );
        }
        WorkspaceStore::open()?.repair_profiles(workspace, expected, &self.selected, &profiles)?;
        Ok(self.missing.len() - self.selected.len())
    }

    pub(super) fn view(
        &self,
        id: TabId,
        workspace: &str,
        status: &str,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        let mut rows = vec![KeyedView::new(
            "heading",
            TextBlock::new()
                .font_size(24.0)
                .text_wrapping(TextWrapping::Wrap)
                .text(format!("Repair SSH profiles in {workspace}")),
        )];
        for row in &self.missing {
            let missing = row.name.clone();
            let noun = if row.panes == 1 { "pane" } else { "panes" };
            let picker: View = if row.replacements.is_empty() {
                TextBlock::new()
                    .text("No compatible saved SSH profiles.")
                    .into()
            } else {
                let labels = row.replacements.iter().map(|name| {
                    let options = &self.profiles[name];
                    let host = if options.host.contains(':') {
                        format!("[{}]", options.host)
                    } else {
                        options.host.clone()
                    };
                    format!("{name} · {}@{host}:{}", options.user, options.port)
                });
                let original = missing.clone();
                let selected = self
                    .selected
                    .get(&missing)
                    .and_then(|name| row.replacements.iter().position(|entry| entry == name));
                ComboBox::new()
                    .automation_name(format!("Replacement for {missing}"))
                    .placeholder_text("Choose a saved SSH profile")
                    .max_width(505.0)
                    .items_source(labels)
                    .selected_index(selected)
                    .on_selection_changed(context.callback(move |index| {
                        Message::Workspace(WorkspaceAction::Manage(
                            id,
                            Action::SelectReplacement(original.clone(), index),
                        ))
                    }))
                    .into()
            };
            rows.push(KeyedView::new(
                format!("missing-{missing}"),
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new()
                        .text(format!("{missing} · {} {noun}", row.panes))
                        .text_wrapping(TextWrapping::Wrap),
                    picker,
                )),
            ));
        }
        rows.push(KeyedView::new(
            "actions",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    Button::new()
                        .on_click(context.callback(move |_| {
                            Message::Workspace(WorkspaceAction::Manage(id, Action::Cancel))
                        }))
                        .content("Cancel"),
                    Button::new()
                        .is_enabled(!self.selected.is_empty())
                        .on_click(context.callback(move |_| {
                            Message::Workspace(WorkspaceAction::Manage(id, Action::ApplyRepair))
                        }))
                        .content("Save replacements"),
                )),
        ));
        rows.push(KeyedView::new(
            "status",
            TextBlock::new()
                .text(status)
                .text_wrapping(TextWrapping::Wrap),
        ));
        ScrollViewer::new().content(
            StackPanel::new()
                .margin(24.0)
                .max_width(550.0)
                .spacing(12.0)
                .horizontal_alignment(HorizontalAlignment::Left)
                .keyed_children(rows),
        )
    }
}
