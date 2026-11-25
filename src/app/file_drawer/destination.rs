use super::*;
use serde::Deserialize;

const PAGE_SIZE: usize = 24;

#[derive(Clone)]
pub enum Action {
    Show,
    Back,
    Open(u64, String),
    Up(u64),
    Root(u64),
    Page(u64, usize),
    Choose(u64),
}

#[derive(Deserialize)]
struct Folders {
    directory: String,
    folders: Vec<String>,
}

#[derive(Default)]
pub(super) struct Picker {
    revision: u64,
    loading: Option<String>,
    folders: Option<Folders>,
    error: Option<String>,
    page: usize,
    pub(super) cancel: TransferCancel,
}

impl Drop for Picker {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Picker {
    fn ready(&self, revision: u64) -> bool {
        self.revision == revision && self.loading.is_none()
    }

    fn pages(&self) -> usize {
        self.folders
            .as_ref()
            .map_or(0, |listing| listing.folders.len().div_ceil(PAGE_SIZE))
    }

    fn can_choose(&self, destination: &str, entry: &EntryDialog) -> bool {
        let EditAction::Move(target, kind) = &entry.action else {
            return false;
        };
        let source = target.parent().trim_end_matches('/');
        if destination.trim_end_matches('/') == source {
            return false;
        }
        if *kind != Some(EntryKind::Directory) {
            return true;
        }
        let path = format!("{source}/{}", target.name());
        destination != path
            && !destination
                .strip_prefix(&path)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    pub(super) fn dialog(
        &self,
        entry: &EntryDialog,
        pane: u64,
        generation: u64,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let serial = entry.serial;
        let revision = self.revision;
        let button = |name: &str, action: Action, enabled| {
            Button::new()
                .automation_name(name)
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| {
                    Message::Files(FileAction::MoveDestination(
                        pane,
                        generation,
                        serial,
                        action.clone(),
                    ))
                }))
        };
        let text = |value: &str| {
            TextBlock::new()
                .text(value)
                .text_wrapping(TextWrapping::Wrap)
        };
        let directory = self
            .folders
            .as_ref()
            .map(|listing| listing.directory.as_str());
        let ready = self.loading.is_none();
        let mut fields = vec![KeyedView::new(
            "navigation",
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    button(
                        "Parent folder",
                        Action::Up(revision),
                        ready && directory.is_some_and(|path| path != "/"),
                    )
                    .content("Parent folder"),
                    button(
                        "Server root",
                        Action::Root(revision),
                        ready && directory != Some("/"),
                    )
                    .content("Server root"),
                )),
        )];
        if let Some(path) = &self.loading {
            fields.push(KeyedView::new(
                "loading",
                text(&format!("Loading folders in {path}…")),
            ));
        } else {
            if let Some(path) = directory {
                fields.push(KeyedView::new("current", text(path)));
            }
            if let Some(error) = &self.error {
                fields.push(KeyedView::new(
                    "error",
                    text(error).automation_name("Destination browsing error"),
                ));
            }
            if let Some(listing) = &self.folders {
                if listing.folders.is_empty() {
                    fields.push(KeyedView::new("empty", text("No subfolders")));
                } else {
                    let rows = listing
                        .folders
                        .iter()
                        .skip(self.page * PAGE_SIZE)
                        .take(PAGE_SIZE)
                        .map(|name| {
                            KeyedView::new(
                                name.as_str(),
                                button(
                                    &format!("Browse folder {name}"),
                                    Action::Open(revision, name.clone()),
                                    true,
                                )
                                .content(text(name)),
                            )
                        });
                    fields.push(KeyedView::new(
                        format!("folders-{revision}-{}", self.page),
                        ScrollViewer::new()
                            .max_height(260.0)
                            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                            .content(StackPanel::new().spacing(4.0).keyed_children(rows)),
                    ));
                }
                if self.pages() > 1 {
                    fields.push(KeyedView::new(
                        "pages",
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(6.0)
                            .children((
                                button(
                                    "Previous folders",
                                    Action::Page(revision, self.page.saturating_sub(1)),
                                    self.page > 0,
                                )
                                .content("Previous"),
                                text(&format!("{} / {}", self.page + 1, self.pages())),
                                button(
                                    "Next folders",
                                    Action::Page(revision, self.page + 1),
                                    self.page + 1 < self.pages(),
                                )
                                .content("Next"),
                            )),
                    ));
                }
            }
        }
        fields.push(KeyedView::new(
            "choose",
            button(
                "Use this folder",
                Action::Choose(revision),
                ready && directory.is_some_and(|path| self.can_choose(path, entry)),
            )
            .content("Use this folder"),
        ));
        fields.push(KeyedView::new(
            "back",
            button("Back to destination", Action::Back, true).content("Back to destination"),
        ));
        ContentDialog::new()
            .title("Choose destination folder")
            .close_button_text("Cancel")
            .is_open(true)
            .on_closed(ctx.callback(move |result| {
                Message::Files(FileAction::FinishEdit(pane, generation, serial, result))
            }))
            .content(
                StackPanel::new()
                    .min_width(300.0)
                    .spacing(8.0)
                    .keyed_children(fields),
            )
    }
}

impl FileDrawer {
    pub(super) fn move_destination(
        &mut self,
        pane: u64,
        serial: u64,
        action: Action,
        session: &Session,
        ctx: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        let Some(entry) = self
            .dialog
            .as_mut()
            .filter(|entry| entry.serial == serial && matches!(entry.action, EditAction::Move(..)))
        else {
            return Ok(());
        };
        let path = match action {
            Action::Show if entry.destination_picker.is_none() && self.pending.is_none() => {
                let path = entry
                    .destination()
                    .or_else(|_| entry.parent.clone().context("Missing source folder"))?;
                entry.destination_picker = Some(Picker::default());
                path
            }
            Action::Back => {
                entry.destination_picker.take();
                self.stop_destination_listing();
                return Ok(());
            }
            Action::Choose(revision) => {
                if let Some(picker) = entry
                    .destination_picker
                    .as_ref()
                    .filter(|p| p.ready(revision))
                    && let Some(listing) = &picker.folders
                    && picker.can_choose(&listing.directory, entry)
                {
                    entry.name = listing.directory.clone();
                    entry.destination_picker.take();
                }
                return Ok(());
            }
            Action::Page(revision, page) => {
                if let Some(picker) = &mut entry.destination_picker
                    && picker.ready(revision)
                    && page < picker.pages()
                {
                    picker.page = page;
                }
                return Ok(());
            }
            Action::Root(revision) if self.pending.is_none() => {
                if !entry
                    .destination_picker
                    .as_ref()
                    .is_some_and(|p| p.ready(revision))
                {
                    return Ok(());
                }
                "/".into()
            }
            Action::Open(revision, _) | Action::Up(revision) if self.pending.is_none() => {
                let Some(listing) = entry
                    .destination_picker
                    .as_ref()
                    .filter(|p| p.ready(revision))
                    .and_then(|p| p.folders.as_ref())
                else {
                    return Ok(());
                };
                match action {
                    Action::Open(_, name) if listing.folders.binary_search(&name).is_ok() => {
                        entry_path(&listing.directory, &name)?
                    }
                    Action::Up(_) if listing.directory != "/" => {
                        let (parent, _) = listing.directory.rsplit_once('/').unwrap();
                        if parent.is_empty() {
                            "/".into()
                        } else {
                            parent.into()
                        }
                    }
                    _ => return Ok(()),
                }
            }
            _ => return Ok(()),
        };
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        let revision = self.dialog_serial;
        let picker = entry.destination_picker.as_mut().unwrap();
        picker.revision = revision;
        picker.loading = Some(path.clone());
        picker.error = None;
        if let Err(error) = self.job(
            pane,
            session,
            Operation::BrowseMoveFolders { path },
            Step::MoveDestination(serial, revision),
            ctx,
        ) {
            self.finish_destination_listing(serial, revision, Err(format!("{error:#}")));
        }
        Ok(())
    }

    pub(super) fn finish_destination_listing(
        &mut self,
        serial: u64,
        revision: u64,
        reply: Result<Value, String>,
    ) {
        let Some(picker) = self
            .dialog
            .as_mut()
            .filter(|entry| entry.serial == serial)
            .and_then(|entry| entry.destination_picker.as_mut())
            .filter(|picker| picker.revision == revision)
        else {
            return;
        };
        let Some(path) = picker.loading.take() else {
            return;
        };
        match reply.and_then(|value| serde_json::from_value(value).map_err(|e| e.to_string())) {
            Ok(folders) => {
                picker.folders = Some(folders);
                picker.page = 0;
            }
            Err(error) => picker.error = Some(format!("Could not open {path}. {error}")),
        }
    }

    pub(super) fn stop_destination_listing(&mut self) {
        if matches!(
            self.pending.as_ref().map(|p| &p.step),
            Some(Step::MoveDestination(..))
        ) {
            self.pending.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn destination_replies_are_bound_to_the_current_dialog_and_request() {
        let mut files = FileDrawer::default();
        files.show_entry_dialog(
            Some("/work".into()),
            EditAction::Move(
                FileTarget::Listed(ListedEntry {
                    parent: "/work".into(),
                    name: "project".into(),
                    kind: ListedKind::Known(EntryKind::Directory),
                    revision: 1,
                }),
                Some(EntryKind::Directory),
            ),
        );
        let entry = files.dialog.as_mut().unwrap();
        let serial = entry.serial;
        let cancel = TransferCancel::default();
        entry.destination_picker = Some(Picker {
            revision: 7,
            loading: Some("/work".into()),
            folders: None,
            error: None,
            page: 0,
            cancel: cancel.clone(),
        });
        let listing = json!({"directory":"/work","folders":["project"]});
        files.finish_destination_listing(serial + 1, 7, Ok(listing.clone()));
        files.finish_destination_listing(serial, 6, Ok(listing.clone()));
        let current = |files: &FileDrawer| {
            files
                .dialog
                .as_ref()
                .unwrap()
                .destination_picker
                .as_ref()
                .unwrap()
                .loading
                .is_some()
        };
        assert!(current(&files));
        files.finish_destination_listing(serial, 7, Ok(listing));
        assert!(!current(&files));
        let entry = files.dialog.as_ref().unwrap();
        let picker = entry.destination_picker.as_ref().unwrap();
        assert!(!picker.can_choose("/work", entry));
        assert!(!picker.can_choose("/work/project/child", entry));
        assert!(picker.can_choose("/work/project-old", entry));

        let result = files
            .finish_edit(serial, ContentDialogResult::Primary)
            .unwrap();
        assert!(result.is_none());
        assert!(!cancel.is_cancelled());
        files.cancel_entry_dialog();
        assert!(cancel.is_cancelled());
        assert!(files.dialog.is_none());
    }
}
