use super::*;

#[derive(Default)]
pub(super) enum State {
    #[default]
    Idle,
    Checking,
    Failed(String),
}

impl State {
    pub(super) fn checking(&self) -> bool {
        matches!(self, Self::Checking)
    }

    pub(super) fn text(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Checking => Some("Checking the remote entry…"),
            Self::Failed(error) => Some(error),
        }
    }
}

#[derive(Clone)]
pub struct Listing {
    directory: String,
    selected: String,
    entries: Vec<Entry>,
}

impl Listing {
    fn from_reply(reply: Value, order: SortOrder) -> Result<Self> {
        let directory = reply["directory"]
            .as_str()
            .context("Missing containing folder")?;
        let selected = reply["selected"]
            .as_str()
            .context("Missing compared entry")?;
        let entries = directory_entries(&reply, order)?;
        ensure!(
            entries.iter().any(|entry| entry.name == selected),
            "The remote entry is no longer in its folder. Compare again"
        );
        Ok(Self {
            directory: directory.into(),
            selected: selected.into(),
            entries,
        })
    }
}

impl FileDrawer {
    pub(in crate::app::file_drawer) fn begin_compared_reveal(
        &mut self,
        serial: u64,
        revision: u64,
        index: usize,
    ) -> Option<Operation> {
        let panel = self.folder_report.as_mut()?;
        if !self.open
            || self.pending.is_some()
            || panel.serial != serial
            || panel.revision != revision
            || panel.reveal.checking()
        {
            return None;
        }
        let super::State::Loaded(Ok(Report::Comparison(report))) = &panel.state else {
            return None;
        };
        if self.directory.as_ref() != Some(&report.current_root) {
            return None;
        }
        let item = report.differences.get(index)?;
        let expected = item.current?;
        if item.path.split('/').any(|name| {
            matches!(name, "" | "." | "..")
                || name.contains('\\')
                || name.chars().any(char::is_control)
        }) {
            return None;
        }
        let path = format!(
            "{}/{}",
            report.current_root.trim_end_matches('/'),
            item.path
        );
        panel.reveal = State::Checking;
        Some(Operation::RevealComparedEntry { path, expected })
    }

    pub(in crate::app::file_drawer) fn parse_compared_listing(
        &mut self,
        pane: u64,
        serial: u64,
        revision: u64,
        reply: Result<Value, String>,
        context: &ComponentContext<TerminalApp>,
    ) {
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                self.finish_compared_reveal(serial, revision, Err(error));
                return;
            }
        };
        let generation = self.generation;
        let order = self.sort_order;
        let message = move |reply| {
            Message::Files(FileAction::ComparedEntryListed(
                pane, generation, serial, revision, reply,
            ))
        };
        context.spawn_background_with_rejection(
            move |_| {
                message(Listing::from_reply(reply, order).map_err(|error| format!("{error:#}")))
            },
            message(Err("Unable to display the containing folder".into())),
        );
    }

    pub(in crate::app::file_drawer) fn finish_compared_reveal(
        &mut self,
        serial: u64,
        revision: u64,
        reply: Result<Listing, String>,
    ) {
        let Some(panel) = self.folder_report.as_mut().filter(|panel| {
            panel.serial == serial && panel.revision == revision && panel.reveal.checking()
        }) else {
            return;
        };
        let listing = match reply {
            Ok(listing) => listing,
            Err(error) => {
                panel.reveal = State::Failed(format!("Could not show remote entry. {error}"));
                return;
            }
        };
        self.close_folder_report(serial);
        self.following = false;
        self.status.clear();
        self.reveal = Some(RevealTarget::Existing(listing.selected));
        self.accept_listing(None, listing.directory, listing.entries);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use w11term::sftp::{FolderChange, FolderDifference, FolderEntry};

    #[test]
    fn reveal_uses_the_current_side_and_discards_previous_report_callbacks() {
        let mut files = FileDrawer {
            open: true,
            generation: 7,
            directory: Some("/current".into()),
            ..Default::default()
        };
        let serial = files.begin_folder_report(Kind::Comparison, "/current".into());
        let file: FolderEntry = serde_json::from_value(json!({
            "kind": 0o100000, "size": 12, "mtime": null,
        }))
        .unwrap();
        files.folder_report.as_mut().unwrap().state =
            super::super::State::Loaded(Ok(Report::Comparison(Arc::new(FolderComparison {
                current_root: "/current".into(),
                differences: vec![
                    FolderDifference {
                        path: "gone.txt".into(),
                        change: FolderChange::ReferenceOnly,
                        reference: Some(file),
                        current: None,
                    },
                    FolderDifference {
                        path: "nested/live.txt".into(),
                        change: FolderChange::Metadata,
                        reference: Some(file),
                        current: Some(file),
                    },
                ],
                ..Default::default()
            }))));
        assert!(files.begin_compared_reveal(serial, 0, 0).is_none());
        assert!(files.begin_compared_reveal(serial, 1, 1).is_none());
        assert!(matches!(
            files.begin_compared_reveal(serial, 0, 1),
            Some(Operation::RevealComparedEntry { path, expected })
                if path == "/current/nested/live.txt" && expected == file
        ));
        let listing = Listing::from_reply(
            json!({
                "directory": "/current/nested",
                "selected": "live.txt",
                "entries": [{"filename":"live.txt", "attrs":{"permissions":0o100644}}]
            }),
            files.sort_order,
        )
        .unwrap();
        files.close_folder_report(serial);
        files.finish_compared_reveal(serial, 0, Ok(listing));
        assert_eq!(files.directory.as_deref(), Some("/current"));
        assert!(files.selected.is_none());
    }
}
