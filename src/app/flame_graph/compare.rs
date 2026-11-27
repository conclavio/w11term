use super::{profile::symbol_pattern, *};
use std::cmp::{Ordering, Reverse};

mod export;
pub(super) use export::Snapshot;

const PAGE_SIZE: usize = 16;

pub enum Action {
    Back,
    Query(String),
    Filter,
    Export,
    Page(bool),
    Select(Arc<Comparison>, usize),
}

pub struct Comparison {
    profiles: [Arc<Profile>; 2],
    paths: [PathBuf; 2],
    totals: [u64; 2],
    changes: Vec<Change>,
}

struct Change {
    frames: [Option<usize>; 2],
    counts: [u64; 2],
    magnitude: u128,
    increased: bool,
}

impl Comparison {
    fn new(profiles: [Arc<Profile>; 2], paths: [PathBuf; 2]) -> Self {
        let [before, after] = &profiles;
        let totals = [before.frame(0).samples, after.frame(0).samples];
        let mut changes = Vec::new();
        let mut pending = vec![[Some(0), Some(0)]];
        while let Some(frames) = pending.pop() {
            let counts = std::array::from_fn(|side| {
                frames[side].map_or(0, |id| profiles[side].frame(id).own_samples)
            });
            // Cross multiply to test equality and rank changes without rounding,
            // even for recordings whose totals approach u64::MAX.
            let left = u128::from(counts[0]) * u128::from(totals[1]);
            let right = u128::from(counts[1]) * u128::from(totals[0]);
            if left != right {
                changes.push(Change {
                    frames,
                    counts,
                    magnitude: left.abs_diff(right),
                    increased: right > left,
                });
            }

            let [mut left, mut right] = std::array::from_fn(|side| {
                frames[side]
                    .map_or(&[][..], |id| profiles[side].row(id, 1))
                    .iter()
                    .copied()
                    .peekable()
            });
            loop {
                // Siblings are in original symbol order. Only pair equal
                // symbols under the same chain of callers.
                let pair = match (left.peek(), right.peek()) {
                    (Some(&a), Some(&b)) => match before.symbol(a).cmp(after.symbol(b)) {
                        Ordering::Less => [left.next(), None],
                        Ordering::Equal => [left.next(), right.next()],
                        Ordering::Greater => [None, right.next()],
                    },
                    (Some(_), None) => [left.next(), None],
                    (None, Some(_)) => [None, right.next()],
                    (None, None) => break,
                };
                pending.push(pair);
            }
        }
        changes.sort_unstable_by_key(|change| (Reverse(change.magnitude), change.frames));
        Self {
            profiles,
            paths,
            totals,
            changes,
        }
    }

    pub fn filter(&self, query: &str) -> Vec<usize> {
        let Some(needle) = symbol_pattern(query) else {
            return (0..self.changes.len()).collect();
        };
        let paths = self
            .profiles
            .each_ref()
            .map(|profile| profile.matching_paths(&needle));
        self.changes
            .iter()
            .enumerate()
            .filter_map(|(index, change)| {
                change
                    .frames
                    .iter()
                    .enumerate()
                    .any(|(side, id)| id.is_some_and(|id| paths[side][id]))
                    .then_some(index)
            })
            .collect()
    }

    fn delta_points(&self, change: &Change) -> f64 {
        let denominator = u128::from(self.totals[0]) * u128::from(self.totals[1]);
        let pp = 100.0 * (change.magnitude as f64 / denominator as f64);
        if change.increased { pp } else { -pp }
    }

    fn delta(&self, change: &Change) -> String {
        let pp = self.delta_points(change);
        let sign = if change.increased { '+' } else { '-' };
        if pp.abs() < 0.005 {
            format!("{sign}<0.01 pp")
        } else {
            format!("{pp:+.2} pp")
        }
    }
}

impl Change {
    fn stack_end<'a>(&self, comparison: &'a Comparison) -> (&'a Profile, usize) {
        if let Some(before) = self.frames[0] {
            (&comparison.profiles[0], before)
        } else {
            (
                &comparison.profiles[1],
                self.frames[1].expect("changed stack"),
            )
        }
    }

    fn stack_frames<'a>(&self, comparison: &'a Comparison) -> (&'a Profile, Vec<usize>) {
        let (profile, mut id) = self.stack_end(comparison);
        let mut ids = Vec::with_capacity(profile.frame(id).depth);
        while id != 0 {
            ids.push(id);
            id = profile.frame(id).parent;
        }
        ids.reverse();
        (profile, ids)
    }

    fn stack(&self, comparison: &Comparison) -> String {
        let (profile, ids) = self.stack_frames(comparison);
        let mut path = ids
            .into_iter()
            .map(|id| profile.frame(id).name.as_str())
            .collect::<Vec<_>>()
            .join(" → ");
        if let Some((end, _)) = path.char_indices().nth(32768) {
            path.truncate(end);
            path.push('…');
        }
        path
    }
}

pub struct ViewState {
    pub comparison: Arc<Comparison>,
    pub query: String,
    pub applied: String,
    visible: Arc<[usize]>,
    page: usize,
    selected: Option<usize>,
}

impl ViewState {
    pub fn new(before: Arc<Profile>, after: Arc<Profile>, paths: [PathBuf; 2]) -> Self {
        let comparison = Arc::new(Comparison::new([before, after], paths));
        let visible = comparison.filter("");
        Self {
            comparison,
            query: String::new(),
            applied: String::new(),
            selected: visible.first().copied(),
            visible: visible.into(),
            page: 0,
        }
    }

    pub fn filtered(&mut self, query: String, rows: Vec<usize>) {
        self.applied = query;
        self.visible = rows.into();
        self.page = 0;
        self.selected = self.visible.first().copied();
    }

    pub(super) fn after_path(&self) -> &std::path::Path {
        &self.comparison.paths[1]
    }

    pub(super) fn before_source(&self) -> (Arc<Profile>, PathBuf) {
        (
            self.comparison.profiles[0].clone(),
            self.comparison.paths[0].clone(),
        )
    }

    fn page_rows(&self) -> &[usize] {
        let first = self.page * PAGE_SIZE;
        &self.visible[first..(first + PAGE_SIZE).min(self.visible.len())]
    }

    fn page_count(&self) -> usize {
        self.visible.len().div_ceil(PAGE_SIZE).max(1)
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
        if Arc::ptr_eq(&self.comparison, comparison) && self.page_rows().contains(&id) {
            self.selected = Some(id);
        }
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
        let comparison = &self.comparison;
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
        ];
        if comparison.changes.is_empty() {
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
                        .is_enabled(idle && self.query.trim() != self.applied)
                        .on_click(send(|| Action::Filter))
                        .content("Apply filter"),
                )),
        ));
        if self.visible.is_empty() {
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
                self.visible.len(),
                comparison.changes.len(),
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
                        let change = &comparison.changes[id];
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
            let change = &comparison.changes[id];
            let count = |side| {
                format!(
                    "{} ({:.2}%)",
                    sample_count(change.counts[side]),
                    plot::share(change.counts[side], comparison.totals[side]),
                )
            };
            fields.push(text(
                "compare-detail",
                format!(
                    "Before {} · After {} · {}",
                    count(0),
                    count(1),
                    comparison.delta(change),
                ),
            ));
            fields.push(KeyedView::new(
                "compare-stack",
                TextBlock::new()
                    .text(change.stack(comparison))
                    .text_wrapping(TextWrapping::Wrap)
                    .is_text_selection_enabled(true),
            ));
        }
        fields
    }
}

fn source(comparison: &Comparison, side: usize) -> String {
    let path = &comparison.paths[side];
    let filename = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let side_label = ["Before", "After"][side];
    format!(
        "{side_label} · {filename} · {}",
        sample_count(comparison.totals[side])
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compare(before: &str, after: &str) -> ViewState {
        let parse = |input| Arc::new(Profile::parse(input).unwrap());
        ViewState::new(
            parse(before),
            parse(after),
            [PathBuf::new(), PathBuf::new()],
        )
    }

    #[test]
    fn doubling_every_count_preserves_sample_shares() {
        let state = compare("main 4\nmain;hot 6", "main 8\nmain;hot 12");
        assert!(state.comparison.changes.is_empty());
    }

    #[test]
    fn ranks_exact_stacks_by_normalized_self_samples_and_searches_callers() {
        let state = compare(
            "main 2\nmain;worker 5\nother;worker 3",
            "main 2\nmain;worker 6\nmain;new 12",
        );
        let comparison = state.comparison;
        let rows: Vec<_> = comparison
            .changes
            .iter()
            .map(|change| {
                (
                    change.stack(&comparison),
                    change.counts,
                    comparison.delta(change),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("main → new".into(), [0, 12], "+60.00 pp".into()),
                ("other → worker".into(), [3, 0], "-30.00 pp".into()),
                ("main → worker".into(), [5, 6], "-20.00 pp".into()),
                ("main".into(), [2, 2], "-10.00 pp".into()),
            ]
        );
        assert_eq!(comparison.filter("mAin"), [0, 2, 3]);
        assert_eq!(comparison.filter("WORKER"), [1, 2]);
        assert!(comparison.filter("main|worker").is_empty());
    }

    #[test]
    fn distinguishes_raw_symbols_that_display_the_same_way() {
        let state = compare("root;foo\u{202e}bar 1", "root;foo\\u{202e}bar 1");
        assert_eq!(state.comparison.changes.len(), 2);
        assert_eq!(state.comparison.changes[0].counts, [0, 1]);
        assert_eq!(state.comparison.changes[1].counts, [1, 0]);
        assert_eq!(state.comparison.filter("foo\\u{202e}bar"), [0, 1]);
    }

    #[test]
    fn full_range_sample_counts_do_not_overflow() {
        let before = format!("left {}", u64::MAX);
        let after = format!("right {}", u64::MAX);
        let state = compare(&before, &after);
        assert_eq!(
            state.comparison.delta(&state.comparison.changes[0]),
            "+100.00 pp"
        );
        assert_eq!(
            state.comparison.delta(&state.comparison.changes[1]),
            "-100.00 pp"
        );
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
        state.select(&state.comparison.clone(), 0);
        assert_eq!(state.selected, Some(PAGE_SIZE));
        let matched = state.comparison.filter("replacement");
        state.filtered("replacement".into(), matched);
        assert_eq!(state.page_count(), 1);
        assert_eq!(state.selected, Some(0));
    }
}
