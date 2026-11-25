use super::*;
use std::collections::HashSet;
use w11term::sftp::MOVE_OUTCOME_UNKNOWN;

pub(super) struct Batch {
    serial: u64,
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
    pub(super) fn new(serial: u64, hits: Vec<FilenameMatch>, destination: String) -> Result<Self> {
        let total = hits.len();
        let mut hits = hits.into_iter();
        let current = hits.next().context("Mark at least one search result")?;
        Ok(Self {
            serial,
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
        serial: u64,
        context: &ViewContext<TerminalApp>,
    ) -> Option<View> {
        let Step::MoveSearchBatch(batch) = &self.pending.as_ref()?.step else {
            return None;
        };
        if batch.serial != serial || (batch.remaining.is_empty() && !batch.stop_requested) {
            return None;
        }
        let generation = self.generation;
        Some(
            Button::new()
                .is_enabled(!batch.stop_requested)
                .on_click(context.callback(move |_| {
                    Message::Files(FileAction::Search(
                        pane,
                        generation,
                        find::Action::StopMoves(serial),
                    ))
                }))
                .content(if batch.stop_requested {
                    "Stopping after current move…"
                } else {
                    "Stop after current move"
                }),
        )
    }

    pub(super) fn stop_move_batch(&mut self, serial: u64) {
        if let Some(Pending {
            step: Step::MoveSearchBatch(batch),
            ..
        }) = self.pending.as_mut()
            && batch.serial == serial
        {
            batch.request_stop();
        }
    }

    pub(super) fn begin_marked_move(&mut self, serial: u64) -> Result<()> {
        let (root, hits) = self.marked_search_moves(serial)?;
        validate_sources(&hits)?;
        self.show_entry_dialog(Some(root), EditAction::MoveSearchBatch(serial, hits));
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
        let serial = batch.serial;
        let summary = batch.summary();
        if let Err(error) = self.job(
            pane,
            session,
            operation,
            Step::MoveSearchBatch(batch),
            context,
        ) {
            self.refresh_changed_search(
                pane,
                session,
                serial,
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
                self.apply_search_change(batch.serial, &batch.current, &change)?;
                if let Some(next) = batch.remaining.pop_front() {
                    batch.current = next;
                    return self.start_move_batch(pane, session, batch, context);
                }
                batch.completed()
            }
            Err(error) => {
                if error.contains(MOVE_OUTCOME_UNKNOWN) {
                    self.invalidate_search_entry(batch.serial, &batch.current)?;
                    // The worker could have moved this entry before it lost the reply.
                    // Leave the cached listing empty until it is visited again.
                    if let Some(directory) = self.directory.clone() {
                        self.accept_listing(None, directory, Vec::new());
                    }
                }
                let from = entry_path(&batch.current.parent, &batch.current.name)?;
                format!("{}. Stopped at {from}. {error}", batch.summary())
            }
        };
        self.refresh_changed_search(pane, session, batch.serial, note, context);
        Ok(())
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
        let mut batch = Batch::new(5, hits.clone(), "/target".into()).unwrap();
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
        let mut batch = Batch::new(5, hits, "/target".into()).unwrap();
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
        let mut batch = Batch::new(5, vec![first.clone(), second], "/target".into()).unwrap();
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

        let mut last = Batch::new(6, vec![first], "/target".into()).unwrap();
        last.request_stop();
        assert!(!last.stop_requested);
    }
}
