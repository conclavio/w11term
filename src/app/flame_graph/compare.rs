use super::*;
use crate::app::openrouter_assistance::clip;
use std::fmt::Write as _;
use w11term::cpu_profile::{Change, Comparison, FilteredComparison};

const PAGE_SIZE: usize = 16;
const REVIEW_OTHER_STACKS: usize = 10;

pub enum Action {
    Back,
    Query(String),
    Filter,
    Export,
    OpenRouter,
    Inspect(ProfileSide),
    Page(bool),
    Select(Arc<Comparison>, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProfileSide {
    Before,
    After,
}

impl ProfileSide {
    pub(super) fn index(self) -> usize {
        self as usize
    }

    pub(super) fn caption(self) -> &'static str {
        match self {
            Self::Before => "Before profile",
            Self::After => "After profile",
        }
    }

    pub(super) fn widths(self) -> &'static str {
        match self {
            Self::Before => "Before widths",
            Self::After => "After widths",
        }
    }
}

pub struct ViewState {
    pub query: String,
    pub results: FilteredComparison,
    page: usize,
    selected: Option<usize>,
}

impl ViewState {
    pub fn new(before: Arc<Profile>, after: Arc<Profile>, paths: [PathBuf; 2]) -> Self {
        let comparison = Arc::new(Comparison::new([before, after], paths));
        let results = comparison.filter("");
        Self {
            query: String::new(),
            selected: results.indices().first().copied(),
            results,
            page: 0,
        }
    }

    pub fn filtered(&mut self, results: FilteredComparison) {
        self.results = results;
        self.page = 0;
        self.selected = self.results.indices().first().copied();
    }

    pub(super) fn after_path(&self) -> &std::path::Path {
        &self.results.comparison().paths()[1]
    }

    pub(super) fn before_source(&self) -> (Arc<Profile>, PathBuf) {
        let comparison = self.results.comparison();
        (
            comparison.profiles()[0].clone(),
            comparison.paths()[0].clone(),
        )
    }

    pub(super) fn selected_source(
        &self,
        side: ProfileSide,
    ) -> Option<(Arc<Profile>, PathBuf, usize)> {
        let comparison = self.results.comparison();
        let change = comparison.changes().get(self.selected?)?;
        let side = side.index();
        let frame = change.frame_ids()[side]?;
        Some((
            comparison.profiles()[side].clone(),
            comparison.paths()[side].clone(),
            frame,
        ))
    }

    fn page_rows(&self) -> &[usize] {
        let first = self.page * PAGE_SIZE;
        let indices = self.results.indices();
        &indices[first..(first + PAGE_SIZE).min(indices.len())]
    }

    fn page_count(&self) -> usize {
        self.results.indices().len().div_ceil(PAGE_SIZE).max(1)
    }

    pub fn page(&mut self, forward: bool) {
        let page = if forward {
            (self.page + 1).min(self.page_count() - 1)
        } else {
            self.page.saturating_sub(1)
        };
        if page != self.page {
            self.page = page;
            self.selected = self.page_rows().first().copied();
        }
    }

    pub fn select(&mut self, comparison: &Arc<Comparison>, id: usize) {
        if Arc::ptr_eq(self.results.comparison(), comparison) && self.page_rows().contains(&id) {
            self.selected = Some(id);
        }
    }

    pub(super) fn openrouter_preview(&self) -> Option<String> {
        let selected = self.selected?;
        let comparison = self.results.comparison();
        let selected_change = comparison.changes().get(selected)?;
        let [before, after] = comparison.totals();
        let matches = self.results.indices();
        let scope = if self.results.matches_query("") {
            "are shown"
        } else {
            "match the applied filter"
        };
        let mut preview = format!(
            "CPU profile comparison\nBefore: {} total · After: {} total\n{} of {} changed stacks {scope}.\n\nSelected stack\n",
            sample_count(before),
            sample_count(after),
            matches.len(),
            comparison.changes().len(),
        );
        write_preview_stack(&mut preview, comparison, selected_change, 2048);
        if matches.len() > 1 {
            preview.push_str("\nOther matching stacks by absolute change in sample share\n");
            for &id in matches
                .iter()
                .filter(|&&id| id != selected)
                .take(REVIEW_OTHER_STACKS)
            {
                write_preview_stack(&mut preview, comparison, &comparison.changes()[id], 768);
            }
            let omitted = matches.len().saturating_sub(REVIEW_OTHER_STACKS + 1);
            if omitted > 0 {
                let _ = writeln!(preview, "{omitted} additional matching stacks omitted.");
            }
        }
        Some(preview)
    }

    pub fn fields(
        &self,
        idle: bool,
        export_idle: bool,
        ctx: &ViewContext<TerminalApp>,
    ) -> Vec<KeyedView> {
        let send = |action: fn() -> Action| {
            ctx.callback(move |_| message(super::Action::Compare(action())))
        };
        let comparison = self.results.comparison();
        let mut fields = vec![
            KeyedView::new(
                "compare-back",
                Button::new()
                    .is_enabled(idle)
                    .on_click(send(|| Action::Back))
                    .content("Back to flame graph"),
            ),
            text("compare-before", source(comparison, 0)),
            text("compare-after", source(comparison, 1)),
            KeyedView::new(
                "compare-export",
                Button::new()
                    .is_enabled(idle && export_idle)
                    .on_click(send(|| Action::Export))
                    .content("Export comparison…"),
            ),
            KeyedView::new(
                "compare-differential",
                Button::new()
                    .is_enabled(idle)
                    .on_click(ctx.callback(|_| message(super::Action::Differential)))
                    .content("Differential flame graph"),
            ),
        ];
        if comparison.changes().is_empty() {
            fields.push(text(
                "compare-empty",
                "No changes in call stack sample shares.",
            ));
            return fields;
        }
        fields.push(KeyedView::new(
            "compare-filter",
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .column_spacing(6.0)
                .children((
                    TextBox::new()
                        .automation_name("Filter changed call stacks")
                        .placeholder_text("Filter by symbol")
                        .is_enabled(idle)
                        .text(&self.query)
                        .on_text_changed(ctx.callback(|query| {
                            message(super::Action::Compare(Action::Query(query)))
                        })),
                    Button::new()
                        .grid_column(1)
                        .is_enabled(idle && !self.results.matches_query(&self.query))
                        .on_click(send(|| Action::Filter))
                        .content("Apply filter"),
                )),
        ));
        if self.results.indices().is_empty() {
            fields.push(text(
                "compare-no-match",
                "No changed call stacks match the filter.",
            ));
            return fields;
        }
        fields.push(text(
            "compare-count",
            format!(
                "{} of {} changed stacks · Page {} of {}",
                self.results.indices().len(),
                comparison.changes().len(),
                self.page + 1,
                self.page_count(),
            ),
        ));
        if self.page_count() > 1 {
            fields.push(KeyedView::new(
                "compare-page",
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        Button::new()
                            .is_enabled(idle && self.page > 0)
                            .on_click(send(|| Action::Page(false)))
                            .content("Previous page"),
                        Button::new()
                            .is_enabled(idle && self.page + 1 < self.page_count())
                            .on_click(send(|| Action::Page(true)))
                            .content("Next page"),
                    )),
            ));
        }
        let rows = self.page_rows().to_vec();
        let selected = rows.iter().position(|&id| Some(id) == self.selected);
        let comparison_for_event = comparison.clone();
        fields.push(KeyedView::new(
            "compare-changes",
            ListView::new()
                .automation_name("Call stack changes by sample share")
                .max_height(280.0)
                .selected_index(selected)
                .on_selection_changed(ctx.callback(move |index: Option<usize>| {
                    index
                        .and_then(|offset| rows.get(offset))
                        .map_or(Message::ControlIdle, |&id| {
                            message(super::Action::Compare(Action::Select(
                                comparison_for_event.clone(),
                                id,
                            )))
                        })
                }))
                .collection_slot(
                    ListViewSlot::Items,
                    self.page_rows().iter().map(|&id| {
                        let change = &comparison.changes()[id];
                        let (profile, frame_id) = change.stack_end(comparison);
                        let frame = profile.frame(frame_id);
                        let symbol: String = frame.name.chars().take(128).collect();
                        let caller = (frame.parent != 0).then(|| {
                            profile
                                .frame(frame.parent)
                                .name
                                .chars()
                                .take(64)
                                .collect::<String>()
                        });
                        let mut label = format!("{} · {symbol}", comparison.delta(change));
                        if let Some(caller) = caller {
                            label.push_str(" ← ");
                            label.push_str(&caller);
                        }
                        KeyedView::new(
                            id.to_string(),
                            ListViewItem::new().automation_name(&label).content(
                                TextBlock::new()
                                    .text(label)
                                    .text_trimming(TextTrimming::CharacterEllipsis),
                            ),
                        )
                    }),
                ),
        ));
        if let Some(id) = self.selected {
            let change = &comparison.changes()[id];
            fields.push(text("compare-detail", change_detail(comparison, change)));
            fields.push(KeyedView::new(
                "compare-stack",
                TextBlock::new()
                    .text(change.stack(comparison))
                    .text_wrapping(TextWrapping::Wrap)
                    .is_text_selection_enabled(true),
            ));
            for (side, key, label) in [
                (
                    ProfileSide::Before,
                    "inspect-before",
                    "Inspect before flame graph",
                ),
                (
                    ProfileSide::After,
                    "inspect-after",
                    "Inspect after flame graph",
                ),
            ] {
                fields.push(KeyedView::new(
                    key,
                    Button::new()
                        .is_enabled(idle && change.frame_ids()[side.index()].is_some())
                        .on_click(ctx.callback(move |_| {
                            message(super::Action::Compare(Action::Inspect(side)))
                        }))
                        .content(label),
                ));
            }
            fields.push(KeyedView::new(
                "compare-review",
                Button::new()
                    .is_enabled(idle)
                    .on_click(send(|| Action::OpenRouter))
                    .content("Review comparison with OpenRouter…"),
            ));
        }
        fields
    }
}

fn change_detail(comparison: &Comparison, change: &Change) -> String {
    let count = |side| {
        let samples = change.counts()[side];
        format!(
            "{} ({:.2}%)",
            sample_count(samples),
            sample_share(samples, comparison.totals()[side]),
        )
    };
    format!(
        "Before {} · After {} · {}",
        count(0),
        count(1),
        comparison.delta(change),
    )
}

fn write_preview_stack(
    output: &mut String,
    comparison: &Comparison,
    change: &Change,
    limit: usize,
) {
    let stack = change.stack(comparison);
    if stack.len() > limit {
        let (profile, frame) = change.stack_end(comparison);
        let leaf = &profile.frame(frame).name;
        let _ = writeln!(
            output,
            "{}… (leaf {})",
            clip(&stack, limit),
            clip(leaf, 256)
        );
    } else {
        let _ = writeln!(output, "{stack}");
    }
    let _ = writeln!(output, "{}", change_detail(comparison, change));
}

fn source(comparison: &Comparison, side: usize) -> String {
    let path = &comparison.paths()[side];
    let filename = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let side_label = ["Before", "After"][side];
    format!(
        "{side_label} · {filename} · {}",
        sample_count(comparison.totals()[side])
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn compare(before: &str, after: &str) -> ViewState {
        let parse = |input| Arc::new(Profile::parse(input).unwrap());
        ViewState::new(
            parse(before),
            parse(after),
            [PathBuf::new(), PathBuf::new()],
        )
    }

    #[test]
    fn navigation_and_filter_reset_selection_to_the_current_page() {
        let before = (0..PAGE_SIZE)
            .map(|id| format!("symbol-{id} 1\n"))
            .collect::<String>();
        let mut state = compare(&before, "replacement 1");
        assert_eq!(state.page_count(), 2);
        assert_eq!(state.selected, Some(0));
        state.page(true);
        assert_eq!(state.selected, Some(PAGE_SIZE));
        state.select(&state.results.comparison().clone(), 0);
        assert_eq!(state.selected, Some(PAGE_SIZE));
        state.filtered(state.results.comparison().filter("replacement"));
        assert_eq!(state.page_count(), 1);
        assert_eq!(state.selected, Some(0));
    }

    #[test]
    fn snapshot_exports_every_applied_match_and_keeps_original_symbols() {
        let stack_count = PAGE_SIZE + 2;
        let mut before: String = (0..stack_count)
            .map(|id| format!("parent;leaf-{id}\u{202e}symbol 1\n"))
            .collect();
        before.push_str("outside-filter 2\n");
        let mut state = ViewState::new(
            Arc::new(Profile::parse(&before).unwrap()),
            Arc::new(Profile::parse("new-stack 7").unwrap()),
            ["folder/before.folded".into(), "folder/after.folded".into()],
        );
        state.filtered(state.results.comparison().filter("PARENT"));
        state.page(true);
        state.query = "typed but never applied".into();
        let comparison = state.results.comparison();
        let selected = &comparison.changes()[state.selected.unwrap()];
        let preview = state.openrouter_preview().unwrap();
        assert!(preview.contains(&format!(
            "Selected stack\n{}\n{}",
            selected.stack(comparison),
            change_detail(comparison, selected),
        )));
        assert!(preview.contains("18 of 20 changed stacks match the applied filter"));
        assert!(preview.contains("7 additional matching stacks omitted"));
        assert!(preview.contains(r"\u{202e}"));
        for omitted in [
            "before.folded",
            "after.folded",
            "typed but never applied",
            "outside-filter",
        ] {
            assert!(!preview.contains(omitted));
        }
        let snapshot = state.results.clone();
        state.filtered(state.results.comparison().filter(""));

        let mut bytes = Vec::new();
        snapshot.write_json(&mut bytes).unwrap();
        let report: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(report["schema_version"], 1);
        assert_eq!(report["filter"], "PARENT");
        assert_eq!(
            report["before"],
            json!({
                "file_name": "before.folded",
                "total_samples": stack_count + 2,
            })
        );
        assert_eq!(
            report["after"],
            json!({
                "file_name": "after.folded",
                "total_samples": 7,
            })
        );
        assert_eq!(report["total_changed_stacks"], stack_count + 2);
        let rows = report["changes"].as_array().unwrap();
        assert_eq!(rows.len(), stack_count);
        assert_eq!(rows[0]["stack"], json!(["parent", "leaf-0\u{202e}symbol"]));
        for row in rows {
            assert_eq!(row["before_samples"], 1);
            assert_eq!(row["after_samples"], 0);
            assert_eq!(row["after_percent"].as_f64().unwrap(), 0.0);
            let before_percent = row["before_percent"].as_f64().unwrap();
            let delta = row["delta_percentage_points"].as_f64().unwrap();
            assert!((before_percent + delta).abs() < 1.0e-12);
        }
    }
}
