use super::*;
use std::path::Path;
use w11term::sftp::validate_download_names;

impl Entry {
    pub(super) fn can_mark_for_download(&self) -> bool {
        supports_batch_download(self.kind)
    }
}

fn supports_batch_download(kind: ListedKind) -> bool {
    matches!(
        kind,
        ListedKind::Known(EntryKind::File | EntryKind::Directory)
    )
}

impl FileDrawer {
    pub(super) fn set_download_mark(&mut self, entry: ListedEntry, checked: bool) {
        if !self.open
            || self.busy()
            || !supports_batch_download(entry.kind)
            || !self.has_entry(&entry)
        {
            return;
        }
        if let Some(marked) = &mut self.marked_downloads {
            if checked {
                marked.insert(entry.name);
            } else {
                marked.remove(&entry.name);
            }
        }
    }

    pub(super) fn marked_entries(&self) -> Result<Vec<&Entry>> {
        let marks = self
            .marked_downloads
            .as_ref()
            .filter(|marked| !marked.is_empty())
            .context("Mark at least one file or folder")?;
        let entries: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| marks.contains(&entry.name) && entry.can_mark_for_download())
            .collect();
        ensure!(
            entries.len() == marks.len(),
            "The remote listing changed. Mark the files again."
        );
        validate_download_names(entries.iter().map(|entry| entry.name.as_str()))?;
        Ok(entries)
    }

    pub(super) fn download_queue(&self, parent: &Path, id: TransferId) -> Result<TransferQueue> {
        ensure!(parent.is_absolute(), "Choose an absolute local folder");
        let remote_folder = self.directory.as_deref().context("Missing remote folder")?;
        let entries = self.marked_entries()?;
        let total = entries.len();
        let remaining = entries
            .into_iter()
            .map(|entry| {
                let remote = remote_child(remote_folder, &entry.name)?;
                Ok(download_to(
                    remote,
                    parent.join(&entry.name),
                    entry.kind == ListedKind::Known(EntryKind::Directory),
                    false,
                    None,
                ))
            })
            .collect::<Result<_>>()?;
        Ok(TransferQueue {
            remaining,
            batch_id: Some(id),
            total,
            download_folder: Some(Arc::from(parent)),
            ..TransferQueue::default()
        })
    }
}

pub(super) fn download_to(
    remote: String,
    local: PathBuf,
    directory: bool,
    overwrite: bool,
    verification: Option<DownloadVerification>,
) -> (Operation, TransferFile) {
    if directory {
        (
            Operation::GetDir {
                remote: remote.clone(),
                local: local.clone(),
            },
            TransferFile::DownloadFolder { remote, local },
        )
    } else {
        let resume = verification.as_ref().is_some_and(|options| options.resume);
        (
            Operation::Get {
                local: local.clone(),
                remote: remote.clone(),
                overwrite,
                max_bytes: None,
                expected_sha256: verification.map(|options| options.sha256),
                resume,
            },
            TransferFile::Download {
                remote,
                resume,
                local,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: impl Into<String>, kind: EntryKind) -> Entry {
        Entry {
            name: name.into(),
            kind: ListedKind::Known(kind),
            mtime: None,
            size: None,
        }
    }

    fn listed(drawer: &FileDrawer, row: &Entry) -> ListedEntry {
        ListedEntry {
            parent: drawer.directory.clone().unwrap(),
            revision: drawer.listing_revision,
            name: row.name.clone(),
            kind: row.kind,
        }
    }

    #[test]
    fn marks_span_pages_and_filters_but_are_scoped_to_the_original_listing() {
        let mut drawer = FileDrawer {
            open: true,
            generation: 1,
            replace: true,
            ..FileDrawer::default()
        };
        let mut entries = vec![entry("folder", EntryKind::Directory)];
        entries.extend((0..PAGE_SIZE).map(|i| entry(format!("{i:02}.log"), EntryKind::File)));
        drawer.accept_listing(None, "/logs".into(), entries.clone());
        drawer.marked_downloads = Some(BTreeSet::new());
        let folder = listed(&drawer, &entries[0]);
        drawer.set_download_mark(folder.clone(), true);
        drawer.set_page(1);
        let file = listed(&drawer, drawer.page_entries().next().unwrap());
        drawer.set_download_mark(file, true);
        drawer.set_filter("folder".into());
        let id = drawer.transfer_id(1);
        let queue = drawer
            .download_queue(Path::new(r"C:\download"), id)
            .unwrap();
        assert_eq!(queue.total, 2);
        assert!(queue.batch_id == Some(id));
        let mut work = queue.remaining.into_iter().map(|(operation, _)| operation);
        assert!(
            matches!(work.next(), Some(Operation::GetDir { remote, local })
            if remote == "/logs/folder" && local == Path::new(r"C:\download\folder"))
        );
        assert!(
            matches!(work.next(), Some(Operation::Get { remote, local, overwrite: false, .. })
            if remote == format!("/logs/{:02}.log", PAGE_SIZE - 1)
                && local == Path::new(r"C:\download").join(format!("{:02}.log", PAGE_SIZE - 1)))
        );

        drawer.accept_listing(None, "/logs".into(), entries);
        assert!(drawer.marked_downloads.is_none());
        drawer.marked_downloads = Some(BTreeSet::new());
        drawer.set_download_mark(folder, true);
        assert!(drawer.marked_entries().is_err());
    }

    #[test]
    fn rejects_windows_name_conflicts_and_reserved_names_before_starting_the_batch() {
        for names in [["Readme.txt", "README.txt"], ["report.txt", "NUL.txt"]] {
            let files = names.map(|name| entry(name, EntryKind::File));
            let drawer = FileDrawer {
                directory: Some("/logs".into()),
                entries: files.into(),
                marked_downloads: Some(names.into_iter().map(String::from).collect()),
                ..FileDrawer::default()
            };
            assert!(TransferPicker::DownloadMarked.purpose(&drawer).is_err());
        }
    }

    #[test]
    fn interrupted_batch_keeps_only_the_destination_of_confirmed_downloads() {
        let destination = Arc::from(Path::new(r"C:\downloads"));
        let (_, item) = download_to(
            "/logs/second".into(),
            PathBuf::from(r"C:\downloads\second"),
            false,
            false,
            None,
        );
        for completed in [0, 1] {
            let mut queue = TransferQueue {
                download_folder: Some(Arc::clone(&destination)),
                total: 2,
                ..TransferQueue::default()
            };
            if completed > 0 {
                queue.record_download(&item);
                queue.completed_one();
            }
            let (sender, mut reply) = oneshot::channel();
            sender
                .send(Ok(serde_json::json!({"cancelled": true})))
                .unwrap();
            let (status, saved, outcome) = item.reconnect_status(&mut reply, 0, &mut queue);
            assert_eq!(outcome, TransferOutcome::Cancelled);
            assert!(status.ends_with(&format!("{completed} of 2 items completed.")));
            assert_eq!(
                saved,
                (completed > 0).then(|| SavedDownload::BatchFolder(destination.clone()))
            );
        }
    }
}
