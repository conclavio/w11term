use super::*;
use russh_sftp::extensions::Statvfs;

const READ_ONLY: u64 = 1;

#[derive(Clone, Copy)]
pub enum Action {
    Refresh,
    Close,
}

pub(super) struct Panel {
    serial: u64,
    directory: String,
    state: State,
}

enum State {
    Loading,
    Loaded(Result<Capacity, String>),
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
        context: &ViewContext<TerminalApp>,
    ) -> View {
        let serial = self.serial;
        let message = move |action| {
            Message::Files(FileAction::FilesystemSpace(
                pane, generation, serial, action,
            ))
        };
        let mut rows = vec![KeyedView::new(
            "folder",
            TextBlock::new()
                .automation_id("sftp-space-folder")
                .text(&self.directory)
                .text_wrapping(TextWrapping::Wrap),
        )];
        match &self.state {
            State::Loading => rows.push(KeyedView::new(
                "loading",
                TextBlock::new().text("Reading filesystem space…"),
            )),
            State::Loaded(Err(error)) => rows.push(KeyedView::new(
                "error",
                TextBlock::new()
                    .automation_id("sftp-space-error")
                    .text(format!("Could not read filesystem space. {error}"))
                    .text_wrapping(TextWrapping::Wrap),
            )),
            State::Loaded(Ok(capacity)) => {
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
        }
        rows.push(KeyedView::new(
            "refresh",
            Button::new()
                .automation_name("Refresh space")
                .is_enabled(!matches!(self.state, State::Loading))
                .on_click(context.callback(move |_| message(Action::Refresh)))
                .content("Refresh space"),
        ));
        ContentDialog::new()
            .title("Filesystem space")
            .close_button_text("Close space report")
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

impl FileDrawer {
    pub(super) fn start_filesystem_space(&mut self, revision: u64) -> Option<(u64, Operation)> {
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
        self.filesystem_space = Some(Panel {
            serial,
            directory: directory.clone(),
            state: State::Loading,
        });
        Some((serial, Operation::Statvfs { path: directory }))
    }

    pub(super) fn refresh_filesystem_space(&mut self, serial: u64) -> Option<Operation> {
        let panel = self
            .filesystem_space
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
        Some(Operation::Statvfs {
            path: panel.directory.clone(),
        })
    }

    pub(super) fn finish_filesystem_space(&mut self, serial: u64, reply: Result<Value, String>) {
        if let Some(panel) = self
            .filesystem_space
            .as_mut()
            .filter(|panel| panel.serial == serial)
        {
            panel.state = State::Loaded(reply.and_then(|value| {
                Capacity::from_reply(value).map_err(|error| format!("{error:#}"))
            }));
        }
    }

    pub(super) fn close_filesystem_space(&mut self, serial: u64) {
        if self
            .filesystem_space
            .as_ref()
            .is_some_and(|panel| panel.serial == serial)
        {
            self.cancel_filesystem_space();
        }
    }

    pub(super) fn cancel_filesystem_space(&mut self) {
        if self.filesystem_space.take().is_some() {
            if matches!(
                self.pending.as_ref().map(|pending| &pending.step),
                Some(Step::FilesystemSpace(_))
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
                .start_filesystem_space(revision.wrapping_sub(1))
                .is_none()
        );
        let (serial, request) = drawer.start_filesystem_space(revision).unwrap();
        assert!(matches!(request, Operation::Statvfs { path } if path == "/work tree"));
        assert!(drawer.entry_dialog_open());
        assert!(drawer.refresh_filesystem_space(serial).is_none());
        drawer.finish_filesystem_space(serial, Err("The server does not advertise statvfs".into()));
        assert!(matches!(
            &drawer.filesystem_space.as_ref().unwrap().state,
            State::Loaded(Err(error)) if error.contains("does not advertise")
        ));
        assert!(drawer.refresh_filesystem_space(serial + 1).is_none());
        assert!(matches!(drawer.refresh_filesystem_space(serial),
            Some(Operation::Statvfs { path }) if path == "/work tree"));
        drawer.close_filesystem_space(serial);
        drawer.finish_filesystem_space(serial, Ok(statistics(4096, 1, 0, 0)));
        assert!(!drawer.entry_dialog_open());
    }
}
