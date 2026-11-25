use super::*;
use w11term::sftp::{DropSource, classify_drop};

#[derive(Clone)]
pub struct DropTarget {
    pub(super) pane: u64,
    generation: u64,
    parent: String,
    revision: u64,
    drawer_serial: u64,
    overwrite_files: bool,
    atomic_files: bool,
}

impl DropTarget {
    pub(super) fn new(pane: u64, files: &FileDrawer, parent: &str) -> Self {
        Self {
            pane,
            generation: files.generation,
            parent: parent.into(),
            revision: files.listing_revision,
            drawer_serial: files.dialog_serial,
            overwrite_files: files.replace,
            atomic_files: files.atomic_uploads,
        }
    }

    pub(super) fn data(&self, data: DroppedData) -> FileAction {
        let paths = match data {
            DroppedData::StorageItems(items) => items
                .into_iter()
                .map(|item| PathBuf::from(item.path))
                .collect(),
            _ => Vec::new(),
        };
        FileAction::Drop(self.clone(), paths)
    }

    fn current(&self, drawer: &FileDrawer) -> bool {
        drawer.open
            && drawer.bookmarks.is_none()
            && drawer.search.is_none()
            && self.generation != 0
            && self.generation == drawer.generation
            && drawer.directory.as_ref() == Some(&self.parent)
            && self.revision == drawer.listing_revision
            && self.drawer_serial == drawer.dialog_serial
    }

    #[cfg(feature = "ui-smoke")]
    pub(super) fn fixture(&self, file: &std::path::Path) -> Result<FileAction> {
        let paths: Vec<String> = serde_json::from_slice(&std::fs::read(file)?)?;
        let items = paths
            .into_iter()
            .map(|path| windows_reactor::DroppedStorageItem {
                name: PathBuf::from(&path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_owned(),
                path,
            })
            .collect();
        Ok(self.data(DroppedData::StorageItems(items)))
    }
}

pub(super) struct DropPreparation {
    pub(super) id: TransferId,
    cancel: TransferCancel,
}

impl Drop for DropPreparation {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl TransferQueue {
    pub(super) fn files(
        paths: Vec<PathBuf>,
        parent: &str,
        overwrite: bool,
        atomic: bool,
        id: TransferId,
    ) -> Result<Self> {
        let mut queue = Self {
            overwrite_files: overwrite,
            batch_id: Some(id),
            total: paths.len(),
            ..Self::default()
        };
        for local in paths {
            ensure!(
                local.is_absolute(),
                "Select a local file with an absolute path"
            );
            let name = basename(&local)?;
            let remote = remote_child(parent, &name)?;
            queue.remaining.push_back((
                Operation::Put {
                    local,
                    remote: remote.clone(),
                    overwrite,
                    atomic,
                },
                TransferFile::Upload {
                    name,
                    remote,
                    atomic,
                },
            ));
        }
        Ok(queue)
    }

    pub(super) fn dropped(
        target: &DropTarget,
        token: TransferId,
        sources: Vec<DropSource>,
    ) -> Result<Self> {
        ensure!(
            !sources.is_empty(),
            "Drop local files or folders onto this drawer"
        );
        let mut queue = Self {
            overwrite_files: target.overwrite_files,
            batch_id: Some(token),
            total: sources.len(),
            ..Self::default()
        };
        for source in sources {
            let name = basename(source.path())?;
            let remote = entry_path(&target.parent, &name)?;
            let folder = source.is_directory();
            let op = source.into_operation(
                remote.clone(),
                !folder && target.overwrite_files,
                !folder && target.atomic_files,
            );
            let transfer = if folder {
                TransferFile::UploadFolder { remote }
            } else {
                TransferFile::Upload {
                    name,
                    remote,
                    atomic: target.atomic_files,
                }
            };
            queue.remaining.push_back((op, transfer));
        }
        Ok(queue)
    }
}

fn basename(local: &std::path::Path) -> Result<String> {
    local
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .context("Choose a named file or folder with a UTF-8 name")
}

impl FileDrawer {
    pub(super) fn transfer_id(&mut self, pane: u64) -> TransferId {
        if let Some(batch) = self.queue.batch_id {
            return batch;
        }
        self.transfer_serial = self.transfer_serial.wrapping_add(1).max(1);
        TransferId {
            pane,
            generation: self.generation,
            serial: self.transfer_serial,
        }
    }

    pub(super) fn start_drop(&mut self, target: &DropTarget) -> Option<TransferId> {
        if self.busy() || !target.current(self) {
            return None;
        }
        let token = self.transfer_id(target.pane);
        self.preparing_drop = Some(DropPreparation {
            id: token,
            cancel: TransferCancel::default(),
        });
        self.status = "Checking dropped files and folders…".into();
        self.end_filter_edit();
        Some(token)
    }

    pub(super) fn classify_drop(
        &mut self,
        target: DropTarget,
        paths: Vec<PathBuf>,
        session: &Session,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        let Some(id) = self.start_drop(&target) else {
            return;
        };
        session.events.sftp_transfer_started(id);
        let cancel = self.preparing_drop.as_ref().unwrap().cancel.clone();
        let rejected = Message::Files(FileAction::PreparedDrop(
            target.clone(),
            id,
            Err("Could not check dropped items".into()),
        ));
        ctx.spawn_background_with_rejection(
            move |_| {
                let result = classify_drop(paths, &cancel).map_err(|e| format!("{e:#}"));
                Message::Files(FileAction::PreparedDrop(target, id, result))
            },
            rejected,
        );
    }

    pub(super) fn finish_drop(&mut self, target: &DropTarget, id: TransferId) -> bool {
        if self.preparing_drop.as_ref().map(|active| active.id) != Some(id) {
            return false;
        }
        self.preparing_drop = None;
        if self.status == "Checking dropped files and folders…" {
            self.status.clear();
        }
        target.current(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drawer() -> FileDrawer {
        FileDrawer {
            open: true,
            generation: 7,
            directory: Some("/logs".into()),
            replace: true,
            ..Default::default()
        }
    }

    #[test]
    fn classification_binds_to_the_original_drawer_independently_of_focus() {
        let mut source = drawer();
        let mut sibling = drawer();
        let origin = DropTarget::new(9, &source, "/logs");
        let token = source.start_drop(&origin).unwrap();
        assert!(source.busy());
        assert!(!sibling.finish_drop(&origin, token));
        assert!(source.finish_drop(&origin, token));
        assert!(!source.finish_drop(&origin, token));

        let token = source.start_drop(&origin).unwrap();
        let scanner = source.preparing_drop.as_ref().unwrap().cancel.clone();
        source.close();
        assert!(scanner.is_cancelled());
        source.open = true;
        assert!(source.start_drop(&origin).is_none());
        let fresh = DropTarget::new(9, &source, "/logs");
        let new_token = source.start_drop(&fresh).unwrap();
        let scanner = source.preparing_drop.as_ref().unwrap().cancel.clone();
        assert!(!source.finish_drop(&origin, token));
        assert!(source.busy());
        source.observe_connection(8);
        assert!(scanner.is_cancelled());
        assert!(!source.finish_drop(&fresh, new_token));
        assert!(!source.busy());
    }

    #[test]
    fn delayed_classification_is_discarded_after_a_new_listing_at_the_same_path() {
        let mut source = drawer();
        let origin = DropTarget::new(9, &source, "/logs");
        let token = source.start_drop(&origin).unwrap();
        source.accept_listing(None, "/other".into(), vec![]);
        source.accept_listing(None, "/logs".into(), vec![]);
        source.status = "New listing".into();
        assert!(!source.finish_drop(&origin, token));
        assert_eq!(source.status, "New listing");
        assert!(!source.busy());
        assert!(source.start_drop(&origin).is_none());
    }

    #[test]
    fn accepted_mixed_drop_keeps_order_origin_and_file_only_overwrite_consent() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("one.txt");
        let folder = temp.path().join("logs 雪");
        let last = temp.path().join("three");
        std::fs::write(&first, "one").unwrap();
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(&last, "three").unwrap();
        let mut drawer = drawer();
        let target = DropTarget::new(9, &drawer, "/logs");
        let paths = vec![first, folder, last];
        let data = DroppedData::StorageItems(
            paths
                .iter()
                .map(|path| windows_reactor::DroppedStorageItem {
                    name: "display label".into(),
                    path: path.to_str().unwrap().into(),
                })
                .collect(),
        );
        let FileAction::Drop(captured, received) = target.data(data) else {
            panic!("drop event")
        };
        assert_eq!(received, paths);
        let token = drawer.start_drop(&captured).unwrap();
        let items = classify_drop(received, &TransferCancel::default()).unwrap();
        drawer.replace = false;
        assert!(drawer.finish_drop(&captured, token));
        drawer.queue = TransferQueue::dropped(&captured, token, items).unwrap();
        assert!(drawer.transfer_id(captured.pane) == token);
        drawer.close();
        for ((operation, _), (path, overwrite)) in drawer.queue.remaining.iter().zip([
            ("/logs/one.txt", true),
            ("/logs/logs 雪", false),
            ("/logs/three", true),
        ]) {
            assert!(
                matches!(operation, Operation::PutDrop {remote, overwrite: replace, ..} if remote == path && *replace == overwrite)
            );
        }
        assert_eq!(drawer.queue.remaining.len(), 3);
        drawer.queue.completed_one();
        assert!(drawer.transfer_id(captured.pane) == token);
        let mut status = "Stopped on second item.".to_owned();
        drawer.queue.finish(&mut status);
        assert_eq!(status, "Stopped on second item. 1 of 3 items completed.");
        assert!(drawer.queue.remaining.is_empty());
        assert!(drawer.transfer_id(captured.pane) != token);
    }

    #[test]
    fn reconnect_counts_an_acknowledged_current_item_only_once() {
        let file = TransferFile::UploadFolder {
            remote: "/logs/tree".into(),
        };
        for reply_value in [None, Some(true), Some(false)] {
            let (sender, mut response) = oneshot::channel();
            let mut queue = TransferQueue {
                total: 4,
                completed: 1,
                ..Default::default()
            };
            if let Some(cancelled) = reply_value {
                sender
                    .send(Ok(serde_json::json!({"cancelled":cancelled, "files":0,
                    "directories":1, "skipped":0, "bytes_transferred":0})))
                    .unwrap();
            }
            let (status, saved, _) = file.reconnect_status(&mut response, 0, &mut queue);
            assert!(saved.is_none());
            let completed = if reply_value == Some(false) { 2 } else { 1 };
            assert!(
                status.ends_with(&format!("{completed} of 4 items completed.")),
                "{status}"
            );
        }
    }
}
