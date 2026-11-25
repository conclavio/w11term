use super::*;
use std::collections::HashSet;
use w11term::sftp::MOVE_OUTCOME_UNKNOWN;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    Listing(u64),
    Search(u64),
}

pub(super) struct ListingRefresh {
    pub(super) directory: String,
    pub(super) note: String,
}

pub(super) struct Batch {
    id: u64,
    origin: Origin,
    destination: String,
    current: FilenameMatch,
    remaining: VecDeque<FilenameMatch>,
    confirmed: usize,
    total: usize,
    stop_requested: bool,
}

fn validate_sources(hits: &[FilenameMatch]) -> Result<()> {
    ensure!(!hits.is_empty(), "Mark at least one search result");
    let mut names = HashSet::new();
    let mut directories = HashSet::new();
    for hit in hits {
        let path = entry_path(&hit.parent, &hit.name)?;
        ensure!(
            matches!(hit.kind, Some(EntryKind::File | EntryKind::Directory)),
            "Mark files or folders to move"
        );
        ensure!(
            names.insert(hit.name.as_str()),
            "Marked entries share the name {}. They need separate destinations.",
            hit.name
        );
        if hit.kind == Some(EntryKind::Directory) {
            directories.insert(path);
        }
    }
    for hit in hits {
        let mut ancestor = hit.parent.as_str();
        while !ancestor.is_empty() {
            ensure!(
                !directories.contains(ancestor),
                "Unmark entries inside {ancestor} before moving that folder"
            );
            ancestor = ancestor.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
    }
    Ok(())
}

impl Batch {
    pub(super) fn new(
        id: u64,
        origin: Origin,
        hits: Vec<FilenameMatch>,
        destination: String,
    ) -> Result<Self> {
        let total = hits.len();
        let mut hits = hits.into_iter();
        let current = hits.next().context("Mark at least one search result")?;
        Ok(Self {
            id,
            origin,
            destination,
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
            format!("{summary}. Stopped after the current move")
        } else {
            summary
        }
    }

    fn summary(&self) -> String {
        format!(
            "Moved {} of {} marked entries to {}",
            self.confirmed, self.total, self.destination,
        )
    }

    pub(super) fn reconnect_status(
        &mut self,
        reply: &mut oneshot::Receiver<Result<Value, String>>,
    ) -> String {
        use oneshot::error::TryRecvError;
        match reply.try_recv() {
            Ok(Ok(value)) => {
                self.confirmed += 1;
                if let Some(directory) = value["directory"].as_str() {
                    self.destination = directory.into();
                }
                format!(
                    "{} before SSH reconnected. Refresh to browse.",
                    self.summary()
                )
            }
            Ok(Err(error)) => format!("{}. {error}", self.summary()),
            Err(TryRecvError::Closed) => {
                format!(
                    "{}. SSH reconnected before the next move started.",
                    self.summary()
                )
            }
            Err(TryRecvError::Empty) => format!(
                "{}. SSH reconnected while moving {}. {MOVE_OUTCOME_UNKNOWN}.",
                self.summary(),
                self.current.name,
            ),
        }
    }
}

impl FileDrawer {
    pub(super) fn stop_move_batch_button(
        &self,
        pane: u64,
        origin: Origin,
        context: &ViewContext<TerminalApp>,
    ) -> Option<View> {
        let Step::MoveBatch(batch) = &self.pending.as_ref()?.step else {
            return None;
        };
        if batch.origin != origin || (batch.remaining.is_empty() && !batch.stop_requested) {
            return None;
        }
        let generation = self.generation;
        let id = batch.id;
        Some(
            Button::new()
                .is_enabled(!batch.stop_requested)
                .on_click(
                    context.callback(move |_| {
                        Message::Files(FileAction::StopMoves(pane, generation, id))
                    }),
                )
                .content(if batch.stop_requested {
                    "Stopping after current move…"
                } else {
                    "Stop after current move"
                }),
        )
    }

    pub(super) fn stop_move_batch(&mut self, id: u64) {
        if let Some(Pending {
            step: Step::MoveBatch(batch),
            ..
        }) = self.pending.as_mut()
            && batch.id == id
        {
            batch.request_stop();
        }
    }

    pub(super) fn marked_moves(&self, origin: Origin) -> Result<(String, Vec<FilenameMatch>)> {
        match origin {
            Origin::Search(serial) => self.marked_search_moves(serial),
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

    pub(super) fn begin_marked_move(&mut self, origin: Origin) -> Result<()> {
        ensure!(!self.busy(), "Wait for the active SFTP operation to finish");
        let (root, hits) = self.marked_moves(origin)?;
        validate_sources(&hits)?;
        self.end_filter_edit();
        self.show_entry_dialog(Some(root), EditAction::MoveBatch(origin, hits));
        Ok(())
    }

    pub(super) fn start_move_batch(
        &mut self,
        pane: u64,
        session: &Session,
        batch: Batch,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        let from = entry_path(&batch.current.parent, &batch.current.name)?;
        let operation = Operation::MoveEntry {
            from: from.clone(),
            directory: batch.destination.clone(),
            kind: batch.current.kind,
            destination_is_canonical: batch.confirmed != 0,
        };
        self.status = format!(
            "Moving marked entry {} of {}…",
            batch.confirmed + 1,
            batch.total,
        );
        let origin = batch.origin;
        let source = batch.current.parent.clone();
        let summary = batch.summary();
        if let Err(error) = self.job(pane, session, operation, Step::MoveBatch(batch), context) {
            self.refresh_after_move_batch(
                pane,
                session,
                origin,
                source,
                format!("{summary}. Could not start moving {from}. {error:#}"),
                context,
            );
        }
        Ok(())
    }

    pub(super) fn finish_move_batch(
        &mut self,
        pane: u64,
        session: &Session,
        mut batch: Batch,
        reply: Result<Value, String>,
        context: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        let note = match reply {
            Ok(value) => {
                if let Some(directory) = value["directory"].as_str() {
                    batch.destination = directory.into();
                }
                batch.confirmed += 1;
                let change = EntryChange::Move {
                    name: batch.current.name.clone(),
                    directory: batch.destination.clone(),
                };
                match batch.origin {
                    Origin::Search(serial) => {
                        self.apply_search_change(serial, &batch.current, &change)?
                    }
                    Origin::Listing(revision) => self.discard_moved_row(revision, &batch.current),
                }
                if let Some(next) = batch.remaining.pop_front() {
                    batch.current = next;
                    return self.start_move_batch(pane, session, batch, context);
                }
                batch.completed()
            }
            Err(error) => {
                if error.contains(MOVE_OUTCOME_UNKNOWN) {
                    match batch.origin {
                        Origin::Search(serial) => {
                            self.invalidate_search_entry(serial, &batch.current)?;
                            // The worker could have moved this entry before it lost the reply.
                            if let Some(directory) = self.directory.clone() {
                                self.accept_listing(None, directory, Vec::new());
                            }
                        }
                        Origin::Listing(revision) => {
                            self.discard_moved_row(revision, &batch.current)
                        }
                    }
                }
                let from = entry_path(&batch.current.parent, &batch.current.name)?;
                format!("{}. Stopped at {from}. {error}", batch.summary())
            }
        };
        self.refresh_after_move_batch(
            pane,
            session,
            batch.origin,
            batch.current.parent,
            note,
            context,
        );
        Ok(())
    }

    fn discard_moved_row(&mut self, revision: u64, hit: &FilenameMatch) {
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

    fn refresh_after_move_batch(
        &mut self,
        pane: u64,
        session: &Session,
        origin: Origin,
        source: String,
        note: String,
        context: &ComponentContext<TerminalApp>,
    ) {
        let revision = match origin {
            Origin::Listing(revision) => revision,
            Origin::Search(serial) => {
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
        assert!(drawer.begin_marked_move(Origin::Listing(6)).is_err());
        drawer.begin_marked_move(Origin::Listing(7)).unwrap();
        let review = drawer.dialog.as_mut().unwrap();
        let serial = review.serial;
        review.name = "/archive".into();
        drawer.marked_downloads.as_mut().unwrap().remove("other");
        assert!(
            drawer
                .finish_edit(serial, ContentDialogResult::Primary)
                .is_err()
        );
        drawer.begin_marked_move(Origin::Listing(7)).unwrap();
        let review = drawer.dialog.as_mut().unwrap();
        let serial = review.serial;
        review.name = "/archive".into();
        let (review, _) = drawer
            .finish_edit(serial, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert!(
            matches!(review.action, EditAction::MoveBatch(_, hits) if hits.len() == 1 && hits[0].name == "CON")
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
        drawer.discard_moved_row(7, &hit("/work", "moved", EntryKind::File));
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
        assert!(validate_sources(&[directory.clone(), unrelated.clone()]).is_ok());
        let inside = hit("/work/assets/nested", "icon.svg", EntryKind::File);
        assert!(
            validate_sources(&[inside.clone(), directory])
                .unwrap_err()
                .to_string()
                .contains("/work/assets")
        );
        assert!(
            validate_sources(&[unrelated, inside])
                .unwrap_err()
                .to_string()
                .contains("share the name icon.svg")
        );
    }

    #[test]
    fn reconnection_distinguishes_the_confirmed_prefix_from_the_in_flight_move() {
        let hits = vec![
            hit("/work", "one", EntryKind::File),
            hit("/work", "two", EntryKind::File),
        ];
        let mut batch = Batch::new(5, Origin::Search(4), hits.clone(), "/target".into()).unwrap();
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
        let mut batch = Batch::new(5, Origin::Search(4), hits, "/target".into()).unwrap();
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

        let mut last = Batch::new(6, Origin::Listing(4), vec![first], "/target".into()).unwrap();
        last.request_stop();
        assert!(!last.stop_requested);
    }
}
