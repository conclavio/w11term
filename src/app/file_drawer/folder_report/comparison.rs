use super::*;
use w11term::sftp::{FolderChange, FolderDifference, FolderEntry};

const PAGE_SIZE: usize = 40;
const MAX_QUERY_CHARS: usize = 4096;
const FILTERS: [Option<FolderChange>; 6] = [
    None,
    Some(FolderChange::ReferenceOnly),
    Some(FolderChange::CurrentOnly),
    Some(FolderChange::Type),
    Some(FolderChange::Metadata),
    Some(FolderChange::Unknown),
];

#[derive(Clone)]
pub enum Action {
    Query(String),
    Filter(Option<FolderChange>),
    Previous,
    Next,
}

#[derive(Default)]
pub(super) struct Browser {
    query: String,
    kind: Option<FolderChange>,
    page: usize,
    matches: Vec<usize>,
    viewport_revision: u64,
}

impl Browser {
    pub(super) fn update(&mut self, report: &FolderComparison, action: Action) {
        match action {
            Action::Query(query) if self.query != query => {
                self.query = query;
                self.index(report);
            }
            Action::Filter(kind) if self.kind != kind => {
                self.kind = kind;
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
        self.matches.extend(
            report
                .differences
                .iter()
                .enumerate()
                .filter_map(|(index, row)| {
                    (self.kind.is_none_or(|kind| row.change == kind)
                        && pattern
                            .as_ref()
                            .is_none_or(|pattern| pattern.is_match(&row.path)))
                    .then_some(index)
                }),
        );
    }

    pub(super) fn rows(
        &self,
        report: &FolderComparison,
        context: &ViewContext<TerminalApp>,
        message: impl Fn(super::Action) -> Message + Copy + 'static,
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
        if report.differences.is_empty() {
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
                        .automation_name("Folder difference type")
                        .items_source(FILTERS.map(change_label))
                        .selected_index(FILTERS.iter().position(|&kind| kind == self.kind))
                        .on_selection_changed(context.callback(move |index: Option<usize>| {
                            index
                                .and_then(|i| FILTERS.get(i))
                                .map_or(Message::ControlIdle, |&kind| {
                                    dispatch(Action::Filter(kind))
                                })
                        })),
                )),
        ));
        if self.matches.is_empty() {
            rows.push(KeyedView::new(
                "empty",
                TextBlock::new().text("No differences match these filters."),
            ));
            return rows;
        }
        let start = self.page * PAGE_SIZE;
        let total = self.matches.len();
        let end = (start + PAGE_SIZE).min(total);
        let status = match total {
            1 => "1 difference".into(),
            _ if end == start + 1 => format!("Showing {end} of {total} differences"),
            _ => format!("Showing {}–{end} of {total} differences", start + 1),
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
                .automation_name("Folder differences")
                .max_height(154.0)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .content(
                    StackPanel::new().spacing(6.0).keyed_children(
                        self.matches[start..end]
                            .iter()
                            .map(|&index| difference_row(&report.differences[index])),
                    ),
                ),
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
                            .content("Previous differences"),
                        Button::new()
                            .is_enabled(end < total)
                            .on_click(context.callback(move |_| dispatch(Action::Next)))
                            .content("Next differences"),
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

fn difference_row(item: &FolderDifference) -> KeyedView {
    let label = change_label(Some(item.change));
    let detail = match (item.reference, item.current) {
        (Some(reference), Some(current)) => format!(
            "Reference  {}\nCurrent  {}",
            entry_summary(reference),
            entry_summary(current),
        ),
        (Some(entry), None) | (None, Some(entry)) => entry_summary(entry),
        (None, None) => String::new(),
    };
    KeyedView::new(
        item.path.as_str(),
        TextBlock::new()
            .text(format!("{label} · {}\n{detail}", item.path))
            .text_wrapping(TextWrapping::Wrap),
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
        assert_eq!(browser.matches, [last]);
        assert_eq!(browser.page, 0);
        browser.update(&report, Action::Filter(Some(FolderChange::CurrentOnly)));
        assert!(browser.matches.is_empty());
        browser.update(&report, Action::Filter(Some(FolderChange::Metadata)));
        browser.index(&report);
        assert_eq!(browser.matches, [last]);
        browser.update(&report, Action::Query("x".repeat(MAX_QUERY_CHARS + 1)));
        assert!(browser.matches.is_empty());
    }
}
