use super::*;
use std::collections::{HashMap, HashSet};
use w11term::sftp::{
    DELETE_OUTCOME_UNKNOWN, MOVE_OUTCOME_UNKNOWN, RENAME_OUTCOME_UNKNOWN, ReviewedRename,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    Listing(u64),
    Search(u64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Move,
    Copy,
    Rename,
    Delete,
}

impl Kind {
    pub(super) fn action(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::Copy => "copy",
            Self::Rename => "rename",
            Self::Delete => "delete",
        }
    }

    pub(super) fn subject(self, count: usize) -> &'static str {
        match (self, count) {
            (Self::Move | Self::Rename | Self::Delete, 1) => "entry",
            (Self::Move | Self::Rename | Self::Delete, _) => "entries",
            (Self::Copy, 1) => "file",
            (Self::Copy, _) => "files",
        }
    }

    fn ongoing(self) -> &'static str {
        match self {
            Self::Move => "moving",
            Self::Copy => "copying",
            Self::Rename => "renaming",
            Self::Delete => "deleting",
        }
    }
}

pub(super) struct ListingRefresh {
    pub(super) directory: String,
    pub(super) note: String,
}

enum Plan {
    Move(String),
    Copy(String),
    Delete,
    Rename {
        names: HashMap<String, String>,
        preflight_cancel: TransferCancel,
    },
}

impl Plan {
    fn kind(&self) -> Kind {
        match self {
            Self::Move(_) => Kind::Move,
            Self::Copy(_) => Kind::Copy,
            Self::Delete => Kind::Delete,
            Self::Rename { .. } => Kind::Rename,
        }
    }

    fn update_directory(&mut self, reply: &Value) {
        if let (Self::Move(directory) | Self::Copy(directory), Some(resolved)) =
            (self, reply["directory"].as_str())
        {
            *directory = resolved.to_owned();
        }
    }
}

pub(super) struct Batch {
    id: u64,
    plan: Plan,
    origin: Origin,
    current: FilenameMatch,
    remaining: VecDeque<FilenameMatch>,
    confirmed: usize,
    total: usize,
    stop_requested: bool,
}

fn validate_sources(kind: Kind, hits: &[FilenameMatch]) -> Result<()> {
    ensure!(!hits.is_empty(), "Mark at least one entry");
    let mut names = HashSet::new();
    let mut directories = HashSet::new();
    for hit in hits {
        let path = if kind == Kind::Delete {
            delete_path(&hit.parent, &hit.name)?
        } else {
            entry_path(&hit.parent, &hit.name)?
        };
        match kind {
            Kind::Move | Kind::Rename => ensure!(
                matches!(hit.kind, Some(EntryKind::File | EntryKind::Directory)),
                "Mark files or folders to move"
            ),
            Kind::Copy => ensure!(
                hit.kind == Some(EntryKind::File),
                "Mark regular files to copy"
            ),
            Kind::Delete => ensure!(
                matches!(hit.kind, Some(EntryKind::File | EntryKind::Directory)),
                "Mark regular files or empty folders to delete"
            ),
        }
        if kind == Kind::Delete {
            ensure!(names.insert(path.clone()), "Duplicate marked path {path}");
        } else {
            ensure!(
                names.insert(hit.name.clone()),
                "Marked entries share the name {}. They need separate destinations.",
                hit.name
            );
        }
        if hit.kind == Some(EntryKind::Directory) {
            directories.insert(path);
        }
    }
    for hit in hits {
        let mut ancestor = hit.parent.as_str();
        while !ancestor.is_empty() {
            ensure!(
                !directories.contains(ancestor),
                "Unmark entries inside {ancestor} before {} that folder",
                kind.ongoing()
            );
            ancestor = ancestor.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }
    Ok(())
}

impl Batch {
    pub(super) fn new(
        id: u64,
        kind: Kind,
        origin: Origin,
        hits: Vec<FilenameMatch>,
        destination: String,
    ) -> Result<Self> {
        let plan = match kind {
            Kind::Move => Plan::Move(destination),
            Kind::Copy => Plan::Copy(destination),
            Kind::Rename => bail!("Choose the new names before starting a rename"),
            Kind::Delete => Plan::Delete,
        };
        Self::with_plan(id, origin, hits, plan)
    }

    pub(super) fn for_rename(
        id: u64,
        origin: Origin,
        edits: Vec<(FilenameMatch, String)>,
    ) -> Result<(Self, Operation)> {
        let mut hits = Vec::new();
        let mut names = HashMap::new();
        let mut entries = Vec::new();
        for (hit, name) in edits {
            let from = entry_path(&hit.parent, &hit.name)?;
            let to = entry_path(&hit.parent, &name)?;
            let kind = hit.kind.context("Missing type for marked rename")?;
            names.insert(from.clone(), name);
            entries.push(ReviewedRename { from, to, kind });
            hits.push(hit);
        }
        let batch = Self::with_plan(
            id,
            origin,
            hits,
            Plan::Rename {
                names,
                preflight_cancel: TransferCancel::default(),
            },
        )?;
        Ok((batch, Operation::CheckRenameBatch { entries }))
    }

    fn with_plan(id: u64, origin: Origin, hits: Vec<FilenameMatch>, plan: Plan) -> Result<Self> {
        let total = hits.len();
        let mut hits = hits.into_iter();
        let current = hits.next().context("Mark at least one entry")?;
        Ok(Self {
            id,
            plan,
            origin,
            current,
            remaining: hits.collect(),
            confirmed: 0,
            total,
            stop_requested: false,
        })
    }

    fn request_stop(&mut self) {
        if !self.remaining.is_empty() {
            self.remaining.clear();
            self.stop_requested = true;
        }
    }

    fn completed(&self) -> String {
        let summary = self.summary();
        if self.stop_requested {
            format!(
                "{summary}. Stopped after the current {}",
                self.plan.kind().action()
            )
        } else {
            summary
        }
    }

    fn summary(&self) -> String {
        match &self.plan {
            Plan::Move(destination) => format!(
                "Moved {} of {} marked entries to {destination}",
                self.confirmed, self.total
            ),
            Plan::Copy(destination) => format!(
                "Copied {} of {} marked {} to {destination}",
                self.confirmed,
                self.total,
                Kind::Copy.subject(self.total)
            ),
            Plan::Rename { .. } => format!(
                "Renamed {} of {} marked {}",
                self.confirmed,
                self.total,
                Kind::Rename.subject(self.total)
            ),
            Plan::Delete => format!(
                "Deleted {} of {} marked {}",
                self.confirmed,
                self.total,
                Kind::Delete.subject(self.total)
            ),
        }
    }

    pub(super) fn reconnect_status(
        &mut self,
        reply: &mut oneshot::Receiver<Result<Value, String>>,
    ) -> String {
        use oneshot::error::TryRecvError;
        match reply.try_recv() {
            Ok(Ok(value)) => {
                self.confirmed += 1;
                self.plan.update_directory(&value);
                format!(
                    "{} before SSH reconnected. Refresh to browse.",
                    self.summary()
                )
            }
            Ok(Err(error)) => format!("{}. {error}", self.summary()),
            Err(TryRecvError::Closed) => {
                format!(
                    "{}. SSH reconnected before the next {} started.",
                    self.summary(),
                    self.plan.kind().action()
                )
            }
            Err(TryRecvError::Empty) => {
                let advice = match self.plan.kind() {
                    Kind::Move => MOVE_OUTCOME_UNKNOWN,
                    Kind::Copy => "Copy outcome unknown; refresh the destination folder",
                    Kind::Rename => RENAME_OUTCOME_UNKNOWN,
                    Kind::Delete => DELETE_OUTCOME_UNKNOWN,
                };
                format!(
                    "{}. SSH reconnected while {} {}. {advice}.",
                    self.summary(),
                    self.plan.kind().ongoing(),
                    self.current.name,
                )
            }
        }
    }

    pub(super) fn preflight_cancel(&self) -> Option<TransferCancel> {
        match &self.plan {
            Plan::Rename {
                preflight_cancel, ..
            } => Some(preflight_cancel.clone()),
            _ => None,
        }
    }
}

impl FileDrawer {
    pub(super) fn stop_folder_batch_button(
        &self,
        pane: u64,
        origin: Origin,
        context: &ViewContext<TerminalApp>,
    ) -> Option<View> {
        let (batch, checking) = match &self.pending.as_ref()?.step {
            Step::FolderBatch(batch) => (batch, false),
            Step::RenamePreflight(batch) => (batch, true),
            _ => return None,
        };
        if batch.origin != origin
            || (!checking && batch.remaining.is_empty() && !batch.stop_requested)
        {
            return None;
        }
        let generation = self.generation;
        let id = batch.id;
        Some(
            Button::new()
                .is_enabled(!batch.stop_requested)
                .on_click(context.callback(move |_| {
                    Message::Files(FileAction::StopFolderBatch(pane, generation, id))
                }))
                .content(if checking {
                    "Cancel rename check"
                } else {
                    match (batch.plan.kind(), batch.stop_requested) {
                        (Kind::Move, true) => "Stopping after current move…",
                        (Kind::Move, false) => "Stop after current move",
                        (Kind::Copy, true) => "Stopping after current copy…",
                        (Kind::Copy, false) => "Stop after current copy",
                        (Kind::Rename, true) => "Stopping after current rename…",
                        (Kind::Rename, false) => "Stop after current rename",
                        (Kind::Delete, true) => "Stopping after current delete…",
                        (Kind::Delete, false) => "Stop after current delete",
                    }
                }),
        )
    }

    pub(super) fn stop_folder_batch(&mut self, id: u64) {
        if let Some(Step::RenamePreflight(batch)) =
            self.pending.as_ref().map(|pending| &pending.step)
            && batch.id == id
        {
            if let Some(cancel) = batch.preflight_cancel() {
                cancel.cancel();
            }
            self.pending.take();
            self.status = "Rename check cancelled. No entries were renamed.".into();
            return;
        }
        if let Some(Pending {
            step: Step::FolderBatch(batch),
            ..
        }) = self.pending.as_mut()
            && batch.id == id
        {
            batch.request_stop();
        }
    }

    pub(super) fn marked_folder_sources(
        &self,
        origin: Origin,
    ) -> Result<(String, Vec<FilenameMatch>)> {
        match origin {
            Origin::Search(serial) => self.marked_search_sources(serial),
            Origin::Listing(revision) => {
                ensure!(
                    self.open && self.listing_revision == revision,
                    "The remote listing changed. Mark the files again."
                );
                let root = self.directory.clone().context("Missing remote folder")?;
                let hits = self
                    .marked_listing_entries()?
                    .into_iter()
                    .map(|entry| FilenameMatch {
                        parent: root.clone(),
                        name: entry.name.clone(),
                        kind: match entry.kind {
                            ListedKind::Known(kind) => Some(kind),
                            _ => None,
                        },
                        mtime: entry.mtime,
                        size: entry.size,
                        content_match: None,
                    })
                    .collect();
                Ok((root, hits))
            }
        }
    }

    pub(super) fn begin_folder_batch(&mut self, kind: Kind, origin: Origin) -> Result<()> {
        ensure!(!self.busy(), "Wait for the active SFTP operation to finish");
        let (root, hits) = self.marked_folder_sources(origin)?;
        if kind == Kind::Rename {
            self.end_filter_edit();
            self.show_entry_dialog(
                Some(root),
                EditAction::RenameMarked(rename_batch::Review::new(origin, hits)),
            );
            return Ok(());
        }
        validate_sources(kind, &hits)?;
        self.end_filter_edit();
        self.show_entry_dialog(Some(root), EditAction::FolderBatch(kind, origin, hits));
        Ok(())
    }

    pub(super) fn start_folder_batch(
        &mut self,
        pane: u64,
        session: &Session,
        batch: Batch,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        let from = entry_path(&batch.current.parent, &batch.current.name)?;
        let destination_is_canonical = batch.confirmed != 0;
        let operation = match &batch.plan {
            Plan::Move(directory) => Operation::MoveEntry {
                from: from.clone(),
                directory: directory.clone(),
                kind: batch.current.kind,
                destination_is_canonical,
            },
            Plan::Copy(directory) => Operation::CopyEntry {
                from: from.clone(),
                destination: CopyDestination::Folder {
                    directory: directory.clone(),
                    is_canonical: destination_is_canonical,
                },
            },
            Plan::Rename { names, .. } => Operation::RenameEntry {
                from: from.clone(),
                to: entry_path(
                    &batch.current.parent,
                    names.get(&from).context("Missing reviewed rename target")?,
                )?,
                search_kind: batch.current.kind,
            },
            Plan::Delete => Operation::RemoveEntry {
                path: delete_path(&batch.current.parent, &batch.current.name)?,
                kind: batch.current.kind.context("Missing reviewed entry type")?,
                check_parent: true,
            },
        };
        let (activity, subject) = match batch.plan.kind() {
            Kind::Move => ("Moving", "entry"),
            Kind::Copy => ("Copying", "file"),
            Kind::Rename => ("Renaming", "entry"),
            Kind::Delete => ("Deleting", "entry"),
        };
        self.status = format!(
            "{activity} marked {subject} {} of {}…",
            batch.confirmed + 1,
            batch.total,
        );
        let summary = batch.summary();
        let kind = batch.plan.kind();
        let origin = batch.origin;
        let source = batch.current.parent.clone();
        if let Err(error) = self.job(pane, session, operation, Step::FolderBatch(batch), context) {
            let note = format!(
                "{summary}. Could not start {} {from}. {error:#}",
                kind.ongoing()
            );
            self.refresh_after_folder_batch(pane, session, (kind, origin), source, note, context);
        }
        Ok(())
    }

    pub(super) fn finish_folder_batch(
        &mut self,
        pane: u64,
        session: &Session,
        mut batch: Batch,
        reply: Result<Value, String>,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        let note = match reply {
            Ok(value) => {
                batch.plan.update_directory(&value);
                batch.confirmed += 1;
                match (&batch.plan, batch.origin) {
                    (Plan::Move(directory), Origin::Search(serial)) => self.apply_search_change(
                        serial,
                        &batch.current,
                        &EntryChange::Move {
                            name: batch.current.name.clone(),
                            directory: directory.clone(),
                        },
                    )?,
                    (Plan::Copy(directory), Origin::Search(serial)) => self.apply_search_change(
                        serial,
                        &batch.current,
                        &EntryChange::Copy {
                            name: batch.current.name.clone(),
                            directory: directory.clone(),
                            size: value["bytes_copied"].as_u64(),
                        },
                    )?,
                    (Plan::Rename { names, .. }, Origin::Search(serial)) => {
                        let from = entry_path(&batch.current.parent, &batch.current.name)?;
                        self.apply_search_change(
                            serial,
                            &batch.current,
                            &EntryChange::Rename {
                                from: batch.current.name.clone(),
                                to: names
                                    .get(&from)
                                    .context("Missing reviewed rename target")?
                                    .clone(),
                            },
                        )?;
                    }
                    (Plan::Delete, Origin::Search(serial)) => self.apply_search_change(
                        serial,
                        &batch.current,
                        &EntryChange::Delete(batch.current.name.clone()),
                    )?,
                    (
                        Plan::Move(_) | Plan::Rename { .. } | Plan::Delete,
                        Origin::Listing(revision),
                    ) => self.discard_batch_row(revision, &batch.current),
                    (Plan::Copy(_), Origin::Listing(_)) => {}
                }
                if let Some(next) = batch.remaining.pop_front() {
                    batch.current = next;
                    return self.start_folder_batch(pane, session, batch, context);
                }
                batch.completed()
            }
            Err(error) => {
                let unknown = match batch.plan.kind() {
                    Kind::Move => error.contains(MOVE_OUTCOME_UNKNOWN),
                    Kind::Rename => error.contains(RENAME_OUTCOME_UNKNOWN),
                    Kind::Delete => error.contains(DELETE_OUTCOME_UNKNOWN),
                    Kind::Copy => false,
                };
                if unknown {
                    match batch.origin {
                        Origin::Search(serial) => {
                            self.invalidate_search_entry(serial, &batch.current)?;
                            // The worker may have changed the entry before losing the reply.
                            if let Some(directory) = self.directory.clone() {
                                self.accept_listing(None, directory, Vec::new());
                            }
                        }
                        Origin::Listing(revision) => {
                            self.discard_batch_row(revision, &batch.current)
                        }
                    }
                }
                let from = entry_path(&batch.current.parent, &batch.current.name)?;
                format!("{}. Stopped at {from}. {error}", batch.summary())
            }
        };
        self.refresh_after_folder_batch(
            pane,
            session,
            (batch.plan.kind(), batch.origin),
            batch.current.parent,
            note,
            context,
        );
        Ok(())
    }

    fn discard_batch_row(&mut self, revision: u64, hit: &FilenameMatch) {
        if self.listing_revision == revision
            && self.directory.as_deref() == Some(hit.parent.as_str())
        {
            self.entries.retain(|entry| entry.name != hit.name);
            if let Some(marked) = &mut self.marked_downloads {
                marked.remove(&hit.name);
            }
            self.update_matches();
            self.set_page(self.page);
        }
    }

    pub(super) fn retained_move_marks(&self, entries: &[Entry]) -> Option<BTreeSet<String>> {
        let marked = self.marked_downloads.as_ref()?;
        let types: std::collections::HashMap<_, _> = self
            .entries
            .iter()
            .filter(|entry| marked.contains(&entry.name))
            .map(|entry| (&entry.name, entry.kind))
            .collect();
        Some(
            entries
                .iter()
                .filter(|entry| types.get(&entry.name) == Some(&entry.kind))
                .map(|entry| entry.name.clone())
                .collect(),
        )
    }

    fn refresh_after_folder_batch(
        &mut self,
        pane: u64,
        session: &Session,
        batch: (Kind, Origin),
        source: String,
        note: String,
        context: &ComponentContext<TerminalApp>,
    ) {
        let revision = match batch {
            (Kind::Move | Kind::Rename | Kind::Delete, Origin::Listing(revision)) => revision,
            (Kind::Copy, Origin::Listing(_)) => {
                self.status = note;
                return;
            }
            (_, Origin::Search(serial)) => {
                self.refresh_changed_search(pane, session, serial, note, context);
                return;
            }
        };
        self.status = note.clone();
        if self.listing_revision != revision || self.directory.as_ref() != Some(&source) {
            return;
        }
        let listing = self.job(
            pane,
            session,
            Operation::Ls {
                path: source.clone(),
            },
            Step::Listing(source.clone(), None),
            context,
        );
        match listing {
            Ok(()) => {
                self.move_listing_refresh = Some(ListingRefresh {
                    directory: source,
                    note,
                })
            }
            Err(error) => self
                .status
                .push_str(&format!(". Directory refresh failed: {error:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hit(parent: &str, name: &str, kind: EntryKind) -> FilenameMatch {
        FilenameMatch {
            parent: parent.into(),
            name: name.into(),
            kind: Some(kind),
            mtime: None,
            size: None,
            content_match: None,
        }
    }

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: name.into(),
            kind: ListedKind::Known(kind),
            mtime: None,
            size: None,
        }
    }

    #[test]
    fn delete_review_distinguishes_equal_leaf_names_and_rejects_ambiguous_paths() {
        let first = hit("/project/one", "old.log", EntryKind::File);
        let second = hit("/project/two", "old.log", EntryKind::File);
        assert!(validate_sources(Kind::Delete, &[first.clone(), second]).is_ok());
        assert!(validate_sources(Kind::Delete, &[first.clone(), first.clone()]).is_err());
        assert!(
            validate_sources(
                Kind::Delete,
                &[hit("/project/../other", "old.log", EntryKind::File)],
            )
            .is_err()
        );
        assert!(
            validate_sources(
                Kind::Delete,
                &[hit("/project", "one", EntryKind::Directory), first],
            )
            .is_err()
        );
    }

    #[test]
    fn delete_confirmation_requires_unchanged_marks() {
        let mut drawer = FileDrawer {
            open: true,
            directory: Some("/work".into()),
            listing_revision: 1,
            entries: vec![
                entry("first", EntryKind::File),
                entry("second", EntryKind::File),
            ],
            marked_downloads: Some(BTreeSet::from(["first".into(), "second".into()])),
            ..Default::default()
        };
        drawer
            .begin_folder_batch(Kind::Delete, Origin::Listing(1))
            .unwrap();
        let serial = drawer.dialog.as_ref().unwrap().serial;
        assert!(!drawer.dialog.as_ref().unwrap().action.uses_folder_picker());
        drawer.marked_downloads.as_mut().unwrap().remove("second");
        assert!(
            drawer
                .finish_edit(serial, ContentDialogResult::Primary)
                .is_err()
        );

        drawer
            .begin_folder_batch(Kind::Delete, Origin::Listing(1))
            .unwrap();
        let serial = drawer.dialog.as_ref().unwrap().serial;
        let (review, _) = drawer
            .finish_edit(serial, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert!(
            matches!(review.action, EditAction::FolderBatch(Kind::Delete, _, hits)
            if hits.len() == 1 && hits[0].name == "first")
        );
    }

    #[test]
    fn listing_review_uses_remote_names_and_requires_the_same_marks_on_confirmation() {
        let mut drawer = FileDrawer {
            open: true,
            directory: Some("/work".into()),
            listing_revision: 7,
            entries: vec![
                entry("CON", EntryKind::File),
                entry("other", EntryKind::File),
            ],
            marked_downloads: Some(BTreeSet::from(["CON".into(), "other".into()])),
            ..Default::default()
        };
        assert!(drawer.marked_entries().is_err()); // Not a Windows download name.
        assert!(
            drawer
                .begin_folder_batch(Kind::Move, Origin::Listing(6))
                .is_err()
        );
        drawer
            .begin_folder_batch(Kind::Move, Origin::Listing(7))
            .unwrap();
        let review = drawer.dialog.as_mut().unwrap();
        let serial = review.serial;
        review.name = "/archive".into();
        drawer.marked_downloads.as_mut().unwrap().remove("other");
        assert!(
            drawer
                .finish_edit(serial, ContentDialogResult::Primary)
                .is_err()
        );
        drawer
            .begin_folder_batch(Kind::Move, Origin::Listing(7))
            .unwrap();
        let review = drawer.dialog.as_mut().unwrap();
        let serial = review.serial;
        review.name = "/archive".into();
        let (review, _) = drawer
            .finish_edit(serial, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert!(
            matches!(review.action, EditAction::FolderBatch(Kind::Move, _, hits) if hits.len() == 1 && hits[0].name == "CON")
        );
    }

    #[test]
    fn source_refresh_preserves_only_untouched_marks_with_the_same_type() {
        let mut drawer = FileDrawer {
            open: true,
            directory: Some("/work".into()),
            listing_revision: 7,
            entries: vec![
                entry("moved", EntryKind::File),
                entry("untouched", EntryKind::File),
                entry("changed", EntryKind::File),
            ],
            marked_downloads: Some(BTreeSet::from([
                "moved".into(),
                "untouched".into(),
                "changed".into(),
            ])),
            ..Default::default()
        };
        drawer.discard_batch_row(7, &hit("/work", "moved", EntryKind::File));
        drawer.move_listing_refresh = Some(ListingRefresh {
            directory: "/work".into(),
            note: "Moved 1 of 3 marked entries to /archive".into(),
        });
        drawer.accept_listing(
            None,
            "/work".into(),
            vec![
                entry("moved", EntryKind::File), // Another writer reused the name.
                entry("untouched", EntryKind::File),
                entry("changed", EntryKind::Directory),
            ],
        );
        assert_eq!(
            drawer.marked_downloads,
            Some(BTreeSet::from(["untouched".into()]))
        );
        assert_eq!(drawer.status, "Moved 1 of 3 marked entries to /archive");
        assert_eq!(drawer.listing_revision, 8);
    }

    #[test]
    fn reviews_reject_colliding_names_and_selected_descendants() {
        let directory = hit("/work", "assets", EntryKind::Directory);
        let unrelated = hit("/work/assets-old", "icon.svg", EntryKind::File);
        assert!(validate_sources(Kind::Move, &[directory.clone(), unrelated.clone()]).is_ok());
        let inside = hit("/work/assets/nested", "icon.svg", EntryKind::File);
        assert!(
            validate_sources(Kind::Move, &[inside.clone(), directory])
                .unwrap_err()
                .to_string()
                .contains("/work/assets")
        );
        assert!(
            validate_sources(Kind::Move, &[unrelated, inside])
                .unwrap_err()
                .to_string()
                .contains("share the name icon.svg")
        );
    }

    #[test]
    fn copy_review_rejects_folders_and_duplicate_basenames() {
        let first = hit("/work", "report", EntryKind::File);
        let other = hit("/work/subdir", "other", EntryKind::File);
        assert!(validate_sources(Kind::Copy, &[first.clone(), other]).is_ok());
        let same_name = hit("/work/subdir", "report", EntryKind::File);
        assert!(
            validate_sources(Kind::Copy, &[first.clone(), same_name])
                .unwrap_err()
                .to_string()
                .contains("share the name report")
        );
        let directory = hit("/work", "archive", EntryKind::Directory);
        assert!(
            validate_sources(Kind::Copy, &[first, directory])
                .unwrap_err()
                .to_string()
                .contains("Mark regular files")
        );
    }

    #[test]
    fn reconnection_distinguishes_the_confirmed_prefix_from_the_in_flight_move() {
        let hits = vec![
            hit("/work", "one", EntryKind::File),
            hit("/work", "two", EntryKind::File),
        ];
        let mut batch = Batch::new(
            5,
            Kind::Move,
            Origin::Search(4),
            hits.clone(),
            "/target".into(),
        )
        .unwrap();
        let (send, mut receive) = oneshot::channel();
        assert!(
            batch
                .reconnect_status(&mut receive)
                .contains(MOVE_OUTCOME_UNKNOWN)
        );
        assert_eq!(batch.confirmed, 0);
        send.send(Ok(json!({"directory":"/canonical"}))).unwrap();
        assert_eq!(
            batch.reconnect_status(&mut receive),
            "Moved 1 of 2 marked entries to /canonical before SSH reconnected. Refresh to browse."
        );
        let mut batch =
            Batch::new(5, Kind::Move, Origin::Search(4), hits, "/target".into()).unwrap();
        let (send, mut receive) = oneshot::channel();
        send.send(Err("Move was not started".into())).unwrap();
        assert_eq!(
            batch.reconnect_status(&mut receive),
            "Moved 0 of 2 marked entries to /target. Move was not started"
        );
    }

    #[test]
    fn stopping_drops_queued_moves_but_preserves_the_current_request() {
        let first = hit("/work", "first", EntryKind::File);
        let second = hit("/work", "second", EntryKind::File);
        let mut batch = Batch::new(
            5,
            Kind::Move,
            Origin::Search(4),
            vec![first.clone(), second],
            "/target".into(),
        )
        .unwrap();
        batch.request_stop();
        batch.request_stop();
        assert!(batch.stop_requested);
        assert!(batch.remaining.is_empty());
        assert_eq!(batch.current, first);
        assert_eq!(batch.confirmed, 0);
        batch.confirmed += 1;
        assert_eq!(
            batch.completed(),
            "Moved 1 of 2 marked entries to /target. Stopped after the current move"
        );

        let mut last = Batch::new(
            6,
            Kind::Move,
            Origin::Listing(4),
            vec![first],
            "/target".into(),
        )
        .unwrap();
        last.request_stop();
        assert!(!last.stop_requested);
    }
}
