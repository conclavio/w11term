use super::*;
use russh_sftp::extensions::Statvfs;
use w11term::sftp::FolderContents;

const READ_ONLY: u64 = 1;

#[derive(Clone, Copy)]
pub enum Action {
    Refresh,
    Stop,
    Close,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Space,
    Contents,
}

impl Kind {
    fn operation(self, path: String) -> Operation {
        match self {
            Self::Space => Operation::Statvfs { path },
            Self::Contents => Operation::MeasureFolder { path },
        }
    }

    fn report(self, reply: Value) -> Result<Report> {
        match self {
            Self::Space => Capacity::from_reply(reply).map(Report::Space),
            Self::Contents => {
                let counts: FolderContents = serde_json::from_value(reply)?;
                let bytes = counts
                    .logical_bytes
                    .parse()
                    .context("Invalid folder size")?;
                Ok(Report::Contents(counts, bytes))
            }
        }
    }
}

pub(super) struct Panel {
    serial: u64,
    directory: String,
    kind: Kind,
    state: State,
    pub(super) cancel: TransferCancel,
}

enum State {
    Loading,
    Loaded(Result<Report, String>),
}

enum Report {
    Space(Capacity),
    Contents(FolderContents, u128),
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

struct Capacity {
    total: u128,
    used: u128,
    available: u128,
    reserved: u128,
    read_only: bool,
}

impl Capacity {
    fn from_reply(reply: Value) -> Result<Self> {
        let stats: Statvfs =
            serde_json::from_value(reply).context("Invalid filesystem statistics")?;
        ensure!(
            stats.fragment_size != 0,
            "The server did not report the filesystem allocation unit"
        );
        ensure!(
            stats.blocks_avail <= stats.blocks_free && stats.blocks_free <= stats.blocks,
            "The server returned inconsistent filesystem block counts"
        );
        let bytes = |blocks| u128::from(blocks) * u128::from(stats.fragment_size);
        Ok(Self {
            total: bytes(stats.blocks),
            used: bytes(stats.blocks - stats.blocks_free),
            available: bytes(stats.blocks_avail),
            reserved: bytes(stats.blocks_free - stats.blocks_avail),
            read_only: stats.flags & READ_ONLY != 0,
        })
    }
}

fn size_value(label: &str, bytes: u128) -> String {
    if bytes == 1 {
        format!("{label}  1 byte")
    } else if bytes < 1024 {
        format!("{label}  {bytes} bytes")
    } else {
        format!("{label}  {} ({bytes} bytes)", byte_size_label(bytes))
    }
}

impl Panel {
    pub(super) fn dialog(
        &self,
        pane: u64,
        generation: u64,
        progress: Option<&SearchProgress>,
        context: &ViewContext<TerminalApp>,
    ) -> View {
        let serial = self.serial;
        let (title, refresh, close) = match self.kind {
            Kind::Space => ("Filesystem space", "Refresh space", "Close space report"),
            Kind::Contents => ("Folder contents", "Measure again", "Close contents report"),
        };
        let message = move |action| {
            Message::Files(FileAction::FolderReport(pane, generation, serial, action))
        };
        let mut rows = vec![KeyedView::new(
            "folder",
            TextBlock::new()
                .automation_id(match self.kind {
                    Kind::Space => "sftp-space-folder",
                    Kind::Contents => "sftp-contents-folder",
                })
                .text(&self.directory)
                .text_wrapping(TextWrapping::Wrap),
        )];
        match &self.state {
            State::Loading => {
                let loading = match self.kind {
                    Kind::Space => "Reading filesystem space…",
                    Kind::Contents if self.cancel.is_cancelled() => "Stopping folder scan…",
                    Kind::Contents => "Scanning folder contents…",
                };
                rows.push(KeyedView::new("loading", TextBlock::new().text(loading)));
                if let Some(progress) = progress.filter(|_| self.kind == Kind::Contents) {
                    rows.push(KeyedView::new(
                        "progress",
                        TextBlock::new()
                            .automation_id("sftp-contents-progress")
                            .text(format!(
                                "Folders visited  {} · Entries scanned  {}",
                                progress.scanned_directories, progress.scanned_entries
                            ))
                            .text_wrapping(TextWrapping::Wrap),
                    ));
                }
                if self.kind == Kind::Contents {
                    rows.push(KeyedView::new(
                        "stop",
                        Button::new()
                            .automation_name("Stop scan")
                            .is_enabled(!self.cancel.is_cancelled())
                            .on_click(context.callback(move |_| message(Action::Stop)))
                            .content("Stop scan"),
                    ));
                }
            }
            State::Loaded(Err(error)) => {
                let (id, prefix) = match self.kind {
                    Kind::Space => ("sftp-space-error", "Could not read filesystem space"),
                    Kind::Contents => ("sftp-contents-error", "Could not measure folder contents"),
                };
                rows.push(KeyedView::new(
                    "error",
                    TextBlock::new()
                        .automation_id(id)
                        .text(format!("{prefix}. {error}"))
                        .text_wrapping(TextWrapping::Wrap),
                ));
            }
            State::Loaded(Ok(Report::Space(capacity))) => {
                for (key, label, bytes) in [
                    (
                        "available",
                        "Available to unprivileged users",
                        capacity.available,
                    ),
                    ("used", "Used", capacity.used),
                    ("capacity", "Total capacity", capacity.total),
                    ("reserved", "Reserved free space", capacity.reserved),
                ] {
                    if key == "reserved" && bytes == 0 {
                        continue;
                    }
                    rows.push(KeyedView::new(
                        key,
                        TextBlock::new()
                            .automation_id(format!("sftp-space-{key}"))
                            .text(size_value(label, bytes))
                            .text_wrapping(TextWrapping::Wrap),
                    ));
                }
                if capacity.read_only {
                    rows.push(KeyedView::new(
                        "read-only",
                        TextBlock::new().text("Read-only filesystem"),
                    ));
                }
            }
            State::Loaded(Ok(Report::Contents(counts, bytes))) => {
                rows.extend(contents_rows(counts, *bytes));
            }
        }
        rows.push(KeyedView::new(
            "refresh",
            Button::new()
                .automation_name(refresh)
                .is_enabled(!matches!(self.state, State::Loading))
                .on_click(context.callback(move |_| message(Action::Refresh)))
                .content(refresh),
        ));
        ContentDialog::new()
            .title(title)
            .close_button_text(close)
            .is_open(true)
            .on_closed(context.callback(move |_| message(Action::Close)))
            .content(
                StackPanel::new()
                    .min_width(290.0)
                    .spacing(8.0)
                    .keyed_children(rows),
            )
    }
}

fn contents_rows(counts: &FolderContents, bytes: u128) -> Vec<KeyedView> {
    let mut rows = vec![KeyedView::new(
        "logical-bytes",
        TextBlock::new()
            .automation_id("sftp-contents-logical-bytes")
            .text(size_value("Logical file size", bytes))
            .text_wrapping(TextWrapping::Wrap),
    )];
    for (key, label, count, always) in [
        ("files", "Regular files", counts.files, true),
        (
            "folders",
            "Folders visited",
            counts.scanned_directories,
            true,
        ),
        ("links", "Symbolic links skipped", counts.links, false),
        (
            "missing-sizes",
            "Files without a reported size",
            counts.files_without_size,
            false,
        ),
        (
            "unclassified",
            "Entries that could not be classified",
            counts.unclassified_entries,
            false,
        ),
        (
            "unreadable",
            "Folders that could not be listed",
            counts.skipped_directories,
            false,
        ),
    ] {
        if always || count != 0 {
            rows.push(KeyedView::new(
                key,
                TextBlock::new()
                    .automation_id(format!("sftp-contents-{key}"))
                    .text(format!("{label}  {count}"))
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
    }
    let partial = match (counts.cancelled, counts.truncated) {
        (true, _) => Some("Partial results. Scan stopped."),
        (false, true) => Some("Partial results. Scan limit reached."),
        _ if counts.skipped_directories > 0
            || counts.files_without_size > 0
            || counts.unclassified_entries > 0 =>
        {
            Some("Partial results. Some entries could not be measured.")
        }
        _ => None,
    };
    if let Some(message) = partial {
        rows.push(KeyedView::new(
            "partial",
            TextBlock::new()
                .automation_id("sftp-contents-partial")
                .text(message),
        ));
    }
    rows
}

impl FileDrawer {
    pub(super) fn start_folder_report(
        &mut self,
        revision: u64,
        kind: Kind,
    ) -> Option<(u64, Operation)> {
        if !self.open
            || self.busy()
            || self.generation == 0
            || revision != self.listing_revision
            || self.search.is_some()
            || self.bookmarks.is_some()
        {
            return None;
        }
        let directory = self.directory.clone()?;
        self.end_filter_edit();
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        let serial = self.dialog_serial;
        self.folder_report = Some(Panel {
            serial,
            directory: directory.clone(),
            kind,
            state: State::Loading,
            cancel: TransferCancel::default(),
        });
        Some((serial, kind.operation(directory)))
    }

    pub(super) fn refresh_folder_report(&mut self, serial: u64) -> Option<Operation> {
        let panel = self
            .folder_report
            .as_mut()
            .filter(|panel| panel.serial == serial)?;
        if !self.open
            || self.directory.as_ref() != Some(&panel.directory)
            || self.pending.is_some()
            || matches!(panel.state, State::Loading)
        {
            return None;
        }
        panel.state = State::Loading;
        panel.cancel = TransferCancel::default();
        Some(panel.kind.operation(panel.directory.clone()))
    }

    pub(super) fn finish_folder_report(&mut self, serial: u64, reply: Result<Value, String>) {
        if let Some(panel) = self
            .folder_report
            .as_mut()
            .filter(|panel| panel.serial == serial)
        {
            panel.state = State::Loaded(reply.and_then(|value| {
                panel
                    .kind
                    .report(value)
                    .map_err(|error| format!("{error:#}"))
            }));
        }
    }

    pub(super) fn stop_folder_report(&mut self, serial: u64) {
        if let Some(panel) = self
            .folder_report
            .as_ref()
            .filter(|panel| panel.serial == serial)
            && panel.kind == Kind::Contents
            && matches!(panel.state, State::Loading)
        {
            panel.cancel.cancel();
        }
    }

    pub(super) fn close_folder_report(&mut self, serial: u64) {
        if self
            .folder_report
            .as_ref()
            .is_some_and(|panel| panel.serial == serial)
        {
            self.cancel_folder_report();
        }
    }

    pub(super) fn cancel_folder_report(&mut self) {
        if self.folder_report.take().is_some() {
            if matches!(
                self.pending.as_ref().map(|pending| &pending.step),
                Some(Step::FolderReport(_))
            ) {
                self.pending.take();
            }
            self.cached.get_mut().take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn statistics(fragment_size: u64, blocks: u64, free: u64, available: u64) -> Value {
        json!({
            "block_size":8192, "fragment_size":fragment_size,
            "blocks":blocks, "blocks_free":free, "blocks_avail":available,
            "inodes":0, "inodes_free":0, "inodes_avail":0, "fs_id":3, "flags":1, "name_max":255
        })
    }

    #[test]
    fn capacity_uses_fragments_and_distinguishes_reserved_free_space() {
        let capacity = Capacity::from_reply(statistics(4096, 1 << 20, 1 << 19, 1 << 18)).unwrap();
        assert_eq!(capacity.total, 4 << 30);
        assert_eq!(capacity.used, 2 << 30);
        assert_eq!(capacity.available, 1 << 30);
        assert_eq!(capacity.reserved, 1 << 30);
        assert!(capacity.read_only);
        assert_eq!(
            size_value("Total", capacity.total),
            "Total  4 GiB (4294967296 bytes)"
        );
    }

    #[test]
    fn invalid_counts_fail_and_large_counts_keep_the_exact_size() {
        assert!(Capacity::from_reply(statistics(0, 20, 10, 5)).is_err());
        assert!(Capacity::from_reply(statistics(4096, 10, 11, 5)).is_err());
        assert!(Capacity::from_reply(statistics(4096, 10, 5, 6)).is_err());
        let empty = Capacity::from_reply(statistics(4096, 0, 0, 0)).unwrap();
        assert_eq!(empty.total, 0);
        let max = u64::MAX;
        let huge = Capacity::from_reply(statistics(max, max, max, max)).unwrap();
        assert_eq!(huge.total, u128::from(max).pow(2));
        assert!(size_value("Total", huge.total).ends_with(&format!("({} bytes)", huge.total)));
    }

    #[test]
    fn the_report_tracks_the_current_folder_and_discards_closed_callbacks() {
        let mut drawer = FileDrawer {
            open: true,
            generation: 7,
            ..Default::default()
        };
        drawer.accept_listing(None, "/work tree".into(), Vec::new());
        let revision = drawer.listing_revision;
        assert!(
            drawer
                .start_folder_report(revision.wrapping_sub(1), Kind::Space)
                .is_none()
        );
        let (serial, request) = drawer.start_folder_report(revision, Kind::Space).unwrap();
        assert!(matches!(request, Operation::Statvfs { path } if path == "/work tree"));
        assert!(drawer.entry_dialog_open());
        assert!(drawer.refresh_folder_report(serial).is_none());
        drawer.finish_folder_report(serial, Err("The server does not advertise statvfs".into()));
        assert!(matches!(
            &drawer.folder_report.as_ref().unwrap().state,
            State::Loaded(Err(error)) if error.contains("does not advertise")
        ));
        assert!(drawer.refresh_folder_report(serial + 1).is_none());
        assert!(matches!(drawer.refresh_folder_report(serial),
            Some(Operation::Statvfs { path }) if path == "/work tree"));
        drawer.close_folder_report(serial);
        drawer.finish_folder_report(serial, Ok(statistics(4096, 1, 0, 0)));
        assert!(!drawer.entry_dialog_open());
    }
}
