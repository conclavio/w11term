use super::*;
use w11term::sftp::{FolderChange, FolderEntry};

const PAGE_SIZE: usize = 40;
const MAX_QUERY_CHARS: usize = 4096;
const FILTERS: [Filter; 7] = [
    Filter::Differences(None),
    Filter::Differences(Some(FolderChange::ReferenceOnly)),
    Filter::Differences(Some(FolderChange::CurrentOnly)),
    Filter::Differences(Some(FolderChange::Type)),
    Filter::Differences(Some(FolderChange::Metadata)),
    Filter::Differences(Some(FolderChange::Unknown)),
    Filter::MatchingFiles,
];

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Filter {
    Differences(Option<FolderChange>),
    MatchingFiles,
}

impl Default for Filter {
    fn default() -> Self {
        Self::Differences(None)
    }
}

impl Filter {
    fn label(self) -> &'static str {
        match self {
            Self::Differences(change) => change_label(change),
            Self::MatchingFiles => "Files with matching metadata",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Row {
    Difference(usize),
    MatchingFile(usize),
}

impl Row {
    pub(super) fn get(self, report: &FolderComparison) -> Option<RowDetails<'_>> {
        match self {
            Self::Difference(index) => report.differences.get(index).map(|row| RowDetails {
                path: &row.path,
                change: Some(row.change),
                reference: row.reference,
                current: row.current,
            }),
            Self::MatchingFile(index) => report.matching_files.get(index).map(|row| RowDetails {
                path: &row.path,
                change: None,
                reference: Some(row.reference),
                current: Some(row.current),
            }),
        }
    }
}

pub(super) struct RowDetails<'a> {
    pub(super) path: &'a str,
    change: Option<FolderChange>,
    reference: Option<FolderEntry>,
    pub(super) current: Option<FolderEntry>,
}

impl RowDetails<'_> {
    pub(super) fn can_verify(&self) -> bool {
        self.reference.is_some_and(FolderEntry::is_file)
            && self.current.is_some_and(FolderEntry::is_file)
    }
}

#[derive(Clone)]
pub enum Action {
    Query(String),
    Filter(Filter),
    Previous,
    Next,
}

#[derive(Default)]
pub(super) struct Browser {
    query: String,
    filter: Filter,
    page: usize,
    matches: Vec<Row>,
    viewport_revision: u64,
}

impl Browser {
    pub(super) fn update(&mut self, report: &FolderComparison, action: Action) {
        match action {
            Action::Query(query) if self.query != query => {
                self.query = query;
                self.index(report);
            }
            Action::Filter(filter) if self.filter != filter => {
                self.filter = filter;
                self.index(report);
            }
            Action::Previous if self.page > 0 => {
                self.page -= 1;
                self.viewport_revision = self.viewport_revision.wrapping_add(1);
            }
            Action::Next if (self.page + 1) * PAGE_SIZE < self.matches.len() => {
                self.page += 1;
                self.viewport_revision = self.viewport_revision.wrapping_add(1);
            }
            _ => {}
        }
    }

    pub(super) fn index(&mut self, report: &FolderComparison) {
        self.page = 0;
        self.viewport_revision = self.viewport_revision.wrapping_add(1);
        self.matches.clear();
        // The walker bounds full paths to 4096 bytes. A longer query in Unicode
        // scalars cannot match any path, even with case folding.
        if self.query.chars().nth(MAX_QUERY_CHARS).is_some() {
            return;
        }
        let pattern = if self.query.is_empty() {
            None
        } else {
            let Ok(pattern) = regex::RegexBuilder::new(&regex::escape(&self.query))
                .case_insensitive(true)
                .build()
            else {
                return;
            };
            Some(pattern)
        };
        let matches_path = |path: &str| {
            pattern
                .as_ref()
                .is_none_or(|pattern| pattern.is_match(path))
        };
        match self.filter {
            Filter::Differences(kind) => {
                self.matches
                    .extend(
                        report
                            .differences
                            .iter()
                            .enumerate()
                            .filter_map(|(index, row)| {
                                (kind.is_none_or(|kind| row.change == kind)
                                    && matches_path(&row.path))
                                .then_some(Row::Difference(index))
                            }),
                    )
            }
            Filter::MatchingFiles => {
                self.matches
                    .extend(
                        report
                            .matching_files
                            .iter()
                            .enumerate()
                            .filter_map(|(index, row)| {
                                matches_path(&row.path).then_some(Row::MatchingFile(index))
                            }),
                    )
            }
        }
    }

    pub(super) fn rows(
        &self,
        report: &FolderComparison,
        context: &ViewContext<TerminalApp>,
        message: impl Fn(super::Action) -> Message + Copy + 'static,
        revision: u64,
        reveal_enabled: bool,
    ) -> Vec<KeyedView> {
        let mut rows = vec![
            KeyedView::new(
                "reference",
                TextBlock::new()
                    .automation_id("sftp-folder-comparison-reference")
                    .text(format!(
                        "{}  {}",
                        reference_label(report.reference_origin, true),
                        report.reference_root,
                    ))
                    .text_wrapping(TextWrapping::Wrap),
            ),
            KeyedView::new(
                "scope",
                TextBlock::new()
                    .text("Compares paths, entry types, and regular-file sizes and modification times. File contents and link targets are unchecked.")
                    .text_wrapping(TextWrapping::Wrap),
            ),
            KeyedView::new("totals", totals(report)),
        ];
        if report.differences.is_empty() && report.matching_files.is_empty() {
            rows.push(KeyedView::new(
                "empty",
                TextBlock::new().text("No differences in the reported metadata."),
            ));
            return rows;
        }
        let dispatch = move |action| message(super::Action::Browse(action));
        rows.push(KeyedView::new(
            "filters",
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    TextBox::new()
                        .min_width(0.0)
                        .automation_name("Filter relative paths")
                        .placeholder_text("Filter relative paths")
                        .text(&self.query)
                        .on_text_changed(
                            context.callback(move |text| dispatch(Action::Query(text))),
                        ),
                    ComboBox::new()
                        .grid_column(1)
                        .automation_name("Folder comparison filter")
                        .items_source(FILTERS.map(Filter::label))
                        .selected_index(FILTERS.iter().position(|&filter| filter == self.filter))
                        .on_selection_changed(context.callback(move |index: Option<usize>| {
                            index
                                .and_then(|i| FILTERS.get(i))
                                .map_or(Message::ControlIdle, |&filter| {
                                    dispatch(Action::Filter(filter))
                                })
                        })),
                )),
        ));
        let matching_files = self.filter == Filter::MatchingFiles;
        if self.matches.is_empty() {
            rows.push(KeyedView::new(
                "empty",
                TextBlock::new().text(if matching_files {
                    "No files with matching metadata fit the path filter."
                } else if report.differences.is_empty() {
                    "No differences in the reported metadata."
                } else {
                    "No differences match these filters."
                }),
            ));
            return rows;
        }
        let start = self.page * PAGE_SIZE;
        let total = self.matches.len();
        let end = (start + PAGE_SIZE).min(total);
        let (singular, plural, previous, next, list_name) = if matching_files {
            (
                "file with matching metadata",
                "files with matching metadata",
                "Previous matching files",
                "Next matching files",
                "Files with matching metadata",
            )
        } else {
            (
                "difference",
                "differences",
                "Previous differences",
                "Next differences",
                "Folder differences",
            )
        };
        let status = match total {
            1 => format!("1 {singular}"),
            _ if end == start + 1 => format!("Showing {end} of {total} {plural}"),
            _ => format!("Showing {}–{end} of {total} {plural}", start + 1),
        };
        rows.push(KeyedView::new(
            "range",
            TextBlock::new()
                .automation_id("sftp-comparison-page")
                .text(status),
        ));
        rows.push(KeyedView::new(
            format!("differences-{}", self.viewport_revision),
            ScrollViewer::new()
                .automation_name(list_name)
                .max_height(154.0)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .content(StackPanel::new().spacing(6.0).keyed_children(
                    self.matches[start..end].iter().filter_map(|&row| {
                        let item = row.get(report)?;
                        let reveal = item.current.map(|_| {
                            Button::new()
                                .automation_name(format!("Show remote entry {}", item.path))
                                .is_enabled(reveal_enabled)
                                .on_click(context.callback(move |_| {
                                    message(super::Action::Reveal { revision, row })
                                }))
                                .content("Show in drawer")
                        });
                        let verify = item.can_verify().then(|| {
                            Button::new()
                                .automation_name(format!("Verify contents of {}", item.path))
                                .is_enabled(reveal_enabled)
                                .on_click(context.callback(move |_| {
                                    message(super::Action::Verify(super::verify::Action::Row {
                                        revision,
                                        row,
                                    }))
                                }))
                                .content("Verify contents")
                        });
                        Some(result_row(item, reveal, verify))
                    }),
                )),
        ));
        if total > PAGE_SIZE {
            rows.push(KeyedView::new(
                "pagination",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        Button::new()
                            .is_enabled(start > 0)
                            .on_click(context.callback(move |_| dispatch(Action::Previous)))
                            .content(previous),
                        Button::new()
                            .is_enabled(end < total)
                            .on_click(context.callback(move |_| dispatch(Action::Next)))
                            .content(next),
                    )),
            ));
        }
        rows
    }
}

fn totals(report: &FolderComparison) -> View {
    use FolderChange::*;
    let overview = [
        (
            "reference-only",
            change_label(Some(ReferenceOnly)),
            report.reference_only,
        ),
        (
            "current-only",
            change_label(Some(CurrentOnly)),
            report.current_only,
        ),
        ("type", change_label(Some(Type)), report.types_changed),
        (
            "metadata",
            change_label(Some(Metadata)),
            report.metadata_changed,
        ),
        ("unknown", change_label(Some(Unknown)), report.unknown),
        ("matching", "Matching entries", report.matching),
    ];
    StackPanel::new()
        .spacing(3.0)
        .keyed_children(overview.chunks(2).map(|pair| {
            KeyedView::new(
                pair[0].0,
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
                    .column_spacing(8.0)
                    .keyed_children(pair.iter().enumerate().map(
                        |(column, &(key, label, count))| {
                            KeyedView::new(
                                key,
                                TextBlock::new()
                                    .grid_column(column as i32)
                                    .automation_id(format!("sftp-comparison-{key}"))
                                    .text(format!("{label}  {count}"))
                                    .text_wrapping(TextWrapping::Wrap),
                            )
                        },
                    )),
            )
        }))
}

fn change_label(kind: Option<FolderChange>) -> &'static str {
    match kind {
        None => "All differences",
        Some(FolderChange::ReferenceOnly) => "Only in reference",
        Some(FolderChange::CurrentOnly) => "Only in current folder",
        Some(FolderChange::Type) => "Type changed",
        Some(FolderChange::Metadata) => "File metadata changed",
        Some(FolderChange::Unknown) => "Needs inspection",
    }
}

fn result_row(item: RowDetails<'_>, reveal: Option<View>, verify: Option<View>) -> KeyedView {
    let label = item
        .change
        .map_or("Matching metadata", |change| change_label(Some(change)));
    let detail = match (item.reference, item.current) {
        (Some(entry), Some(_)) if item.change.is_none() => entry_summary(entry),
        (Some(reference), Some(current)) => format!(
            "Reference  {}\nCurrent  {}",
            entry_summary(reference),
            entry_summary(current),
        ),
        (Some(entry), None) | (None, Some(entry)) => entry_summary(entry),
        (None, None) => String::new(),
    };
    KeyedView::new(
        item.path,
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto])
            .column_spacing(6.0)
            .children((
                TextBlock::new()
                    .text(format!("{label} · {}\n{detail}", item.path))
                    .text_wrapping(TextWrapping::Wrap),
                StackPanel::new()
                    .grid_column(1)
                    .vertical_alignment(VerticalAlignment::Top)
                    .spacing(4.0)
                    .children((
                        reveal.unwrap_or_else(View::empty),
                        verify.unwrap_or_else(View::empty),
                    )),
            )),
    )
}

fn entry_summary(entry: FolderEntry) -> String {
    if let Some(size) = entry.size {
        format!(
            "{} · {}",
            size_value(entry.kind_label(), u128::from(size)),
            properties::timestamp(entry.mtime)
        )
    } else if entry.is_file() {
        format!(
            "File · Size unavailable · {}",
            properties::timestamp(entry.mtime)
        )
    } else {
        entry.kind_label().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use w11term::sftp::{FolderDifference, FolderMatchedFile};

    #[test]
    fn paging_and_literal_path_filters_cover_the_complete_report() {
        let mut report = FolderComparison::default();
        for index in 0..(PAGE_SIZE * 6) {
            report.differences.push(FolderDifference {
                path: format!("logs/{index:03}.txt"),
                change: FolderChange::CurrentOnly,
                reference: None,
                current: None,
            });
        }
        let last = report.differences.len();
        report.differences.push(FolderDifference {
            path: "nested/Ä[summary].txt".into(),
            change: FolderChange::Metadata,
            reference: None,
            current: None,
        });
        let mut browser = Browser::default();
        browser.index(&report);
        assert_eq!(browser.matches.len(), last + 1);
        for _ in 0..10 {
            browser.update(&report, Action::Next);
        }
        assert_eq!(browser.page * PAGE_SIZE, last);
        browser.update(&report, Action::Previous);
        assert_eq!(browser.page * PAGE_SIZE, last - PAGE_SIZE);
        browser.update(&report, Action::Query("ä[SUMMARY]".into()));
        assert_eq!(browser.matches, [Row::Difference(last)]);
        assert_eq!(browser.page, 0);
        browser.update(
            &report,
            Action::Filter(Filter::Differences(Some(FolderChange::CurrentOnly))),
        );
        assert!(browser.matches.is_empty());
        browser.update(
            &report,
            Action::Filter(Filter::Differences(Some(FolderChange::Metadata))),
        );
        browser.index(&report);
        assert_eq!(browser.matches, [Row::Difference(last)]);
        browser.update(&report, Action::Query("x".repeat(MAX_QUERY_CHARS + 1)));
        assert!(browser.matches.is_empty());
    }

    #[test]
    fn matching_files_are_searchable_when_the_report_has_no_differences() {
        let file = serde_json::from_value(json!({
            "kind": 0o100000, "size": 10, "mtime": 5,
        }))
        .unwrap();
        let report = FolderComparison {
            matching_files: (0..45)
                .map(|index| FolderMatchedFile {
                    path: format!("docs/{index:02}.txt"),
                    reference: file,
                    current: file,
                })
                .collect(),
            ..Default::default()
        };
        let mut browser = Browser::default();
        browser.index(&report);
        assert!(browser.matches.is_empty());
        browser.update(&report, Action::Filter(Filter::MatchingFiles));
        browser.update(&report, Action::Next);
        assert_eq!(browser.matches.len(), 45);
        assert_eq!(browser.page, 1);
        browser.update(&report, Action::Query("DOCS/44".into()));
        assert_eq!(browser.matches, [Row::MatchingFile(44)]);
        assert_eq!(browser.page, 0);
        let row = browser.matches[0].get(&report).unwrap();
        assert_eq!(row.path, "docs/44.txt");
        assert!(row.can_verify());
    }
}
