use super::*;
use w11term::workspace::WorkspaceEntry;

pub enum Action {
    Create(String),
    Review(String),
    Replace,
    Cancel,
}

pub(super) struct Replacement {
    name: String,
    saved: WorkspaceEntry,
    proposed: Workspace,
}

impl TerminalApp {
    pub(super) fn save_workspace(&mut self, id: TabId, action: Action) {
        match action {
            Action::Create(name) => self.prepare_workspace_save(id, name, false),
            Action::Review(name) => self.prepare_workspace_save(id, name, true),
            Action::Replace => {
                if let Some(replacement) = self
                    .workspace_page_mut(id)
                    .and_then(|page| page.replacement.take())
                {
                    self.persist_workspace(
                        id,
                        &replacement.name,
                        &replacement.proposed,
                        Some(&replacement.saved),
                    );
                }
            }
            Action::Cancel => {
                if let Some(page) = self.workspace_page_mut(id) {
                    page.replacement = None;
                }
            }
        }
    }

    fn prepare_workspace_save(&mut self, id: TabId, name: String, review_existing: bool) {
        if !self.workspace_page_mut(id).is_some_and(|page| {
            !page.switching
                && page.startup.is_none()
                && page.editor.is_none()
                && page.replacement.is_none()
                && page.name.trim() == name
        }) {
            return;
        }
        let result = self.capture_workspace().and_then(|layout| {
            let saved = review_existing
                .then(|| WorkspaceStore::open()?.entry(&name))
                .transpose()?;
            Ok((layout, saved))
        });
        match result {
            Ok((proposed, Some(saved))) => {
                if let Some(page) = self.workspace_page_mut(id) {
                    page.status.clear();
                    page.replacement = Some(Replacement {
                        name,
                        saved,
                        proposed,
                    });
                }
            }
            Ok((layout, None)) => self.persist_workspace(id, &name, &layout, None),
            Err(error) => {
                if let Some(page) = self.workspace_page_mut(id) {
                    page.reload_names();
                    page.status = format!("{error:#}");
                }
            }
        }
    }

    fn persist_workspace(
        &mut self,
        id: TabId,
        name: &str,
        layout: &Workspace,
        expected: Option<&WorkspaceEntry>,
    ) {
        let result = crate::workspace_window::save(
            name,
            self.store.path(),
            layout,
            expected,
            &self._control,
            self.registry.window_id(),
        );
        if let Some(page) = self.workspace_page_mut(id) {
            page.status = match result {
                Ok(()) => {
                    page.insert_saved_name(name);
                    if expected.is_some() {
                        format!("Updated {name}")
                    } else {
                        format!("Saved {name}")
                    }
                }
                Err(error) => {
                    page.reload_names();
                    format!("{error:#}")
                }
            };
        }
    }
}

pub(super) fn view(
    id: TabId,
    replacement: &Replacement,
    context: &ViewContext<TerminalApp>,
) -> View {
    ScrollViewer::new().content(
        StackPanel::new()
            .margin(24.0)
            .max_width(570.0)
            .spacing(14.0)
            .horizontal_alignment(HorizontalAlignment::Left)
            .children((
                TextBlock::new()
                    .text(format!("Replace saved workspace {}?", replacement.name))
                    .font_size(24.0)
                    .text_wrapping(TextWrapping::Wrap),
                TextBlock::new()
                    .text("This updates the layout used on the next open. Live sessions keep running.")
                    .text_wrapping(TextWrapping::Wrap),
                layout_summary("Saved layout", &replacement.saved.layout),
                layout_summary("Replacement layout", &replacement.proposed),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(8.0)
                    .children((
                        Button::new()
                            .on_click(context.callback(move |_| {
                                Message::Workspace(WorkspaceAction::Save(id, Action::Cancel))
                            }))
                            .content("Cancel"),
                        Button::new()
                            .on_click(context.callback(move |_| {
                                Message::Workspace(WorkspaceAction::Save(id, Action::Replace))
                            }))
                            .content("Replace saved workspace"),
                    )),
            )),
    )
}

fn layout_summary(label: &str, workspace: &Workspace) -> View {
    fn quantity(count: usize, unit: &str) -> String {
        format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
    }
    let mut panes = 0;
    let rows: Vec<_> = workspace
        .tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| {
            let count = tab.layout.pane_count();
            panes += count;
            let title = tab
                .title
                .as_ref()
                .map(|title| title.as_str())
                .or_else(|| first_leaf(&tab.layout).source.profile_name())
                .map_or_else(|| format!("Tab {}", index + 1), str::to_owned);
            format!("{title} · {}", quantity(count, "pane"))
        })
        .collect();
    StackPanel::new().spacing(4.0).children((
        TextBlock::new()
            .text(format!(
                "{label} · {}, {}",
                quantity(workspace.tabs.len(), "tab"),
                quantity(panes, "pane")
            ))
            .font_size(16.0),
        TextBlock::new()
            .text(rows.join("\n"))
            .text_wrapping(TextWrapping::Wrap),
    ))
}
