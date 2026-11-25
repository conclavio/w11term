use super::*;
use w11term::sftp::{FilenameSearch, valid_filename_query};

const RESULTS_PER_PAGE: usize = 25;

#[derive(Clone)]
pub enum Action {
    Show(u64),
    Restore(u64),
    Query(u64, String),
    Run(u64),
    Cancel(u64),
    Page(u64, usize),
    Reveal(u64, FilenameMatch),
    CopyPaths(u64, Option<FilenameMatch>),
    Back(u64),
}

pub(super) struct Panel {
    pub(super) serial: u64,
    root: String,
    query: String,
    result: Option<FilenameSearch>,
    page: usize,
    pub(super) cancel: Option<TransferCancel>,
    copy_status: Option<String>,
}

fn result_status(result: &FilenameSearch) -> String {
    let mut status = format!(
        "{} matches in {} folders",
        result.matches.len(),
        result.scanned_directories,
    );
    if result.cancelled {
        status.push_str(". Search cancelled");
    } else if result.truncated {
        status.push_str(". Search limit reached");
    }
    if result.skipped_directories > 0 {
        use std::fmt::Write;
        let _ = write!(
            status,
            ". {} unavailable folders skipped",
            result.skipped_directories
        );
    }
    status
}

impl Panel {
    fn page_count(&self) -> usize {
        self.result
            .as_ref()
            .map_or(0, |r| r.matches.len().div_ceil(RESULTS_PER_PAGE))
    }

    fn paths_to_copy(
        &self,
        serial: u64,
        hit: Option<&FilenameMatch>,
    ) -> Result<Option<(String, usize)>> {
        let Some(result) = self.result.as_ref().filter(|_| serial == self.serial) else {
            return Ok(None);
        };
        if let Some(hit) = hit {
            if !result.matches.contains(hit) {
                return Ok(None);
            }
            return Ok(Some((remote_child(&hit.parent, &hit.name)?, 1)));
        }
        if result.matches.is_empty() {
            return Ok(None);
        }
        let paths = result
            .matches
            .iter()
            .map(|hit| {
                let path = remote_child(&hit.parent, &hit.name)?;
                ensure!(
                    !path.contains(['\r', '\n']),
                    "Some paths contain line breaks. Copy those paths individually."
                );
                Ok(path)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Some((paths.join("\r\n"), paths.len())))
    }

    pub(super) fn view(
        &self,
        files: &FileDrawer,
        pane: u64,
        shell_ready: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> View {
        let generation = files.generation;
        let revision = files.listing_revision;
        let serial = self.serial;
        let ready = !files.busy();
        let button = |label: &str, action: Action, enabled: bool| {
            Button::new()
                .automation_name(label)
                .is_enabled(enabled)
                .on_click(ctx.callback(move |_| {
                    Message::Files(FileAction::Search(pane, generation, action.clone()))
                }))
                .content(label)
        };
        let running = self.cancel.as_ref();
        let mut rows = vec![
            KeyedView::new(
                "back",
                button(
                    "Back to files",
                    Action::Back(serial),
                    files.picking.is_none(),
                ),
            ),
            KeyedView::new(
                "path",
                TextBlock::new()
                    .text(format!("Find files in {}", self.root))
                    .text_wrapping(TextWrapping::Wrap),
            ),
            KeyedView::new(
                "query",
                TextBox::new()
                    .automation_name("Filename contains")
                    .placeholder_text("Filename contains")
                    .is_enabled(ready)
                    .text(&self.query)
                    .on_text_changed(ctx.callback(move |text| {
                        Message::Files(FileAction::Search(
                            pane,
                            generation,
                            Action::Query(serial, text),
                        ))
                    })),
            ),
        ];
        let run = if let Some(cancel) = running {
            button(
                "Cancel search",
                Action::Cancel(serial),
                !cancel.is_cancelled(),
            )
        } else {
            button(
                "Search",
                Action::Run(serial),
                ready && valid_filename_query(&self.query),
            )
        };
        let search_actions = if self
            .result
            .as_ref()
            .is_some_and(|result| !result.matches.is_empty())
        {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children((
                    run,
                    button("Copy all paths", Action::CopyPaths(serial, None), ready),
                ))
        } else {
            run
        };
        rows.push(KeyedView::new("run", search_actions));
        if !files.status.is_empty() {
            rows.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .text(&files.status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if let Some(status) = &self.copy_status {
            rows.push(KeyedView::new(
                "copy-status",
                TextBlock::new()
                    .text(status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if let Some([bar, bytes, cancel]) = files.transfer_controls(ctx) {
            rows.extend([
                KeyedView::new("progress-bar", bar),
                KeyedView::new("transfer-bytes", bytes),
                KeyedView::new("cancel-transfer", cancel),
            ]);
        }
        if self.page_count() > 1 {
            rows.push(KeyedView::new(
                "page",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        button(
                            "Previous results",
                            Action::Page(serial, self.page.saturating_sub(1)),
                            self.page > 0,
                        ),
                        button(
                            "Next results",
                            Action::Page(serial, self.page + 1),
                            self.page + 1 < self.page_count(),
                        ),
                    )),
            ));
        }
        let matches = self
            .result
            .iter()
            .flat_map(|result| &result.matches)
            .enumerate()
            .skip(self.page * RESULTS_PER_PAGE)
            .take(RESULTS_PER_PAGE)
            .map(|(index, hit)| {
                let path = format!("{}/{}", hit.parent.trim_end_matches('/'), hit.name);
                let reveal = Button::new()
                    .automation_name(format!("Reveal {path}"))
                    .is_enabled(ready)
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .on_click({
                        let hit = hit.clone();
                        ctx.callback(move |_| {
                            Message::Files(FileAction::Search(
                                pane,
                                generation,
                                Action::Reveal(serial, hit.clone()),
                            ))
                        })
                    })
                    .content(
                        TextBlock::new()
                            .text(&path)
                            .text_wrapping(TextWrapping::Wrap),
                    );
                let entry = if shell_ready
                    && let Some(origin) =
                        directory_split::Origin::search(pane, generation, serial, hit)
                {
                    let shell = Button::new()
                        .automation_name(format!("Open shell in containing folder of {path}"))
                        .is_enabled(ready)
                        .on_click(ctx.callback(move |_| {
                            directory_split::message(
                                directory_split::Destination::Split(Axis::Columns),
                                Some(&origin),
                            )
                        }))
                        .content("Shell in containing folder");
                    StackPanel::new().spacing(4.0).children((reveal, shell))
                } else {
                    reveal
                };
                let mut actions = Vec::new();
                if let Some(label) = match hit.kind {
                    Some(EntryKind::File) => Some("Download…"),
                    Some(EntryKind::Directory) => Some("Download folder…"),
                    _ => None,
                } {
                    let hit = hit.clone();
                    actions.push(KeyedView::new(
                        "download",
                        Button::new()
                            .automation_name(format!("Download {path}"))
                            .is_enabled(ready)
                            .on_click(ctx.callback(move |_| {
                                Message::Files(FileAction::Pick(
                                    pane,
                                    generation,
                                    revision,
                                    TransferPicker::Download(
                                        FileTarget::Search(serial, hit.clone()),
                                        None,
                                    ),
                                ))
                            }))
                            .content(label),
                    ));
                }
                actions.push(KeyedView::new(
                    "copy",
                    Button::new()
                        .automation_name(format!("Copy path {path}"))
                        .is_enabled(ready)
                        .on_click({
                            let hit = hit.clone();
                            ctx.callback(move |_| {
                                Message::Files(FileAction::Search(
                                    pane,
                                    generation,
                                    Action::CopyPaths(serial, Some(hit.clone())),
                                ))
                            })
                        })
                        .content("Copy path"),
                ));
                actions.push(KeyedView::new(
                    "properties",
                    Button::new()
                        .automation_name(format!("Properties {path}"))
                        .is_enabled(ready)
                        .on_click({
                            let hit = hit.clone();
                            ctx.callback(move |_| {
                                Message::Files(FileAction::Preview(
                                    pane,
                                    generation,
                                    FileTarget::Search(serial, hit.clone()),
                                    PreviewKind::Properties,
                                ))
                            })
                        })
                        .content("Properties…"),
                ));
                KeyedView::new(
                    index,
                    Grid::new()
                        .columns([GridLength::Star(1.0), GridLength::Auto])
                        .column_spacing(6.0)
                        .children((
                            entry,
                            StackPanel::new()
                                .grid_column(1)
                                .spacing(4.0)
                                .keyed_children(actions),
                        )),
                )
            });
        Border::new()
            .padding(10.0)
            .background(Color::rgb(31, 32, 35))
            .content(
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Star(1.0)])
                    .children((
                        StackPanel::new().spacing(6.0).keyed_children(rows),
                        ScrollViewer::new()
                            .grid_row(1)
                            .automation_name("SFTP search results")
                            .content(StackPanel::new().spacing(4.0).keyed_children(matches)),
                    )),
            )
    }
}

impl FileDrawer {
    pub(super) fn search_hit_path(&self, serial: u64, hit: &FilenameMatch) -> Result<String> {
        ensure!(
            self.search
                .as_ref()
                .filter(|panel| panel.serial == serial)
                .and_then(|panel| panel.result.as_ref())
                .is_some_and(|result| result.matches.contains(hit)),
            "The search results changed. Select the result again."
        );
        remote_child(&hit.parent, &hit.name)
    }

    pub(super) fn search_download_target<'a>(
        &self,
        serial: u64,
        hit: &'a FilenameMatch,
        verify: bool,
    ) -> Result<downloads::DownloadTarget<'a>> {
        let remote = self.search_hit_path(serial, hit)?;
        ensure!(
            verify || matches!(hit.kind, Some(EntryKind::File | EntryKind::Directory)),
            "Choose a regular file or folder"
        );
        Ok(downloads::DownloadTarget {
            remote,
            name: &hit.name,
            directory: !verify && hit.kind == Some(EntryKind::Directory),
        })
    }

    pub(super) fn close_search(&mut self) {
        if let Some(panel) = self.search.take() {
            if let Some(cancel) = panel.cancel {
                cancel.cancel();
            }
            self.status.clear();
        }
    }

    fn restore_search(&mut self, serial: u64) {
        if self.busy() || !self.open || self.search.is_some() || self.bookmarks.is_some() {
            return;
        }
        let Some(mut panel) = self.saved_search.take_if(|panel| panel.serial == serial) else {
            return;
        };
        self.end_filter_edit();
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        panel.serial = self.dialog_serial;
        self.status = panel
            .result
            .as_ref()
            .map_or_else(String::new, result_status);
        self.search = Some(panel);
    }

    pub(super) fn finish_search(&mut self, serial: u64, result: Result<Value, String>) {
        let Some(panel) = self.search.as_mut().filter(|panel| panel.serial == serial) else {
            return;
        };
        panel.cancel = None;
        panel.copy_status = None;
        self.status = match result.and_then(|value| {
            serde_json::from_value::<FilenameSearch>(value).map_err(|error| error.to_string())
        }) {
            Ok(result) => {
                let status = result_status(&result);
                panel.result = Some(result);
                status
            }
            Err(error) => error,
        };
    }

    pub(super) fn filename_search(
        &mut self,
        pane: u64,
        session: &Session,
        action: Action,
        ctx: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        if let Action::Show(revision) = action {
            if self.busy() || !self.open || self.listing_revision != revision {
                return Ok(());
            }
            let Some(root) = self.directory.clone() else {
                return Ok(());
            };
            self.end_filter_edit();
            self.dialog_serial = self.dialog_serial.wrapping_add(1);
            self.status.clear();
            self.saved_search = None;
            self.search = Some(Panel {
                serial: self.dialog_serial,
                root,
                query: String::new(),
                result: None,
                page: 0,
                cancel: None,
                copy_status: None,
            });
            return Ok(());
        }
        if let Action::Restore(serial) = action {
            self.restore_search(serial);
            return Ok(());
        }
        let busy = self.busy();
        let Some(panel) = self.search.as_mut() else {
            return Ok(());
        };
        match action {
            Action::Query(serial, query) if serial == panel.serial && !busy => {
                if panel.query != query {
                    panel.query = query;
                    panel.result = None;
                    panel.copy_status = None;
                    self.status.clear();
                }
            }
            Action::Run(serial)
                if serial == panel.serial && !busy && valid_filename_query(&panel.query) =>
            {
                let op = Operation::Find {
                    path: panel.root.clone(),
                    query: panel.query.clone(),
                };
                self.dialog_serial = self.dialog_serial.wrapping_add(1);
                panel.serial = self.dialog_serial;
                let serial = panel.serial;
                panel.result = None;
                panel.copy_status = None;
                panel.page = 0;
                panel.cancel = Some(TransferCancel::default());
                self.status = "Searching folders…".into();
                if let Err(error) = self.job(pane, session, op, Step::Search(serial), ctx) {
                    self.finish_search(serial, Err(format!("{error:#}")));
                }
            }
            Action::Cancel(serial) if serial == panel.serial => {
                if let Some(cancel) = &panel.cancel {
                    cancel.cancel();
                    self.status = "Cancelling search…".into();
                }
            }
            Action::Page(serial, page) if serial == panel.serial && page < panel.page_count() => {
                panel.page = page;
            }
            Action::CopyPaths(serial, hit) if !busy => {
                panel.copy_status = match panel.paths_to_copy(serial, hit.as_ref()) {
                    Ok(Some((paths, count))) => Some(match w11term::clipboard::write(&paths) {
                        Ok(()) if count == 1 => "Copied path".to_owned(),
                        Ok(()) => format!("Copied {count} paths"),
                        Err(error) => format!("Could not copy paths. {error}"),
                    }),
                    Ok(None) => return Ok(()),
                    Err(error) => Some(error.to_string()),
                };
            }
            Action::Reveal(serial, hit)
                if serial == panel.serial
                    && !busy
                    && panel.result.as_ref().is_some_and(|result| {
                        result
                            .matches
                            .iter()
                            .any(|item| item.parent == hit.parent && item.name == hit.name)
                    }) =>
            {
                self.following = false;
                self.saved_search = self.search.take();
                self.browse(pane, session, hit.parent, None, ctx)?;
                self.reveal = Some(RevealTarget::Existing(hit.name));
            }
            Action::Back(serial) if serial == panel.serial && self.picking.is_none() => {
                self.close_search();
                self.status.clear();
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn restoring_search_keeps_the_query_and_page_and_rejects_old_actions() {
        let matches = (0..=RESULTS_PER_PAGE)
            .map(|index| FilenameMatch {
                parent: "/project".into(),
                name: format!("log-{index}.txt"),
                kind: Some(EntryKind::File),
            })
            .collect();
        let mut drawer = FileDrawer {
            open: true,
            dialog_serial: 7,
            saved_search: Some(Panel {
                serial: 7,
                root: "/project".into(),
                query: "log".into(),
                result: Some(FilenameSearch {
                    root: "/project".into(),
                    matches,
                    scanned_directories: 1,
                    scanned_entries: 30,
                    skipped_directories: 0,
                    truncated: true,
                    cancelled: false,
                }),
                page: 1,
                cancel: None,
                copy_status: None,
            }),
            ..FileDrawer::default()
        };
        drawer.restore_search(6);
        assert!(drawer.search.is_none());
        drawer.restore_search(7);
        let panel = drawer.search.take().unwrap();
        assert_eq!(panel.page, 1);
        assert_eq!(panel.query, "log");
        assert_eq!(panel.serial, 8);
        let (paths, count) = panel.paths_to_copy(8, None).unwrap().unwrap();
        assert_eq!(count, RESULTS_PER_PAGE + 1);
        assert_eq!(paths.lines().next(), Some("/project/log-0.txt"));
        assert_eq!(paths.lines().last(), Some("/project/log-25.txt"));
        assert!(drawer.status.contains("Search limit reached"));
        assert!(drawer.saved_search.is_none());
        drawer.saved_search = Some(panel);
        drawer.close();
        assert!(drawer.saved_search.is_none());
    }

    #[test]
    fn search_download_requires_the_current_result_and_downloadable_type() {
        let file = FilenameMatch {
            parent: "/project/logs".into(),
            name: "build log.txt".into(),
            kind: Some(EntryKind::File),
        };
        let folder = FilenameMatch {
            name: "logs archive".into(),
            kind: Some(EntryKind::Directory),
            ..file.clone()
        };
        let link = FilenameMatch {
            name: "log link".into(),
            kind: Some(EntryKind::Symlink),
            ..file.clone()
        };
        let drawer = FileDrawer {
            search: Some(Panel {
                serial: 2,
                root: "/project".into(),
                query: "log".into(),
                result: Some(FilenameSearch {
                    root: "/project".into(),
                    matches: vec![file.clone(), folder.clone(), link.clone()],
                    scanned_directories: 2,
                    scanned_entries: 4,
                    skipped_directories: 0,
                    truncated: false,
                    cancelled: false,
                }),
                page: 0,
                cancel: None,
                copy_status: None,
            }),
            ..FileDrawer::default()
        };
        assert_eq!(
            drawer
                .search_download_target(2, &file, false)
                .unwrap()
                .remote,
            "/project/logs/build log.txt"
        );
        let target = drawer.search_download_target(2, &folder, false).unwrap();
        let (operation, _) = target
            .operation(r"C:\Downloads".into(), false, true, None)
            .unwrap();
        assert!(
            matches!(operation, Operation::GetDir { remote, local, preserve_mtime: true }
            if remote == "/project/logs/logs archive" && local == std::path::Path::new(r"C:\Downloads\logs archive"))
        );
        assert!(drawer.search_download_target(1, &file, false).is_err());
        assert!(drawer.search_download_target(2, &link, false).is_err());
        let forged = FilenameMatch {
            kind: Some(EntryKind::Directory),
            ..file
        };
        assert!(drawer.search_download_target(2, &forged, false).is_err());
    }

    #[test]
    fn search_properties_and_verified_download_follow_the_exact_result() {
        let hit = FilenameMatch {
            parent: "/project/logs".into(),
            name: "build log.txt".into(),
            kind: None,
        };
        let path = "/project/logs/build log.txt";
        let target = FileTarget::Search(2, hit.clone());
        let mut drawer = FileDrawer {
            open: true,
            generation: 1,
            directory: Some("/project".into()),
            search: Some(Panel {
                serial: 2,
                root: "/project".into(),
                query: "log".into(),
                result: Some(FilenameSearch {
                    root: "/project".into(),
                    matches: vec![hit.clone()],
                    scanned_directories: 2,
                    scanned_entries: 3,
                    skipped_directories: 0,
                    truncated: false,
                    cancelled: false,
                }),
                page: 0,
                cancel: None,
                copy_status: None,
            }),
            ..FileDrawer::default()
        };
        assert!(drawer.search_shell_origin(14, 2, &hit).is_some());
        assert!(drawer.download_target(&target, false).is_err());
        let (serial, op) = drawer
            .begin_preview(target.clone(), PreviewKind::Properties)
            .unwrap();
        assert!(matches!(op, Operation::EntryProperties { path: remote } if remote == path));
        let metadata = json!({"attrs": {"permissions": 0o100600}});
        drawer.finish_preview(serial, Ok(metadata.clone()));
        let (checksum, op) = drawer
            .begin_preview(target.clone(), PreviewKind::Checksum(serial))
            .unwrap();
        assert!(matches!(op, Operation::Sha256 { path: remote, .. } if remote == path));
        let digest = "f1".repeat(32);
        drawer.finish_preview(checksum, Ok(json!({"sha256": digest})));
        drawer.verify_checksum(checksum).unwrap();
        let serial = drawer.dialog.as_ref().unwrap().serial;
        let (dialog, hash) = drawer
            .finish_edit(serial, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        let EditAction::VerifyDownload(selected) = dialog.action else {
            panic!("Expected a verified download from the search results");
        };
        let (op, _) = drawer
            .download_operation(
                &selected,
                r"C:\Downloads\build log.txt".into(),
                Some(DownloadVerification {
                    sha256: hash,
                    resume: true,
                }),
            )
            .unwrap();
        assert!(
            matches!(op, Operation::Get { remote, expected_sha256: Some(hash), resume: true, .. }
            if remote == path && hash == digest)
        );
        assert_eq!(drawer.directory.as_deref(), Some("/project"));
        assert_eq!(drawer.search.as_ref().unwrap().query, "log");

        let (serial, _) = drawer
            .begin_preview(target.clone(), PreviewKind::Properties)
            .unwrap();
        drawer.search.as_mut().unwrap().serial += 1;
        drawer.finish_preview(serial, Ok(metadata));
        assert!(!drawer.entry_dialog_open());
        assert!(drawer.download_target(&target, true).is_err());
        assert!(drawer.search_shell_origin(14, 2, &hit).is_none());
    }

    #[test]
    fn search_copy_keeps_literal_paths_and_rejects_stale_or_ambiguous_lists() {
        let file = FilenameMatch {
            parent: "/project/雪 files".into(),
            name: "Résumé [draft] $1.txt".into(),
            kind: Some(EntryKind::File),
        };
        let link = FilenameMatch {
            parent: "/".into(),
            name: "snap link".into(),
            kind: Some(EntryKind::Symlink),
        };
        let mut panel = Panel {
            serial: 4,
            root: "/".into(),
            query: "snap".into(),
            result: Some(FilenameSearch {
                root: "/".into(),
                matches: vec![link, file.clone()],
                scanned_directories: 2,
                scanned_entries: 2,
                skipped_directories: 0,
                truncated: false,
                cancelled: true,
            }),
            page: 0,
            cancel: None,
            copy_status: None,
        };
        let path = "/project/雪 files/Résumé [draft] $1.txt";
        assert_eq!(
            panel.paths_to_copy(4, Some(&file)).unwrap(),
            Some((path.into(), 1))
        );
        assert_eq!(
            panel.paths_to_copy(4, None).unwrap(),
            Some((format!("/snap link\r\n{path}"), 2))
        );
        assert!(panel.paths_to_copy(3, None).unwrap().is_none());
        let updated = &mut panel.result.as_mut().unwrap().matches[1];
        updated.parent = "/line\nfeed".into();
        let updated = updated.clone();
        assert!(panel.paths_to_copy(4, Some(&file)).unwrap().is_none());
        assert!(panel.paths_to_copy(4, None).is_err());
        assert!(
            panel
                .paths_to_copy(4, Some(&updated))
                .unwrap()
                .unwrap()
                .0
                .contains('\n')
        );
    }
}
