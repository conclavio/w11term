use super::Profile;
use super::profile::{symbol_pattern, symbol_query};
use std::{
    cmp::{Ordering, Reverse},
    path::PathBuf,
    sync::Arc,
};

mod export;

pub struct Comparison {
    profiles: [Arc<Profile>; 2],
    paths: [PathBuf; 2],
    totals: [u64; 2],
    changes: Vec<Change>,
}

pub struct Change {
    frames: [Option<usize>; 2],
    counts: [u64; 2],
    magnitude: u128,
    increased: bool,
}

impl Comparison {
    pub fn new(profiles: [Arc<Profile>; 2], paths: [PathBuf; 2]) -> Self {
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

    pub fn profiles(&self) -> &[Arc<Profile>; 2] {
        &self.profiles
    }

    pub fn paths(&self) -> &[PathBuf; 2] {
        &self.paths
    }

    pub fn totals(&self) -> [u64; 2] {
        self.totals
    }

    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    pub fn filter(self: &Arc<Self>, query: &str) -> FilteredComparison {
        let filter = symbol_query(query).to_owned();
        let indices: Arc<[usize]> = match symbol_pattern(&filter) {
            None => (0..self.changes.len()).collect(),
            Some(needle) => {
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
        };
        FilteredComparison {
            comparison: self.clone(),
            indices,
            filter,
        }
    }

    fn delta_points(&self, change: &Change) -> f64 {
        let denominator = u128::from(self.totals[0]) * u128::from(self.totals[1]);
        let pp = 100.0 * (change.magnitude as f64 / denominator as f64);
        if change.increased { pp } else { -pp }
    }

    pub fn delta(&self, change: &Change) -> String {
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
    pub fn counts(&self) -> [u64; 2] {
        self.counts
    }

    /// The terminal frame in each source tree, including paths with only descendant samples.
    pub fn frame_ids(&self) -> [Option<usize>; 2] {
        self.frames
    }

    pub fn stack_end<'a>(&self, comparison: &'a Comparison) -> (&'a Profile, usize) {
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

    pub fn stack(&self, comparison: &Comparison) -> String {
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

/// A selection tied to its source comparison, including every matching row.
#[derive(Clone)]
pub struct FilteredComparison {
    comparison: Arc<Comparison>,
    indices: Arc<[usize]>,
    filter: String,
}

impl FilteredComparison {
    pub fn comparison(&self) -> &Arc<Comparison> {
        &self.comparison
    }

    pub fn indices(&self) -> &[usize] {
        &self.indices
    }

    pub fn matches_query(&self, query: &str) -> bool {
        self.filter == symbol_query(query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compare(before: &str, after: &str) -> Arc<Comparison> {
        let profiles = [before, after].map(|input| Arc::new(Profile::parse(input).unwrap()));
        Arc::new(Comparison::new(profiles, Default::default()))
    }

    #[test]
    fn doubling_every_count_preserves_sample_shares() {
        let comparison = compare("main 4\nmain;hot 6", "main 8\nmain;hot 12");
        assert!(comparison.changes.is_empty());
    }

    #[test]
    fn ranks_exact_stacks_by_normalized_self_samples_and_searches_callers() {
        let comparison = compare(
            "main 2\nmain;worker 5\nother;worker 3",
            "main 2\nmain;worker 6\nmain;new 12",
        );
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
        assert_eq!(comparison.filter("mAin").indices(), [0, 2, 3]);
        assert_eq!(comparison.filter("WORKER").indices(), [1, 2]);
        assert!(comparison.filter("main|worker").indices().is_empty());
    }

    #[test]
    fn distinguishes_raw_symbols_that_display_the_same_way() {
        let comparison = compare("root;foo\u{202e}bar 1", "root;foo\\u{202e}bar 1");
        assert_eq!(comparison.changes.len(), 2);
        assert_eq!(comparison.changes[0].counts, [0, 1]);
        assert_eq!(comparison.changes[1].counts, [1, 0]);
        assert_eq!(comparison.filter("foo\\u{202e}bar").indices(), [0, 1]);
    }

    #[test]
    fn retains_both_source_frames_when_one_side_only_has_descendant_samples() {
        let comparison = compare("main;worker 4", "main;worker;child 5");
        let worker = comparison
            .changes()
            .iter()
            .find(|change| change.counts() == [4, 0])
            .unwrap();
        let [before, after] = worker.frame_ids().map(Option::unwrap);
        let [before, after] = [
            comparison.profiles()[0].frame(before),
            comparison.profiles()[1].frame(after),
        ];
        assert_eq!((before.name.as_str(), before.own_samples), ("worker", 4));
        assert_eq!(
            (after.name.as_str(), after.own_samples, after.samples),
            ("worker", 0, 5)
        );
        let child = comparison
            .changes()
            .iter()
            .find(|change| change.counts() == [0, 5])
            .unwrap();
        assert_eq!(child.frame_ids()[0], None);
    }

    #[test]
    fn full_range_sample_counts_do_not_overflow() {
        let before = format!("left {}", u64::MAX);
        let after = format!("right {}", u64::MAX);
        let comparison = compare(&before, &after);
        assert_eq!(comparison.delta(&comparison.changes[0]), "+100.00 pp");
        assert_eq!(comparison.delta(&comparison.changes[1]), "-100.00 pp");
    }
}
