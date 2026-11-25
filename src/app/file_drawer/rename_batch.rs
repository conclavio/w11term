use super::*;
use std::collections::HashSet;

const MAX_RENAME_PATH: usize = 4096;

#[derive(Clone, Copy)]
pub enum Field {
    Find,
    Replacement,
}

pub(super) struct Review {
    origin: FolderBatchOrigin,
    hits: Vec<FilenameMatch>,
    find: String,
    replacement: String,
}

impl Review {
    pub(super) fn new(origin: FolderBatchOrigin, hits: Vec<FilenameMatch>) -> Self {
        Self {
            origin,
            hits,
            find: String::new(),
            replacement: String::new(),
        }
    }

    fn edits(&self) -> Result<Vec<(FilenameMatch, String)>> {
        ensure!(
            !self.find.is_empty(),
            "Enter the text to find in marked names"
        );
        ensure!(
            self.find.len() <= MAX_RENAME_PATH && self.replacement.len() <= MAX_RENAME_PATH,
            "Keep the find and replacement text under 4096 bytes"
        );
        let sources = self
            .hits
            .iter()
            .map(|hit| entry_path(&hit.parent, &hit.name))
            .collect::<Result<HashSet<_>>>()?;
        let mut destinations = HashSet::new();
        let mut directories = HashSet::new();
        let mut edits = Vec::new();
        for hit in &self.hits {
            let replacements = hit.name.matches(&self.find).count();
            if replacements == 0 || self.find == self.replacement {
                continue;
            }
            let kept = hit.name.len() - replacements * self.find.len();
            let name_size =
                kept.saturating_add(replacements.saturating_mul(self.replacement.len()));
            ensure!(
                hit.parent
                    .trim_end_matches('/')
                    .len()
                    .saturating_add(1)
                    .saturating_add(name_size)
                    <= MAX_RENAME_PATH,
                "The new path for {} would exceed 4096 bytes",
                hit.name
            );
            let name = hit.name.replace(&self.find, &self.replacement);
            ensure!(
                matches!(hit.kind, Some(EntryKind::File | EntryKind::Directory)),
                "The server did not report a regular file or folder type for {}",
                hit.name
            );
            let destination = entry_path(&hit.parent, &name)?;
            ensure!(
                !sources.contains(&destination) && destinations.insert(destination.clone()),
                "More than one marked entry would use {destination}"
            );
            if hit.kind == Some(EntryKind::Directory) {
                directories.insert(entry_path(&hit.parent, &hit.name)?);
            }
            edits.push((hit.clone(), name));
        }
        for hit in &self.hits {
            let mut ancestor = hit.parent.as_str();
            while !ancestor.is_empty() {
                ensure!(
                    !directories.contains(ancestor),
                    "Unmark entries inside {ancestor} before renaming that folder"
                );
                ancestor = ancestor.rsplit_once('/').map_or("", |(parent, _)| parent);
            }
        }
        ensure!(!edits.is_empty(), "No marked names would change");
        Ok(edits)
    }

    pub(super) fn dialog(
        &self,
        entry: &EntryDialog,
        pane: u64,
        generation: u64,
        ssh: &SshOptions,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        let serial = entry.serial;
        let input = |field, label, value: &str| {
            TextBox::new()
                .automation_name(label)
                .placeholder_text(label)
                .text(value)
                .on_text_changed(context.callback(move |value| {
                    Message::Files(FileAction::RenameMarkedText(
                        pane, generation, serial, field, value,
                    ))
                }))
        };
        let edits = self.edits();
        let preview: View = match &edits {
            Ok(edits) => {
                let rows = edits
                    .iter()
                    .map(|(hit, name)| match self.origin {
                        FolderBatchOrigin::Listing(_) => format!("{} → {name}", hit.name),
                        FolderBatchOrigin::Search(_) => {
                            let parent = hit.parent.trim_end_matches('/');
                            format!("{parent}/{} → {parent}/{name}", hit.name)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                ScrollViewer::new()
                    .max_height(200.0)
                    .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                    .content(
                        TextBlock::new()
                            .automation_id("sftp-rename-preview")
                            .text(rows)
                            .text_wrapping(TextWrapping::Wrap),
                    )
            }
            Err(error) => TextBlock::new()
                .text(error.to_string())
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        };
        let parent = match self.origin {
            FolderBatchOrigin::Listing(_) => {
                format!(" · {}", entry.parent.as_deref().unwrap_or("/"))
            }
            FolderBatchOrigin::Search(_) => String::new(),
        };
        ContentDialog::new()
            .title("Rename marked entries")
            .primary_button_text("Rename")
            .is_primary_button_enabled(edits.is_ok())
            .close_button_text("Cancel")
            .is_open(true)
            .on_closed(context.callback(move |result| {
                Message::Files(if result == ContentDialogResult::Primary {
                    FileAction::ConfirmRenameMarked(pane, generation, serial)
                } else {
                    FileAction::FinishEdit(pane, generation, serial, result)
                })
            }))
            .content(
                StackPanel::new().spacing(8.0).min_width(320.0).children((
                    TextBlock::new()
                        .text(format!("{}@{}:{}{parent}", ssh.user, ssh.host, ssh.port))
                        .text_wrapping(TextWrapping::Wrap),
                    input(Field::Find, "Find text in names", &self.find),
                    input(Field::Replacement, "Replace with", &self.replacement),
                    preview,
                )),
            )
    }
}

impl FileDrawer {
    pub(super) fn edit_rename_marked(&mut self, serial: u64, field: Field, value: String) {
        if let Some(EntryDialog {
            action: EditAction::RenameMarked(review),
            ..
        }) = self.dialog.as_mut().filter(|entry| entry.serial == serial)
        {
            match field {
                Field::Find => review.find = value,
                Field::Replacement => review.replacement = value,
            }
        }
    }

    pub(super) fn confirm_rename_marked(
        &mut self,
        pane: u64,
        session: &Session,
        serial: u64,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        if !self.dialog.as_ref().is_some_and(|entry| {
            entry.serial == serial && matches!(entry.action, EditAction::RenameMarked(_))
        }) {
            return Ok(());
        }
        let Some(EntryDialog {
            parent,
            action: EditAction::RenameMarked(review),
            ..
        }) = self.dialog.take()
        else {
            unreachable!()
        };
        ensure!(
            self.open && !self.busy(),
            "Wait for the active SFTP operation to finish"
        );
        let (root, hits) = self.marked_folder_sources(review.origin)?;
        ensure!(
            parent.as_ref() == Some(&root) && hits == review.hits,
            "The marked entries changed. Review the rename again"
        );
        let edits = review.edits()?;
        self.status = format!("Checking {} destination names…", edits.len());
        let (batch, operation) = folder_batch::Batch::for_rename(serial, review.origin, edits)?;
        self.job(
            pane,
            session,
            operation,
            Step::RenamePreflight(batch),
            context,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(parent: &str, name: &str, kind: EntryKind) -> FilenameMatch {
        FilenameMatch {
            parent: parent.into(),
            name: name.into(),
            kind: Some(kind),
            size: None,
            mtime: None,
            content_match: None,
        }
    }

    #[test]
    fn preview_applies_a_literal_replacement_scoped_to_each_folder() {
        let mut review = Review::new(
            FolderBatchOrigin::Search(5),
            vec![
                hit("/work", "draft-$draft.txt", EntryKind::File),
                hit("/other", "draft-$draft.txt", EntryKind::File),
                hit("/work", "DRAFT.txt", EntryKind::File),
            ],
        );
        review.find = "draft".into();
        review.replacement = "sent 雪".into();
        let edits = review.edits().unwrap();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].1, "sent 雪-$sent 雪.txt");
        assert_eq!(edits[1].1, "sent 雪-$sent 雪.txt");
        review.replacement = "../invalid".into();
        assert!(review.edits().is_err());
    }

    #[test]
    fn preview_refuses_duplicate_targets_and_marked_descendants() {
        let mut review = Review::new(
            FolderBatchOrigin::Search(5),
            vec![
                hit("/work", "xxy", EntryKind::File),
                hit("/work", "xyx", EntryKind::File),
            ],
        );
        review.find = "x".into();
        assert!(review.edits().unwrap_err().to_string().contains("/work/y"));
        review.hits = vec![
            hit("/work", "x-assets", EntryKind::Directory),
            hit("/work/x-assets", "child", EntryKind::File),
        ];
        assert!(
            review
                .edits()
                .unwrap_err()
                .to_string()
                .contains("Unmark entries inside /work/x-assets")
        );
    }
}
