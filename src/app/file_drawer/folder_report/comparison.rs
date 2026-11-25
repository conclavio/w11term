use super::verify::{Checks, CompletedCheck};
use super::*;
use w11term::sftp::{FolderChange, FolderEntry};

const PAGE_SIZE: usize = 40;
const MAX_QUERY_CHARS: usize = 4096;
const FILTERS: [Filter; 10] = [
    Filter::Differences(None),
    Filter::Differences(Some(FolderChange::ReferenceOnly)),
    Filter::Differences(Some(FolderChange::CurrentOnly)),
    Filter::Differences(Some(FolderChange::Type)),
    Filter::Differences(Some(FolderChange::Metadata)),
    Filter::Differences(Some(FolderChange::Unknown)),
    Filter::MatchingFiles,
    Filter::Verification(VerificationFilter::Different),
    Filter::Verification(VerificationFilter::Matching),
    Filter::Verification(VerificationFilter::Unchecked),
];

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Filter {
    Differences(Option<FolderChange>),
    MatchingFiles,
    Verification(VerificationFilter),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum VerificationFilter {
    Different,
    Matching,
    Unchecked,
}

impl VerificationFilter {
    fn includes(self, check: Option<&CompletedCheck>) -> bool {
        match (self, check) {
            (Self::Different, Some(check)) => !check.digests.contents_match(),
            (Self::Matching, Some(check)) => check.digests.contents_match(),
            (Self::Unchecked, None) => true,
            _ => false,
        }
    }
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
            Self::Verification(VerificationFilter::Different) => "SHA-256: Contents differ",
            Self::Verification(VerificationFilter::Matching) => "SHA-256: Contents match",
            Self::Verification(VerificationFilter::Unchecked) => "SHA-256: Unchecked paired files",
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
    fn page_rows(&self) -> &[Row] {
        let start = self.page * PAGE_SIZE;
        let end = (start + PAGE_SIZE).min(self.matches.len());
        &self.matches[start..end]
    }

    pub(super) fn verification_paths(
        &self,
        report: &FolderComparison,
        viewport_revision: u64,
    ) -> Vec<String> {
        if self.viewport_revision != viewport_revision {
            return Vec::new();
        }
        self.page_rows()
            .iter()
            .filter_map(|row| {
                row.get(report)
                    .filter(RowDetails::can_verify)
                    .map(|row| row.path.to_owned())
            })
            .collect()
    }

    pub(super) fn update(&mut self, report: &FolderComparison, checks: &Checks, action: Action) {
        match action {
            Action::Query(query) if self.query != query => {
                self.query = query;
                self.index(report, checks);
            }
            Action::Filter(filter) if self.filter != filter => {
                self.filter = filter;
                self.index(report, checks);
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

    pub(super) fn index(&mut self, report: &FolderComparison, checks: &Checks) {
        self.page = 0;
        self.rebuild(report, checks);
    }

    pub(super) fn refresh_verification_filter(
        &mut self,
        report: &FolderComparison,
        checks: &Checks,
    ) {
        if matches!(self.filter, Filter::Verification(_)) {
            self.rebuild(report, checks);
            self.page = self
                .page
                .min(self.matches.len().saturating_sub(1) / PAGE_SIZE);
        }
    }

    fn rebuild(&mut self, report: &FolderComparison, checks: &Checks) {
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
            Filter::Verification(status) => {
                let mut files: Vec<_> = (0..report.differences.len())
                    .map(Row::Difference)
                    .chain((0..report.matching_files.len()).map(Row::MatchingFile))
                    .filter_map(|row| {
                        let item = row.get(report)?;
                        (item.can_verify()
                            && matches_path(item.path)
                            && status.includes(checks.get(item.path)))
                        .then_some((item.path, row))
                    })
                    .collect();
                files.sort_unstable_by_key(|&(path, _)| path);
                self.matches.extend(files.into_iter().map(|(_, row)| row));
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
        checks: &Checks,
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
                    .text("Compares paths, entry types, and regular-file sizes and modification times. Use SHA-256 to check file contents separately.")
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
        if self.matches.is_empty() {
            rows.push(KeyedView::new(
                "empty",
                TextBlock::new().text(match self.filter {
                    Filter::MatchingFiles => "No files with matching metadata fit the path filter.",
                    Filter::Verification(_) => "No paired files match this verification filter.",
                    Filter::Differences(_) if report.differences.is_empty() => {
                        "No differences in the reported metadata."
                    }
                    Filter::Differences(_) => "No differences match these filters.",
                }),
            ));
            return rows;
        }
        let start = self.page * PAGE_SIZE;
        let total = self.matches.len();
        let end = (start + PAGE_SIZE).min(total);
        let (singular, plural, previous, next, list_name) = match self.filter {
            Filter::MatchingFiles => (
                "file with matching metadata",
                "files with matching metadata",
                "Previous matching files",
                "Next matching files",
                "Files with matching metadata",
            ),
            Filter::Verification(_) => (
                "paired file",
                "paired files",
                "Previous paired files",
                "Next paired files",
                "Paired files by SHA-256 result",
            ),
            Filter::Differences(_) => (
                "difference",
                "differences",
                "Previous differences",
                "Next differences",
                "Folder differences",
            ),
        };
        let status = match total {
            1 => format!("1 {singular}"),
            _ if end == start + 1 => format!("Showing {end} of {total} {plural}"),
            _ => format!("Showing {}–{end} of {total} {plural}", start + 1),
        };
        let viewport_revision = self.viewport_revision;
        let can_verify_page = self
            .page_rows()
            .iter()
            .any(|row| row.get(report).is_some_and(|row| row.can_verify()));
        rows.push(KeyedView::new(
            "range",
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    TextBlock::new()
                        .automation_id("sftp-comparison-page")
                        .text(status)
                        .text_wrapping(TextWrapping::Wrap),
                    if can_verify_page {
                        Button::new()
                            .grid_column(1)
                            .is_enabled(reveal_enabled)
                            .on_click(context.callback(move |_| {
                                message(super::Action::Verify(super::verify::Action::Page {
                                    revision,
                                    viewport_revision,
                                }))
                            }))
                            .content("Verify files on this page…")
                    } else {
                        View::empty()
                    },
                )),
        ));
        rows.push(KeyedView::new(
            format!("differences-{}", self.viewport_revision),
            ScrollViewer::new()
                .automation_name(list_name)
                .max_height(154.0)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .content(StackPanel::new().spacing(6.0).keyed_children(
                    self.page_rows().iter().filter_map(|&row| {
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
                        let check = checks.get(item.path);
                        Some(result_row(item, reveal, verify, check))
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

fn result_row(
    item: RowDetails<'_>,
    reveal: Option<View>,
    verify: Option<View>,
    check: Option<&CompletedCheck>,
) -> KeyedView {
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
                StackPanel::new().spacing(4.0).children((
                    TextBlock::new()
                        .text(format!("{label} · {}\n{detail}", item.path))
                        .text_wrapping(TextWrapping::Wrap),
                    check
                        .map(|check| -> View {
                            TextBlock::new()
                                .text(check.label())
                                .text_wrapping(TextWrapping::Wrap)
                                .into()
                        })
                        .unwrap_or_else(View::empty),
                )),
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
        let checks = Checks::new();
        browser.index(&report, &checks);
        assert_eq!(browser.matches.len(), last + 1);
        for _ in 0..10 {
            browser.update(&report, &checks, Action::Next);
        }
        assert_eq!(browser.page * PAGE_SIZE, last);
        browser.update(&report, &checks, Action::Previous);
        assert_eq!(browser.page * PAGE_SIZE, last - PAGE_SIZE);
        browser.update(&report, &checks, Action::Query("ä[SUMMARY]".into()));
        assert_eq!(browser.matches, [Row::Difference(last)]);
        assert_eq!(browser.page, 0);
        browser.update(
            &report,
            &checks,
            Action::Filter(Filter::Differences(Some(FolderChange::CurrentOnly))),
        );
        assert!(browser.matches.is_empty());
        browser.update(
            &report,
            &checks,
            Action::Filter(Filter::Differences(Some(FolderChange::Metadata))),
        );
        browser.index(&report, &checks);
        assert_eq!(browser.matches, [Row::Difference(last)]);
        browser.update(
            &report,
            &checks,
            Action::Query("x".repeat(MAX_QUERY_CHARS + 1)),
        );
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
        let checks = Checks::new();
        browser.index(&report, &checks);
        assert!(browser.matches.is_empty());
        browser.update(&report, &checks, Action::Filter(Filter::MatchingFiles));
        browser.update(&report, &checks, Action::Next);
        assert_eq!(browser.matches.len(), 45);
        assert_eq!(browser.page, 1);
        let viewport = browser.viewport_revision;
        assert_eq!(
            browser.verification_paths(&report, viewport),
            [
                "docs/40.txt",
                "docs/41.txt",
                "docs/42.txt",
                "docs/43.txt",
                "docs/44.txt"
            ],
        );
        browser.update(&report, &checks, Action::Query("DOCS/44".into()));
        assert!(browser.verification_paths(&report, viewport).is_empty());
        assert_eq!(browser.matches, [Row::MatchingFile(44)]);
        assert_eq!(browser.page, 0);
        let row = browser.matches[0].get(&report).unwrap();
        assert_eq!(row.path, "docs/44.txt");
        assert!(row.can_verify());
    }

    #[test]
    fn verification_filters_cover_both_metadata_categories_and_update_paging() {
        let file = serde_json::from_value(json!({
            "kind": 0o100000, "size": 10, "mtime": 5,
        }))
        .unwrap();
        let mut report = FolderComparison {
            matching_files: (0..(PAGE_SIZE * 2))
                .map(|index| FolderMatchedFile {
                    path: format!("docs/{index:03}.txt"),
                    reference: file,
                    current: file,
                })
                .collect(),
            ..Default::default()
        };
        report.differences = vec![
            FolderDifference {
                path: "docs/-changed.txt".into(),
                change: FolderChange::Metadata,
                reference: Some(file),
                current: Some(file),
            },
            FolderDifference {
                path: "docs/only-here.txt".into(),
                change: FolderChange::CurrentOnly,
                reference: None,
                current: Some(file),
            },
        ];
        let check = |current: &str| CompletedCheck {
            completed_at_unix_seconds: None,
            digests: w11term::sftp::VerifiedFolderFile {
                reference_sha256: "reference".into(),
                current_sha256: current.into(),
            },
        };
        let mut checks = Checks::new();
        let mut browser = Browser::default();
        browser.update(
            &report,
            &checks,
            Action::Filter(Filter::Verification(VerificationFilter::Unchecked)),
        );
        assert_eq!(browser.matches.len(), PAGE_SIZE * 2 + 1);
        assert_eq!(browser.matches[0], Row::Difference(0));
        browser.update(&report, &checks, Action::Next);
        browser.update(&report, &checks, Action::Next);
        assert_eq!(browser.page, 2);
        assert_eq!(browser.page_rows(), &[Row::MatchingFile(PAGE_SIZE * 2 - 1)]);
        let old_viewport = browser.viewport_revision;
        checks.insert("docs/-changed.txt".into(), check("different"));
        checks.insert("docs/000.txt".into(), check("different"));
        checks.insert("docs/001.txt".into(), check("reference"));
        browser.refresh_verification_filter(&report, &checks);
        assert_eq!(browser.page, 1);
        assert_eq!(browser.matches.len(), PAGE_SIZE * 2 - 2);
        assert!(browser.verification_paths(&report, old_viewport).is_empty());

        browser.update(
            &report,
            &checks,
            Action::Filter(Filter::Verification(VerificationFilter::Different)),
        );
        assert_eq!(browser.matches, [Row::Difference(0), Row::MatchingFile(0)]);
        browser.update(&report, &checks, Action::Query("DOCS/000".into()));
        assert_eq!(browser.matches, [Row::MatchingFile(0)]);
        browser.update(
            &report,
            &checks,
            Action::Filter(Filter::Verification(VerificationFilter::Matching)),
        );
        assert!(browser.matches.is_empty());
        browser.update(&report, &checks, Action::Query(String::new()));
        assert_eq!(browser.matches, [Row::MatchingFile(1)]);
    }
}
