use super::*;
use w11term::sftp::{
    FileSizeRange, FileTextMatch, FilenameFilter, FilenameSearch, FilenameSort as ResultSort,
    SearchDownload, valid_filename_query,
};

pub mod saved;

const RESULTS_PER_PAGE: usize = 25;
const RESULT_TYPES: [(Option<EntryKind>, &str); 4] = [
    (None, "All types"),
    (Some(EntryKind::File), "Files"),
    (Some(EntryKind::Directory), "Folders"),
    (Some(EntryKind::Symlink), "Symbolic links"),
];
const RESULT_SORTS: [(ResultSort, &str); 4] = [
    (ResultSort::Folder, "Sort by folder"),
    (ResultSort::Name, "Sort by filename"),
    (ResultSort::Newest, "Newest first"),
    (ResultSort::Largest, "Largest first"),
];

#[derive(Clone)]
pub enum Action {
    Show(u64),
    Restore(u64),
    FocusQuery(u64, u8),
    Query(u64, String),
    SetOption(u64, MatchOption),
    ShowAdvanced(u64, bool),
    Run(u64),
    Cancel(u64),
    Sort(u64, ResultSort),
    Page(u64, usize),
    SelectDownloads(u64),
    MarkDownload(u64, usize, bool),
    BulkMarks(u64, SelectionChange),
    StartFolderBatch(u64, FolderBatchKind),
    Reveal(u64, FilenameMatch),
    Rename(u64, FilenameMatch),
    Delete(u64, FilenameMatch),
    CopyPaths(u64, Option<FilenameMatch>),
    Saved(u64, saved::Action),
    Back(u64),
}

#[derive(Clone)]
pub enum MatchOption {
    Regex(bool),
    MatchCase(bool),
    MatchPath(bool),
    Kind(Option<EntryKind>),
    ExcludeDirs(String),
    MinimumSize(String),
    MaximumSize(String),
    ModifiedFrom(String),
    ModifiedThrough(String),
    ContentQuery(String),
}

#[derive(Default)]
struct SizeFields {
    minimum: String,
    maximum: String,
}

impl SizeFields {
    fn apply(&self, filter: &FilenameFilter) -> Result<FilenameFilter> {
        let mut filter = filter.clone();
        filter.file_size = FileSizeRange::parse(&self.minimum, &self.maximum)?;
        filter.file_size.validate(filter.kind)?;
        Ok(filter)
    }
}

impl From<FileSizeRange> for SizeFields {
    fn from(range: FileSizeRange) -> Self {
        let text = |bound: Option<u64>| bound.map_or_else(String::new, |bytes| bytes.to_string());
        Self {
            minimum: text(range.minimum),
            maximum: text(range.maximum),
        }
    }
}

pub(super) struct Panel {
    pub(super) serial: u64,
    root: String,
    query: String,
    input: ElementRef<TextBox>,
    filter: FilenameFilter,
    size: SizeFields,
    advanced_open: bool,
    sort: ResultSort,
    saved: Option<saved::List>,
    result: Option<FilenameSearch>,
    page: usize,
    pub(super) cancel: Option<TransferCancel>,
    copy_status: Option<String>,
    marked_downloads: Option<BTreeSet<usize>>,
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
    match result.excluded_directories {
        0 => {}
        1 => status.push_str(". 1 folder excluded"),
        count => {
            use std::fmt::Write;
            let _ = write!(status, ". {count} folders excluded");
        }
    }
    if let Some(scan) = &result.content_scan {
        use std::fmt::Write;
        let suffix = if scan.inspected == 1 { "" } else { "s" };
        let _ = write!(status, ". {} text file{suffix} inspected", scan.inspected);
        if scan.skipped != 0 {
            let _ = write!(status, ". {} skipped", scan.skipped);
        }
    }
    status
}

fn within<'a>(path: &'a str, ancestor: &str) -> Option<&'a str> {
    path.strip_prefix(ancestor)
        .filter(|suffix| suffix.is_empty() || suffix.starts_with('/'))
}

fn markable_result(hit: &FilenameMatch) -> bool {
    matches!(hit.kind, Some(EntryKind::File | EntryKind::Directory))
}

impl Panel {
    pub(super) fn match_file_text_lines(&self, text: &str) -> Result<Vec<FileTextMatch>> {
        self.filter.match_file_text_lines(text)
    }

    fn advanced_summary(&self) -> Option<String> {
        let parts: Vec<_> = [
            (
                "Exclude folders",
                self.filter.exclude_dir_regex.as_str(),
                "",
            ),
            ("Min size", self.size.minimum.trim(), ""),
            ("Max size", self.size.maximum.trim(), ""),
            (
                "Modified from",
                self.filter.modified_dates.from.trim(),
                " UTC",
            ),
            (
                "Modified through",
                self.filter.modified_dates.through.trim(),
                " UTC",
            ),
        ]
        .into_iter()
        .filter(|(_, value, _)| !value.is_empty())
        .map(|(label, value, suffix)| format!("{label} {value}{suffix}"))
        .collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    fn query_optional(&self) -> bool {
        self.filter.kind.is_some()
            || !self.filter.exclude_dir_regex.is_empty()
            || !self.size.minimum.trim().is_empty()
            || !self.size.maximum.trim().is_empty()
            || !self.filter.modified_dates.is_unrestricted()
            || !self.filter.content_query.is_empty()
    }

    fn can_search(&self) -> bool {
        valid_filename_query(&self.query) || (self.query.is_empty() && self.query_optional())
    }

    fn clear_results(&mut self) {
        self.result = None;
        self.page = 0;
        self.copy_status = None;
        self.marked_downloads = None;
    }

    fn install_result(&mut self, mut result: FilenameSearch) -> usize {
        self.sort.apply(&mut result.matches);
        let previous = self.result.as_ref().filter(|old| old.root == result.root);
        if previous.is_none() {
            self.page = 0;
        }
        let mut cleared = 0;
        if let Some(marked) = &mut self.marked_downloads {
            let old_count = marked.len();
            let wanted: Vec<_> = previous
                .into_iter()
                .flat_map(|old| marked.iter().filter_map(|&index| old.matches.get(index)))
                .collect();
            *marked = result
                .matches
                .iter()
                .enumerate()
                .filter_map(|(index, hit)| {
                    wanted
                        .iter()
                        .any(|old| {
                            old.parent == hit.parent && old.name == hit.name && old.kind == hit.kind
                        })
                        .then_some(index)
                })
                .collect();
            cleared = old_count.saturating_sub(marked.len());
        }
        self.result = Some(result);
        self.page = self.page.min(self.page_count().saturating_sub(1));
        cleared
    }

    fn search_download_selection(&self, serial: u64) -> Result<SearchDownload> {
        let (result, hits) = self.marked_hits(serial)?;
        SearchDownload::new(&result.root, hits)
    }

    fn marked_hits(&self, serial: u64) -> Result<(&FilenameSearch, Vec<&FilenameMatch>)> {
        let result = self
            .result
            .as_ref()
            .filter(|_| serial == self.serial)
            .context("The search results changed. Mark the files again.")?;
        let marked = self
            .marked_downloads
            .as_ref()
            .context("Mark at least one search result")?;
        let hits = marked
            .iter()
            .map(|&index| result.matches.get(index))
            .collect::<Option<Vec<_>>>()
            .context("The search results changed. Mark the files again.")?;
        ensure!(!hits.is_empty(), "Mark at least one search result");
        Ok((result, hits))
    }

    fn mark_download(&mut self, index: usize, checked: bool) {
        let hit = self
            .result
            .as_ref()
            .and_then(|result| result.matches.get(index));
        if hit.is_some_and(markable_result)
            && let Some(marked) = &mut self.marked_downloads
        {
            if checked {
                marked.insert(index);
            } else {
                marked.remove(&index);
            }
        }
    }

    fn bulk_marks(&mut self, change: SelectionChange) {
        let Some(marked) = &mut self.marked_downloads else {
            return;
        };
        match change {
            SelectionChange::MarkAll => {
                if let Some(result) = &self.result {
                    marked.extend(
                        result
                            .matches
                            .iter()
                            .enumerate()
                            .filter_map(|(index, hit)| markable_result(hit).then_some(index)),
                    );
                }
            }
            SelectionChange::Clear => marked.clear(),
        }
    }

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
        layout: DrawerLayout,
        profile: Option<&str>,
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
        let type_filter = ComboBox::new()
            .vertical_alignment(VerticalAlignment::Top)
            .automation_name("Search result type")
            .is_enabled(ready)
            .items_source(RESULT_TYPES.map(|(_, label)| label))
            .selected_index(
                RESULT_TYPES
                    .iter()
                    .position(|(kind, _)| *kind == self.filter.kind),
            )
            .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                index.and_then(|i| RESULT_TYPES.get(i)).map_or(
                    Message::ControlIdle,
                    |&(kind, _)| {
                        Message::Files(FileAction::Search(
                            pane,
                            generation,
                            Action::SetOption(serial, MatchOption::Kind(kind)),
                        ))
                    },
                )
            }));
        let input_label = if self.filter.match_path {
            "Relative path contains"
        } else {
            "Filename contains"
        };
        let prompt = match (self.filter.match_path, self.filter.regex) {
            (true, true) => "Relative path regex",
            (false, true) => "Filename regex",
            (_, false) => input_label,
        };
        let query = TextBox::new()
            .min_width(0.0)
            .automation_name(input_label)
            .element_ref(&self.input)
            .placeholder_text(if self.query_optional() {
                format!("{prompt} (optional)")
            } else {
                prompt.into()
            })
            .is_enabled(ready)
            .text(&self.query)
            .on_text_changed(ctx.callback(move |text| {
                Message::Files(FileAction::Search(
                    pane,
                    generation,
                    Action::Query(serial, text),
                ))
            }));
        let option_field =
            |label: &str, placeholder: &str, text: &str, option: fn(String) -> MatchOption| {
                TextBox::new()
                    .min_width(0.0)
                    .automation_name(label)
                    .placeholder_text(placeholder)
                    .is_enabled(ready)
                    .text(text)
                    .on_text_changed(ctx.callback(move |text| {
                        Message::Files(FileAction::Search(
                            pane,
                            generation,
                            Action::SetOption(serial, option(text)),
                        ))
                    }))
            };
        let contents = option_field(
            "File text contains",
            if self.filter.regex {
                "File text regex"
            } else {
                "File text contains"
            },
            &self.filter.content_query,
            MatchOption::ContentQuery,
        );
        let mut fields = vec![
            KeyedView::new("pattern", query),
            KeyedView::new("contents", contents),
        ];
        if self.advanced_open {
            let sizes = Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
                .column_spacing(6.0)
                .children((
                    option_field(
                        "Minimum file size",
                        "Min size",
                        &self.size.minimum,
                        MatchOption::MinimumSize,
                    ),
                    option_field(
                        "Maximum file size",
                        "Max size",
                        &self.size.maximum,
                        MatchOption::MaximumSize,
                    )
                    .grid_column(1),
                ));
            let dates = Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
                .column_spacing(6.0)
                .children((
                    option_field(
                        "Modified from (UTC)",
                        "Modified from",
                        &self.filter.modified_dates.from,
                        MatchOption::ModifiedFrom,
                    ),
                    option_field(
                        "Modified through (UTC)",
                        "Modified through",
                        &self.filter.modified_dates.through,
                        MatchOption::ModifiedThrough,
                    )
                    .grid_column(1),
                ));
            fields.extend([
                KeyedView::new(
                    "exclude",
                    option_field(
                        "Exclude folder paths (regex)",
                        "Exclude folder paths (regex)",
                        &self.filter.exclude_dir_regex,
                        MatchOption::ExcludeDirs,
                    ),
                ),
                KeyedView::new("sizes", sizes),
                KeyedView::new("dates", dates),
            ]);
        }
        let advanced_toggle = button(
            if self.advanced_open {
                "Hide filters"
            } else {
                "More filters"
            },
            Action::ShowAdvanced(serial, !self.advanced_open),
            true,
        );
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
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto])
                    .column_spacing(8.0)
                    .children((
                        StackPanel::new()
                            .automation_id(crate::input::file_search_tag(pane))
                            .spacing(6.0)
                            .keyed_children(fields),
                        StackPanel::new()
                            .grid_column(1)
                            .spacing(6.0)
                            .children((type_filter, advanced_toggle)),
                    )),
            ),
        ];
        let check = |label, checked, option: fn(bool) -> MatchOption| {
            CheckBox::new()
                .automation_name(label)
                .is_enabled(ready)
                .is_checked(checked)
                .on_is_checked_changed(ctx.callback(move |enabled| {
                    Message::Files(FileAction::Search(
                        pane,
                        generation,
                        Action::SetOption(serial, option(enabled)),
                    ))
                }))
                .content(label)
        };
        rows.push(KeyedView::new(
            "options",
            StackPanel::new()
                .orientation(if layout.content_width() >= 280.0 {
                    Orientation::Horizontal
                } else {
                    Orientation::Vertical
                })
                .spacing(8.0)
                .children((
                    check("Regex", self.filter.regex, MatchOption::Regex),
                    check("Match case", self.filter.match_case, MatchOption::MatchCase),
                    check("Match path", self.filter.match_path, MatchOption::MatchPath),
                )),
        ));
        if !self.advanced_open
            && let Some(summary) = self.advanced_summary()
        {
            rows.push(KeyedView::new(
                "advanced-summary",
                TextBlock::new()
                    .automation_id("sftp-search-filters-summary")
                    .text(summary)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        let run = if let Some(cancel) = running {
            button(
                "Cancel search",
                Action::Cancel(serial),
                !cancel.is_cancelled(),
            )
        } else {
            button(
                if self.result.is_some() {
                    "Refresh results"
                } else {
                    "Search"
                },
                Action::Run(serial),
                ready && self.can_search(),
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
        if let Some(profile) = profile.filter(|_| generation != 0) {
            if let Some(list) = self.saved.as_ref().filter(|list| list.profile == profile) {
                rows.push(KeyedView::new(
                    "saved",
                    list.view(self, pane, generation, ready, ctx),
                ));
            } else {
                rows.push(KeyedView::new(
                    "saved-toggle",
                    button(
                        "Saved searches…",
                        Action::Saved(serial, saved::Action::Show),
                        ready,
                    ),
                ));
            }
        }
        if let Some(result) = self.result.as_ref()
            && result.matches.iter().any(markable_result)
        {
            let mode = if self.marked_downloads.is_some() {
                "Exit selection"
            } else {
                "Select results…"
            };
            let select = button(mode, Action::SelectDownloads(serial), ready);
            let mut actions = vec![KeyedView::new("select", select)];
            if let Some(marked) = &self.marked_downloads {
                let can_add = result
                    .matches
                    .iter()
                    .enumerate()
                    .any(|(index, hit)| markable_result(hit) && !marked.contains(&index));
                actions.push(KeyedView::new(
                    "bulk-marks",
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(6.0)
                        .children((
                            button(
                                "Mark all results",
                                Action::BulkMarks(serial, SelectionChange::MarkAll),
                                ready && can_add,
                            ),
                            button(
                                "Clear marks",
                                Action::BulkMarks(serial, SelectionChange::Clear),
                                ready && !marked.is_empty(),
                            ),
                        )),
                ));
                actions.push(KeyedView::new(
                    "download",
                    Button::new()
                        .automation_name("Download marked results")
                        .is_enabled(ready && !marked.is_empty())
                        .on_click(ctx.callback(move |_| {
                            Message::Files(FileAction::Pick(
                                pane,
                                generation,
                                revision,
                                TransferPicker::DownloadSearch(serial),
                            ))
                        }))
                        .content(format!("Download marked ({})…", marked.len())),
                ));
                actions.push(KeyedView::new(
                    "move",
                    button(
                        "Move marked results…",
                        Action::StartFolderBatch(serial, FolderBatchKind::Move),
                        ready && !marked.is_empty(),
                    ),
                ));
                actions.push(KeyedView::new(
                    "copy",
                    button(
                        "Copy marked files…",
                        Action::StartFolderBatch(serial, FolderBatchKind::Copy),
                        ready
                            && !marked.is_empty()
                            && marked.iter().all(|&index| {
                                result
                                    .matches
                                    .get(index)
                                    .is_some_and(|hit| hit.kind == Some(EntryKind::File))
                            }),
                    ),
                ));
                actions.push(KeyedView::new(
                    "rename-marked",
                    button(
                        "Rename marked entries…",
                        Action::StartFolderBatch(serial, FolderBatchKind::Rename),
                        ready && !marked.is_empty(),
                    ),
                ));
            }
            rows.push(KeyedView::new(
                "marking",
                StackPanel::new().spacing(6.0).keyed_children(actions),
            ));
        }
        if self
            .result
            .as_ref()
            .is_some_and(|result| result.matches.len() > 1)
        {
            rows.push(KeyedView::new(
                "sort",
                ComboBox::new()
                    .automation_name("Sort search results")
                    .is_enabled(ready)
                    .items_source(RESULT_SORTS.map(|(_, label)| label))
                    .selected_index(
                        RESULT_SORTS
                            .iter()
                            .position(|(order, _)| *order == self.sort),
                    )
                    .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                        index.and_then(|i| RESULT_SORTS.get(i)).map_or(
                            Message::ControlIdle,
                            |&(sort, _)| {
                                Message::Files(FileAction::Search(
                                    pane,
                                    generation,
                                    Action::Sort(serial, sort),
                                ))
                            },
                        )
                    })),
            ));
        }
        if let Some(resume) = files.resume_draft_button(pane, ctx) {
            rows.push(KeyedView::new("resume-draft", resume));
        }
        if !files.status.is_empty() {
            rows.push(KeyedView::new(
                "status",
                TextBlock::new()
                    .automation_id("sftp-search-status")
                    .text(&files.status)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        }
        if let Some(button) =
            files.stop_folder_batch_button(pane, FolderBatchOrigin::Search(serial), ctx)
        {
            rows.push(KeyedView::new("stop-batch", button));
        }
        if let Some(progress) = files.search_progress(serial) {
            let entries = if progress.scanned_entries == 1 {
                "entry"
            } else {
                "entries"
            };
            let folders = if progress.scanned_directories == 1 {
                "folder"
            } else {
                "folders"
            };
            let matches = if progress.matches == 1 {
                "match"
            } else {
                "matches"
            };
            let counts = format!(
                "{} {entries} scanned · {} {folders} complete · {} {matches}",
                progress.scanned_entries, progress.scanned_directories, progress.matches
            );
            rows.push(KeyedView::new(
                "search-progress",
                TextBlock::new()
                    .automation_id("sftp-search-progress")
                    .text(format!("Scanning {}\n{counts}", progress.folder))
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
                        TextBlock::new()
                            .vertical_alignment(VerticalAlignment::Center)
                            .text(format!("{} / {}", self.page + 1, self.page_count())),
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
                let mut entry_actions = vec![KeyedView::new("reveal", reveal)];
                if let Some(hit) = &hit.content_match {
                    entry_actions.push(KeyedView::new(
                        "content-match",
                        TextBlock::new()
                            .text(format!("Line {} · {}", hit.line, hit.excerpt))
                            .text_wrapping(TextWrapping::Wrap),
                    ));
                }
                let sort_detail = match self.sort {
                    ResultSort::Newest => hit.mtime.map(|time| properties::timestamp(Some(time))),
                    ResultSort::Largest => hit.size.map(listing_size_label),
                    ResultSort::Folder | ResultSort::Name => None,
                };
                if let Some(detail) = sort_detail {
                    entry_actions.push(KeyedView::new(
                        "sort-detail",
                        TextBlock::new()
                            .text(detail)
                            .text_wrapping(TextWrapping::Wrap),
                    ));
                }
                if let Some(marked) = &self.marked_downloads
                    && markable_result(hit)
                {
                    entry_actions.push(KeyedView::new(
                        "mark",
                        CheckBox::new()
                            .automation_name(format!("Mark {path}"))
                            .is_checked(marked.contains(&index))
                            .is_enabled(ready)
                            .on_is_checked_changed(ctx.callback(move |checked| {
                                Message::Files(FileAction::Search(
                                    pane,
                                    generation,
                                    Action::MarkDownload(serial, index, checked),
                                ))
                            }))
                            .content("Mark result"),
                    ));
                }
                if hit.kind == Some(EntryKind::File) {
                    let preview = |key, label, kind| {
                        let hit = hit.clone();
                        KeyedView::new(
                            key,
                            Button::new()
                                .automation_name(format!("{label} {path}"))
                                .is_enabled(ready)
                                .on_click(ctx.callback(move |_| {
                                    Message::Files(FileAction::Preview(
                                        pane,
                                        generation,
                                        FileTarget::Search(serial, hit.clone()),
                                        kind,
                                    ))
                                }))
                                .content(format!("{label}…")),
                        )
                    };
                    if image_preview_name(&hit.name) {
                        entry_actions.push(preview("image", "Preview image", PreviewKind::Image));
                    }
                    let label = if hit.content_match.is_some() {
                        "Preview match"
                    } else {
                        "Preview text"
                    };
                    entry_actions.push(preview("preview", label, PreviewKind::Text));
                    entry_actions.push(preview("tail", "Tail text file", PreviewKind::Tail));
                }
                if shell_ready
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
                    entry_actions.push(KeyedView::new("shell", shell));
                }
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
                if hit.kind.is_some() {
                    actions.push(KeyedView::new(
                        "rename",
                        Button::new()
                            .automation_name(format!("Rename {path}"))
                            .is_enabled(ready)
                            .on_click({
                                let hit = hit.clone();
                                ctx.callback(move |_| {
                                    Message::Files(FileAction::Search(
                                        pane,
                                        generation,
                                        Action::Rename(serial, hit.clone()),
                                    ))
                                })
                            })
                            .content("Rename…"),
                    ));
                    if delete_path(&hit.parent, &hit.name).is_ok() {
                        actions.push(KeyedView::new(
                            "delete",
                            Button::new()
                                .automation_name(format!("Delete {path}"))
                                .is_enabled(ready)
                                .on_click({
                                    let hit = hit.clone();
                                    ctx.callback(move |_| {
                                        Message::Files(FileAction::Search(
                                            pane,
                                            generation,
                                            Action::Delete(serial, hit.clone()),
                                        ))
                                    })
                                })
                                .content("Delete…"),
                        ));
                    }
                    if entry_path(&hit.parent, &hit.name).is_ok() {
                        actions.push(KeyedView::new(
                            "move",
                            Button::new()
                                .automation_name(format!("Move to folder {path}"))
                                .is_enabled(ready)
                                .on_click({
                                    let hit = hit.clone();
                                    ctx.callback(move |_| {
                                        Message::Files(FileAction::MoveEntry(
                                            pane,
                                            generation,
                                            FileTarget::Search(serial, hit.clone()),
                                        ))
                                    })
                                })
                                .content("Move to folder…"),
                        ));
                    }
                }
                if hit.kind == Some(EntryKind::File) && entry_path(&hit.parent, &hit.name).is_ok() {
                    for (key, label, mode) in [
                        ("copy-file", "Copy to folder", CopyFileMode::ToFolder),
                        ("duplicate-file", "Duplicate as", CopyFileMode::Duplicate),
                    ] {
                        actions.push(KeyedView::new(
                            key,
                            Button::new()
                                .automation_name(format!("{label} {path}"))
                                .is_enabled(ready)
                                .on_click({
                                    let hit = hit.clone();
                                    ctx.callback(move |_| {
                                        Message::Files(FileAction::CopyEntry(
                                            pane,
                                            generation,
                                            FileTarget::Search(serial, hit.clone()),
                                            mode,
                                        ))
                                    })
                                })
                                .content(format!("{label}…")),
                        ));
                    }
                }
                KeyedView::new(
                    index,
                    Grid::new()
                        .columns([GridLength::Star(1.0), GridLength::Auto])
                        .column_spacing(6.0)
                        .children((
                            StackPanel::new().spacing(4.0).keyed_children(entry_actions),
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
                        ScrollViewer::new()
                            .automation_name("SFTP search actions")
                            .max_height(layout.actions_max_height)
                            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                            .content(StackPanel::new().spacing(6.0).keyed_children(rows)),
                        ScrollViewer::new()
                            .grid_row(1)
                            .automation_name("SFTP search results")
                            .content(StackPanel::new().spacing(4.0).keyed_children(matches)),
                    )),
            )
    }
}

impl FileDrawer {
    fn search_progress(&self, serial: u64) -> Option<SearchProgress> {
        let pending = self.pending.as_ref()?;
        if !matches!(pending.step, Step::Search(current, _) if current == serial) {
            return None;
        }
        let progress = pending.search_progress.as_ref()?.borrow();
        (!progress.folder.is_empty()).then(|| progress.clone())
    }

    pub(super) fn search_serial(&self) -> Option<u64> {
        self.search.as_ref().map(|panel| panel.serial)
    }

    pub(super) fn search_download_selection(&self, serial: u64) -> Result<SearchDownload> {
        ensure!(self.open, "Open the file drawer");
        self.search
            .as_ref()
            .context("The search results closed")?
            .search_download_selection(serial)
    }

    pub(super) fn marked_search_sources(
        &self,
        serial: u64,
    ) -> Result<(String, Vec<FilenameMatch>)> {
        ensure!(self.open, "Open the file drawer");
        let (result, hits) = self
            .search
            .as_ref()
            .context("The search results closed")?
            .marked_hits(serial)?;
        Ok((result.root.clone(), hits.into_iter().cloned().collect()))
    }

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

    fn restore_search(&mut self, serial: u64) -> Option<u64> {
        if self.busy() || !self.open || self.search.is_some() || self.bookmarks.is_some() {
            return None;
        }
        let mut panel = self.saved_search.take_if(|panel| panel.serial == serial)?;
        self.end_filter_edit();
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        panel.serial = self.dialog_serial;
        self.status = panel
            .result
            .as_ref()
            .map_or_else(String::new, result_status);
        self.search = Some(panel);
        Some(self.dialog_serial)
    }

    pub(super) fn finish_search(
        &mut self,
        serial: u64,
        result: Result<Value, String>,
        note: Option<&str>,
    ) {
        let Some(panel) = self.search.as_mut().filter(|panel| panel.serial == serial) else {
            return;
        };
        panel.cancel = None;
        panel.copy_status = None;
        // The old rows are visible but disabled during a refresh. Give the
        // completed scan a new serial so callbacks from those rows cannot act
        // on indices in the replacement results.
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        panel.serial = self.dialog_serial;
        self.status = match result.and_then(|value| {
            serde_json::from_value::<FilenameSearch>(value).map_err(|error| error.to_string())
        }) {
            Ok(result) if result.cancelled && panel.result.is_some() => {
                "Refresh cancelled. Showing previous results.".into()
            }
            Ok(result) => {
                let mut status = result_status(&result);
                let cleared = panel.install_result(result);
                if cleared > 0 {
                    use std::fmt::Write;
                    let ending = if cleared == 1 { "" } else { "s" };
                    let _ = write!(status, ". {cleared} selection mark{ending} cleared");
                }
                status
            }
            Err(error) if panel.result.is_some() => {
                format!("Refresh failed. Showing previous results. {error}")
            }
            Err(error) => error,
        };
        if let Some(note) = note {
            self.status = format!("{note}. {}", self.status);
        }
    }

    fn sort_search_results(&mut self, serial: u64, sort: ResultSort) {
        let Some(panel) = self
            .search
            .as_mut()
            .filter(|panel| panel.serial == serial && panel.sort != sort)
        else {
            return;
        };
        let Some(result) = panel
            .result
            .as_ref()
            .filter(|result| result.matches.len() > 1)
        else {
            return;
        };
        panel.sort = sort;
        panel.page = 0;
        panel.copy_status = None;
        panel.install_result(result.clone());
        // Rows are indexed within the current order. Expire their callbacks.
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        panel.serial = self.dialog_serial;
    }

    fn start_filename_search(
        &mut self,
        pane: u64,
        session: &Session,
        ctx: &ComponentContext<TerminalApp>,
        note: Option<String>,
    ) {
        let panel = self.search.as_mut().unwrap();
        let filter = match panel.size.apply(&panel.filter) {
            Ok(filter) => filter,
            Err(error) => {
                self.status = format!("{error:#}");
                return;
            }
        };
        let op = Operation::Find {
            path: panel.root.clone(),
            query: panel.query.clone(),
            regex: filter.regex,
            match_case: filter.match_case,
            match_path: filter.match_path,
            kind: filter.kind,
            exclude_dir_regex: filter.exclude_dir_regex,
            file_size: filter.file_size,
            modified_dates: filter.modified_dates,
            content_query: filter.content_query,
        };
        self.dialog_serial = self.dialog_serial.wrapping_add(1);
        panel.serial = self.dialog_serial;
        let serial = panel.serial;
        panel.copy_status = None;
        panel.cancel = Some(TransferCancel::default());
        self.status = match (&note, panel.result.is_some()) {
            (Some(note), _) => format!("{note}. Refreshing search results…"),
            (None, true) => "Refreshing search results…".into(),
            (None, false) => "Searching folders…".into(),
        };
        if let Err(error) = self.job(pane, session, op, Step::Search(serial, note.clone()), ctx) {
            self.finish_search(serial, Err(format!("{error:#}")), note.as_deref());
        }
    }

    fn begin_search_rename(&mut self, serial: u64, hit: FilenameMatch) -> Result<()> {
        ensure!(
            self.open && !self.busy() && hit.kind.is_some(),
            "Select a file, folder or link in the current search"
        );
        self.search_hit_path(serial, &hit)?;
        entry_path(&hit.parent, &hit.name)?;
        self.show_entry_dialog(
            Some(hit.parent.clone()),
            EditAction::RenameSearch(serial, hit),
        );
        Ok(())
    }

    fn begin_search_delete(&mut self, serial: u64, hit: FilenameMatch) -> Result<()> {
        ensure!(
            self.open && !self.busy(),
            "Select a file, folder or link in the current search"
        );
        self.search_hit_path(serial, &hit)?;
        let kind = hit
            .kind
            .context("The search result has no supported entry type")?;
        delete_path(&hit.parent, &hit.name)?;
        self.show_entry_dialog(
            Some(hit.parent.clone()),
            EditAction::Delete(FileTarget::Search(serial, hit), kind),
        );
        Ok(())
    }

    pub(super) fn search_change_completed(
        &mut self,
        pane: u64,
        session: &Session,
        serial: u64,
        hit: FilenameMatch,
        change: EntryChange,
        ctx: &ComponentContext<TerminalApp>,
    ) -> Result<()> {
        let status = change.completed();
        self.apply_search_change(serial, &hit, &change)?;
        self.refresh_changed_search(pane, session, serial, status, ctx);
        Ok(())
    }

    pub(super) fn apply_search_change(
        &mut self,
        serial: u64,
        hit: &FilenameMatch,
        change: &EntryChange,
    ) -> Result<()> {
        if let EntryChange::Copy {
            name,
            directory,
            size,
        } = change
        {
            self.accept_copied_entry(directory, name, *size);
            return Ok(());
        }
        let old_path = entry_path(&hit.parent, &hit.name)?;
        if let Some(directory) = self.directory.clone() {
            if directory == hit.parent {
                let mut entries = std::mem::take(&mut self.entries);
                match change {
                    EntryChange::Rename { to, .. } => {
                        entries.retain(|entry| &entry.name != to);
                        if let Some(entry) = entries.iter_mut().find(|entry| entry.name == hit.name)
                        {
                            entry.name = to.clone();
                        } else if let Some(kind) = hit.kind {
                            entries.push(Entry {
                                name: to.clone(),
                                kind: ListedKind::Known(kind),
                                mtime: None,
                                size: None,
                            });
                        }
                        self.sort_order.sort(&mut entries);
                    }
                    _ => entries.retain(|entry| entry.name != hit.name),
                }
                self.accept_listing(None, directory, entries);
            } else if let EntryChange::Move {
                directory: destination,
                ..
            } = change
                && directory == *destination
                && let Some(kind) = hit.kind
            {
                let mut entries = std::mem::take(&mut self.entries);
                entries.retain(|entry| entry.name != hit.name);
                entries.push(Entry {
                    name: hit.name.clone(),
                    kind: ListedKind::Known(kind),
                    mtime: hit.mtime,
                    size: hit.size,
                });
                self.sort_order.sort(&mut entries);
                self.accept_listing(None, directory, entries);
            } else if let Some(suffix) = within(&directory, &old_path) {
                let new_path = match change {
                    EntryChange::Rename { to, .. } => Some(entry_path(&hit.parent, to)?),
                    EntryChange::Move { directory, .. } => Some(entry_path(directory, &hit.name)?),
                    _ => None,
                };
                if let Some(new_path) = new_path {
                    let entries = std::mem::take(&mut self.entries);
                    self.accept_listing(None, format!("{new_path}{suffix}"), entries);
                } else {
                    // The cached listing was inside the removed path.
                    self.accept_listing(None, directory, Vec::new());
                    self.directory = None;
                }
            }
        }
        self.invalidate_search_entry(serial, hit)
    }

    pub(super) fn invalidate_search_entry(
        &mut self,
        serial: u64,
        hit: &FilenameMatch,
    ) -> Result<()> {
        let old_path = entry_path(&hit.parent, &hit.name)?;
        let Some(panel) = self.search.as_mut().filter(|panel| panel.serial == serial) else {
            return Ok(());
        };
        // Remove obsolete rows before refreshing, so an interrupted scan
        // cannot leave the old path (or old descendants) actionable.
        if let Some(mut previous) = panel.result.clone() {
            previous.matches.retain(|entry| {
                (entry.parent != hit.parent || entry.name != hit.name)
                    && within(&entry.parent, &old_path).is_none()
            });
            panel.install_result(previous);
        }
        Ok(())
    }

    pub(super) fn refresh_changed_search(
        &mut self,
        pane: u64,
        session: &Session,
        serial: u64,
        status: String,
        ctx: &ComponentContext<TerminalApp>,
    ) {
        if self.search_serial() == Some(serial) {
            self.start_filename_search(pane, session, ctx, Some(status));
        } else {
            self.status = status;
        }
    }

    pub(super) fn filename_search(
        &mut self,
        pane: u64,
        session: &Session,
        action: Action,
        profile: Option<(&str, &ProfileStore)>,
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
                input: ElementRef::new(),
                filter: FilenameFilter::default(),
                size: SizeFields::default(),
                advanced_open: false,
                sort: ResultSort::default(),
                saved: None,
                result: None,
                page: 0,
                cancel: None,
                copy_status: None,
                marked_downloads: None,
            });
            ctx.sender().send(Message::Files(FileAction::Search(
                pane,
                self.generation,
                Action::FocusQuery(self.dialog_serial, 0),
            )));
            return Ok(());
        }
        if let Action::Restore(serial) = action {
            if let Some(serial) = self.restore_search(serial) {
                ctx.sender().send(Message::Files(FileAction::Search(
                    pane,
                    self.generation,
                    Action::FocusQuery(serial, 0),
                )));
            }
            return Ok(());
        }
        if let Action::Saved(serial, action) = action {
            return self.saved_search_action(pane, session, serial, action, profile, ctx);
        }
        let busy = self.busy();
        let Some(panel) = self.search.as_mut() else {
            return Ok(());
        };
        match action {
            Action::FocusQuery(serial, attempt) if serial == panel.serial && !busy => {
                let next = FileAction::Search(
                    pane,
                    self.generation,
                    Action::FocusQuery(serial, attempt + 1),
                );
                focus_file_input(&panel.input, attempt, next, ctx);
            }
            Action::Query(serial, query) if serial == panel.serial && !busy => {
                if panel.query != query {
                    panel.query = query;
                    panel.clear_results();
                    self.status.clear();
                }
            }
            Action::SetOption(serial, option) if serial == panel.serial && !busy => {
                let previous = (
                    panel.filter.clone(),
                    panel.size.minimum.clone(),
                    panel.size.maximum.clone(),
                );
                match option {
                    MatchOption::Regex(enabled) => panel.filter.regex = enabled,
                    MatchOption::MatchCase(enabled) => panel.filter.match_case = enabled,
                    MatchOption::MatchPath(enabled) => panel.filter.match_path = enabled,
                    MatchOption::Kind(kind) => panel.filter.kind = kind,
                    MatchOption::ExcludeDirs(regex) => panel.filter.exclude_dir_regex = regex,
                    MatchOption::MinimumSize(value) => panel.size.minimum = value,
                    MatchOption::MaximumSize(value) => panel.size.maximum = value,
                    MatchOption::ModifiedFrom(value) => panel.filter.modified_dates.from = value,
                    MatchOption::ModifiedThrough(value) => {
                        panel.filter.modified_dates.through = value
                    }
                    MatchOption::ContentQuery(value) => panel.filter.content_query = value,
                }
                if (&panel.filter, &panel.size.minimum, &panel.size.maximum)
                    != (&previous.0, &previous.1, &previous.2)
                {
                    panel.clear_results();
                    self.status.clear();
                }
            }
            Action::ShowAdvanced(serial, expanded) if serial == panel.serial => {
                panel.advanced_open = expanded;
            }
            Action::Run(serial) if serial == panel.serial && !busy && panel.can_search() => {
                self.start_filename_search(pane, session, ctx, None);
            }
            Action::Cancel(serial) if serial == panel.serial => {
                if let Some(cancel) = &panel.cancel {
                    cancel.cancel();
                    self.status = "Cancelling search…".into();
                }
            }
            Action::Sort(serial, sort) if !busy => self.sort_search_results(serial, sort),
            Action::Page(serial, page) if serial == panel.serial && page < panel.page_count() => {
                panel.page = page;
            }
            Action::SelectDownloads(serial) if serial == panel.serial && !busy => {
                panel.marked_downloads = panel.marked_downloads.is_none().then(BTreeSet::new);
            }
            Action::MarkDownload(serial, index, checked) if serial == panel.serial && !busy => {
                panel.mark_download(index, checked);
            }
            Action::BulkMarks(serial, change) if serial == panel.serial && !busy => {
                panel.bulk_marks(change);
            }
            Action::StartFolderBatch(serial, kind) if !busy => {
                self.begin_folder_batch(kind, FolderBatchOrigin::Search(serial))?
            }
            Action::Rename(serial, hit) if !busy => self.begin_search_rename(serial, hit)?,
            Action::Delete(serial, hit) if !busy => self.begin_search_delete(serial, hit)?,
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

    fn search_result(root: &str, entries: &[(&str, EntryKind)]) -> FilenameSearch {
        FilenameSearch {
            root: root.into(),
            matches: entries
                .iter()
                .map(|(name, kind)| FilenameMatch {
                    parent: "/project/nested".into(),
                    name: (*name).into(),
                    kind: Some(*kind),
                    mtime: None,
                    size: None,
                    content_match: None,
                })
                .collect(),
            scanned_directories: 2,
            scanned_entries: entries.len(),
            skipped_directories: 0,
            excluded_directories: 0,
            content_scan: None,
            truncated: false,
            cancelled: false,
        }
    }

    fn search_drawer(result: FilenameSearch) -> FileDrawer {
        FileDrawer {
            open: true,
            generation: 1,
            directory: Some("/project".into()),
            dialog_serial: 5,
            search: Some(Panel {
                serial: 5,
                root: result.root.clone(),
                query: "name".into(),
                input: ElementRef::new(),
                filter: FilenameFilter::default(),
                size: SizeFields::default(),
                advanced_open: false,
                sort: ResultSort::default(),
                saved: None,
                result: Some(result),
                page: 0,
                cancel: None,
                copy_status: None,
                marked_downloads: None,
            }),
            ..FileDrawer::default()
        }
    }

    #[test]
    fn mark_all_results_spans_pages_and_skips_nontransferable_entries() {
        let names: Vec<_> = (0..RESULTS_PER_PAGE + 2)
            .map(|index| format!("{index:02}.txt"))
            .collect();
        let mut matches: Vec<_> = names
            .iter()
            .map(|name| (name.as_str(), EntryKind::File))
            .collect();
        matches.push(("subdir", EntryKind::Directory));
        matches.push(("link", EntryKind::Symlink));
        matches.push(("unknown", EntryKind::File));
        let mut result = search_result("/project", &matches);
        result.matches.last_mut().unwrap().kind = None;
        let mut drawer = search_drawer(result);
        let panel = drawer.search.as_mut().unwrap();
        panel.bulk_marks(SelectionChange::MarkAll);
        assert!(panel.marked_downloads.is_none());
        panel.marked_downloads = Some(BTreeSet::new());
        panel.bulk_marks(SelectionChange::MarkAll);
        let marked = panel.marked_downloads.as_ref().unwrap();
        assert_eq!(marked.len(), RESULTS_PER_PAGE + 3);
        assert!(marked.contains(&(RESULTS_PER_PAGE + 2)));
        assert!(!marked.contains(&(RESULTS_PER_PAGE + 3)));
        assert!(!marked.contains(&(RESULTS_PER_PAGE + 4)));
        panel.bulk_marks(SelectionChange::Clear);
        assert_eq!(panel.marked_downloads, Some(BTreeSet::new()));
        assert!(panel.result.is_some());
    }

    #[test]
    fn collapsed_advanced_summary_reflects_the_current_search_criteria() {
        let mut drawer = search_drawer(search_result("/project", &[]));
        let panel = drawer.search.as_mut().unwrap();
        assert!(!panel.advanced_open);
        assert_eq!(panel.advanced_summary(), None);
        panel.filter.exclude_dir_regex = "^generated/".into();
        panel.size.minimum = " 1 KiB ".into();
        panel.size.maximum = "0".into();
        panel.filter.modified_dates.from = "2025-01-01".into();
        panel.filter.modified_dates.through = "2025-01-31".into();
        assert_eq!(
            panel.advanced_summary().as_deref(),
            Some(
                "Exclude folders ^generated/ · Min size 1 KiB · Max size 0 · Modified from 2025-01-01 UTC · Modified through 2025-01-31 UTC"
            )
        );
    }

    #[test]
    fn search_rename_uses_the_hit_parent_and_rejects_an_expired_result() {
        let result = search_result("/project", &[("old name", EntryKind::File)]);
        let hit = result.matches[0].clone();
        let mut drawer = search_drawer(result);
        drawer.begin_search_rename(5, hit.clone()).unwrap();
        let dialog = drawer.dialog.as_mut().unwrap();
        assert_eq!(dialog.name, "old name");
        dialog.name = "replacement name".into();
        let serial = dialog.serial;
        let (_, path) = drawer
            .finish_edit(serial, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert_eq!(path, "/project/nested/replacement name");
        assert_eq!(drawer.directory.as_deref(), Some("/project"));
        drawer.begin_search_rename(5, hit).unwrap();
        let dialog = drawer.dialog.as_mut().unwrap();
        dialog.name = "replacement name".into();
        let serial = dialog.serial;
        drawer.search.as_mut().unwrap().serial += 1;
        assert!(
            drawer
                .finish_edit(serial, ContentDialogResult::Primary)
                .is_err()
        );
    }

    #[test]
    fn search_delete_confirms_the_exact_entry_and_rejects_a_changed_result() {
        let result = search_result("/project", &[("link name", EntryKind::Symlink)]);
        let hit = result.matches[0].clone();
        let mut drawer = search_drawer(result);
        drawer.begin_search_delete(5, hit.clone()).unwrap();
        let (dialog, path) = drawer
            .finish_edit(drawer.dialog_serial, ContentDialogResult::Primary)
            .unwrap()
            .unwrap();
        assert_eq!(path, "/project/nested/link name");
        assert!(matches!(
            dialog.action,
            EditAction::Delete(FileTarget::Search(5, matched), EntryKind::Symlink) if matched == hit
        ));

        drawer.begin_search_delete(5, hit).unwrap();
        drawer
            .search
            .as_mut()
            .unwrap()
            .result
            .as_mut()
            .unwrap()
            .matches[0]
            .kind = Some(EntryKind::Directory);
        assert!(
            drawer
                .finish_edit(drawer.dialog_serial, ContentDialogResult::Primary)
                .is_err()
        );
    }

    #[test]
    fn result_sort_changes_all_pages_and_remaps_marks_by_path_and_type() {
        use EntryKind::File;
        let mut result = search_result(
            "/project",
            &[
                ("a.log", File),
                ("z.log", File),
                ("b.log", File),
                ("a.log", File),
            ],
        );
        result.matches[0].parent = "/project/a".into();
        result.matches[0].mtime = Some(20);
        result.matches[0].size = Some(42);
        result.matches[1].parent = "/project/a".into();
        result.matches[2].parent = "/project/b".into();
        result.matches[2].mtime = Some(50);
        result.matches[2].size = Some(42);
        result.matches[3].parent = "/project/z".into();
        result.matches[3].mtime = Some(50);
        result.matches[3].size = Some(0);
        let mut drawer = search_drawer(result);
        drawer.search.as_mut().unwrap().marked_downloads = Some(BTreeSet::from([3]));

        for (sort, order) in [
            (ResultSort::Name, [0, 3, 2, 1]),
            (ResultSort::Newest, [2, 3, 0, 1]),
            (ResultSort::Largest, [0, 2, 3, 1]),
            (ResultSort::Folder, [0, 1, 2, 3]),
        ] {
            let serial = drawer.search_serial().unwrap();
            drawer.sort_search_results(serial, sort);
            let panel = drawer.search.as_ref().unwrap();
            let hits = panel.result.as_ref().unwrap();
            let paths = [
                "/project/a/a.log",
                "/project/a/z.log",
                "/project/b/b.log",
                "/project/z/a.log",
            ];
            assert_eq!(
                hits.matches
                    .iter()
                    .map(|hit| format!("{}/{}", hit.parent, hit.name))
                    .collect::<Vec<_>>(),
                order.map(|index| paths[index]),
            );
            assert_eq!(panel.serial, serial + 1);
            assert!(panel.search_download_selection(serial).is_err());
            let marked = panel.marked_downloads.as_ref().unwrap();
            assert_eq!(marked.len(), 1);
            assert_eq!(hits.matches[*marked.first().unwrap()].parent, "/project/z");
        }
    }

    #[test]
    fn refresh_remaps_marks_by_root_path_and_type_then_expires_old_callbacks() {
        use EntryKind::{Directory, File};
        let before = search_result(
            "/project",
            &[("keep", File), ("removed", File), ("changed", File)],
        );
        let mut drawer = FileDrawer {
            open: true,
            dialog_serial: 3,
            search: Some(Panel {
                serial: 3,
                root: "/project".into(),
                query: "test".into(),
                input: ElementRef::new(),
                filter: FilenameFilter::default(),
                size: SizeFields::default(),
                advanced_open: false,
                sort: ResultSort::default(),
                saved: None,
                result: Some(before),
                page: 1,
                cancel: Some(TransferCancel::default()),
                copy_status: None,
                marked_downloads: Some(BTreeSet::from([0, 1, 2])),
            }),
            ..FileDrawer::default()
        };
        let mut after = search_result(
            "/project",
            &[("added", File), ("changed", Directory), ("keep", File)],
        );
        after.matches[2].mtime = Some(1);
        after.matches[2].size = Some(12);
        drawer.finish_search(3, Ok(json!(after)), None);
        let panel = drawer.search.as_ref().unwrap();
        assert_eq!(panel.serial, 4);
        assert_eq!(panel.page, 0);
        assert_eq!(panel.marked_downloads, Some(BTreeSet::from([2])));
        assert!(panel.search_download_selection(3).is_err());
        assert!(panel.search_download_selection(4).is_ok());
        assert!(drawer.status.ends_with("2 selection marks cleared"));

        let mut cancelled = search_result("/project", &[]);
        cancelled.cancelled = true;
        drawer.finish_search(4, Ok(json!(cancelled)), None);
        assert_eq!(
            drawer.search.as_ref().unwrap().marked_downloads,
            Some(BTreeSet::from([2]))
        );
        assert!(drawer.status.contains("Refresh cancelled"));
        drawer.finish_search(5, Err("SSH request timed out".into()), None);
        assert_eq!(
            drawer.search.as_ref().unwrap().marked_downloads,
            Some(BTreeSet::from([2]))
        );
        assert!(drawer.status.contains("Refresh failed"));

        let moved_root = search_result("/project/nested", &[("keep", File)]);
        drawer.finish_search(6, Ok(json!(moved_root)), None);
        assert!(drawer.search_download_selection(7).is_err());
        assert!(drawer.status.ends_with("1 selection mark cleared"));
    }

    #[test]
    fn restoring_search_keeps_the_query_and_page_and_rejects_old_actions() {
        let matches = (0..=RESULTS_PER_PAGE)
            .map(|index| FilenameMatch {
                parent: "/project".into(),
                name: format!("log-{index}.txt"),
                kind: Some(EntryKind::File),
                mtime: None,
                size: None,
                content_match: None,
            })
            .collect();
        let mut drawer = FileDrawer {
            open: true,
            dialog_serial: 7,
            saved_search: Some(Panel {
                serial: 7,
                root: "/project".into(),
                query: "log".into(),
                input: ElementRef::new(),
                filter: FilenameFilter {
                    regex: true,
                    match_case: true,
                    match_path: false,
                    kind: Some(EntryKind::File),
                    exclude_dir_regex: String::new(),
                    file_size: FileSizeRange::default(),
                    modified_dates: Default::default(),
                    content_query: String::new(),
                },
                size: SizeFields::default(),
                advanced_open: false,
                sort: ResultSort::default(),
                saved: None,
                result: Some(FilenameSearch {
                    root: "/project".into(),
                    matches,
                    scanned_directories: 1,
                    scanned_entries: 30,
                    skipped_directories: 0,
                    excluded_directories: 0,
                    content_scan: None,
                    truncated: true,
                    cancelled: false,
                }),
                page: 1,
                cancel: None,
                copy_status: None,
                marked_downloads: Some(BTreeSet::from([0])),
            }),
            ..FileDrawer::default()
        };
        assert_eq!(drawer.restore_search(6), None);
        assert!(drawer.search.is_none());
        assert_eq!(drawer.restore_search(7), Some(8));
        let mut panel = drawer.search.take().unwrap();
        assert_eq!(panel.page, 1);
        assert_eq!(panel.query, "log");
        assert_eq!(panel.serial, 8);
        assert!(panel.filter.regex && panel.filter.match_case);
        assert_eq!(panel.filter.kind, Some(EntryKind::File));
        panel.mark_download(RESULTS_PER_PAGE, true);
        assert_eq!(
            panel.marked_downloads,
            Some(BTreeSet::from([0, RESULTS_PER_PAGE]))
        );
        assert!(panel.search_download_selection(7).is_err());
        assert_eq!(
            panel.search_download_selection(8).unwrap().root(),
            "/project"
        );
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
            mtime: None,
            size: None,
            content_match: None,
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
                input: ElementRef::new(),
                filter: FilenameFilter::default(),
                size: SizeFields::default(),
                advanced_open: false,
                sort: ResultSort::default(),
                saved: None,
                result: Some(FilenameSearch {
                    root: "/project".into(),
                    matches: vec![file.clone(), folder.clone(), link.clone()],
                    scanned_directories: 2,
                    scanned_entries: 4,
                    skipped_directories: 0,
                    excluded_directories: 0,
                    content_scan: None,
                    truncated: false,
                    cancelled: false,
                }),
                page: 0,
                cancel: None,
                copy_status: None,
                marked_downloads: None,
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
    fn search_text_editor_uses_the_current_hit_then_pins_the_draft_path() {
        let hit = FilenameMatch {
            parent: "/project/logs".into(),
            name: "settings.txt".into(),
            kind: Some(EntryKind::File),
            mtime: None,
            size: None,
            content_match: None,
        };
        let other = FilenameMatch {
            parent: "/project".into(),
            ..hit.clone()
        };
        let mut drawer = FileDrawer {
            open: true,
            generation: 3,
            directory: Some("/project".into()),
            search: Some(Panel {
                serial: 4,
                root: "/project".into(),
                query: "settings".into(),
                input: ElementRef::new(),
                filter: FilenameFilter::default(),
                size: SizeFields::default(),
                advanced_open: false,
                sort: ResultSort::default(),
                saved: None,
                result: Some(FilenameSearch {
                    root: "/project".into(),
                    matches: vec![other, hit.clone()],
                    scanned_directories: 2,
                    scanned_entries: 2,
                    skipped_directories: 0,
                    excluded_directories: 0,
                    content_scan: None,
                    truncated: false,
                    cancelled: false,
                }),
                page: 0,
                cancel: None,
                copy_status: None,
                marked_downloads: None,
            }),
            ..FileDrawer::default()
        };
        let path = "/project/logs/settings.txt";
        let (preview, operation) = drawer
            .begin_preview(FileTarget::Search(4, hit.clone()), PreviewKind::Text)
            .unwrap();
        assert!(matches!(operation, Operation::PreviewText { path: remote } if remote == path));
        drawer.finish_preview(preview, Ok(json!({"text": "original"})));

        drawer.search.as_mut().unwrap().serial = 5;
        drawer.start_editor(preview);
        assert!(drawer.editor.is_none());
        drawer.close_preview(preview);
        let (preview, _) = drawer
            .begin_preview(FileTarget::Search(5, hit), PreviewKind::Text)
            .unwrap();
        drawer.finish_preview(preview, Ok(json!({"text": "original"})));
        drawer.start_editor(preview);
        let serial = drawer.dialog_serial;
        drawer.editor_action(serial, editor::Action::Mounted);
        drawer.editor_action(serial, editor::Action::Text("edited".into()));
        let (save, operation) = drawer.editor_action(serial, editor::Action::Save).unwrap();
        assert!(
            matches!(operation, Operation::SaveText { path: remote, original: Some(original), text }
            if remote == path && original == "original" && text == "edited")
        );
        assert!(drawer.finish_save(save, Ok(json!({}))).is_none());
        assert_eq!(drawer.directory.as_deref(), Some("/project"));
        assert_eq!(drawer.search.unwrap().query, "settings");
    }

    #[test]
    fn search_properties_and_verified_download_follow_the_exact_result() {
        let hit = FilenameMatch {
            parent: "/project/logs".into(),
            name: "build log.txt".into(),
            kind: None,
            mtime: None,
            size: None,
            content_match: None,
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
                input: ElementRef::new(),
                filter: FilenameFilter::default(),
                size: SizeFields::default(),
                advanced_open: false,
                sort: ResultSort::default(),
                saved: None,
                result: Some(FilenameSearch {
                    root: "/project".into(),
                    matches: vec![hit.clone()],
                    scanned_directories: 2,
                    scanned_entries: 3,
                    skipped_directories: 0,
                    excluded_directories: 0,
                    content_scan: None,
                    truncated: false,
                    cancelled: false,
                }),
                page: 0,
                cancel: None,
                copy_status: None,
                marked_downloads: None,
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
            mtime: None,
            size: None,
            content_match: None,
        };
        let link = FilenameMatch {
            parent: "/".into(),
            name: "snap link".into(),
            kind: Some(EntryKind::Symlink),
            mtime: None,
            size: None,
            content_match: None,
        };
        let mut panel = Panel {
            serial: 4,
            root: "/".into(),
            query: "snap".into(),
            input: ElementRef::new(),
            filter: FilenameFilter::default(),
            size: SizeFields::default(),
            advanced_open: false,
            sort: ResultSort::default(),
            saved: None,
            result: Some(FilenameSearch {
                root: "/".into(),
                matches: vec![link, file.clone()],
                scanned_directories: 2,
                scanned_entries: 2,
                skipped_directories: 0,
                excluded_directories: 0,
                content_scan: None,
                truncated: false,
                cancelled: true,
            }),
            page: 0,
            cancel: None,
            copy_status: None,
            marked_downloads: None,
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
