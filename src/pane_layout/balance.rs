use super::{Axis, Layout};

#[derive(Clone, Copy)]
struct Span {
    current: f64,
    minimum: f64,
    largest_cell: f64,
    lanes: usize,
}

impl Span {
    fn leaf((current, minimum): (f64, f64)) -> Option<Self> {
        if !current.is_finite() || current < 1.0 || !minimum.is_finite() || minimum <= 0.0 {
            return None;
        }
        Some(Self {
            current: current.floor(),
            minimum: minimum.ceil(),
            largest_cell: minimum.ceil(),
            lanes: 1,
        })
    }

    fn combine(self, other: Self, divided: bool, divider: f64) -> Option<Self> {
        let (current, minimum, lanes) = if divided {
            // Use the smaller possible device pixel count for the observed extent
            // and reserve the larger one when guaranteeing a cell in each leaf.
            (
                self.current + other.current + divider.floor(),
                self.minimum + other.minimum + divider.ceil(),
                self.lanes.checked_add(other.lanes)?,
            )
        } else {
            (
                self.current.min(other.current),
                self.minimum.max(other.minimum),
                self.lanes.max(other.lanes),
            )
        };
        (current.is_finite() && minimum.is_finite()).then_some(Self {
            current,
            minimum,
            lanes,
            largest_cell: self.largest_cell.max(other.largest_cell),
        })
    }
}

struct Measured {
    width: Span,
    height: Span,
    children: Option<(Axis, Box<Measured>, Box<Measured>)>,
}

impl Measured {
    fn read(
        layout: &Layout,
        divider: f64,
        measure: &mut impl FnMut(u64, Axis) -> Option<(f64, f64)>,
    ) -> Option<Self> {
        match layout {
            Layout::Leaf(id) => Some(Self {
                width: Span::leaf(measure(*id, Axis::Columns)?)?,
                height: Span::leaf(measure(*id, Axis::Rows)?)?,
                children: None,
            }),
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                if !(*ratio > 0.0 && *ratio < 1.0) {
                    return None;
                }
                let a = Self::read(first, divider, measure)?;
                let b = Self::read(second, divider, measure)?;
                Some(Self {
                    width: a.width.combine(b.width, *axis == Axis::Columns, divider)?,
                    height: a.height.combine(b.height, *axis == Axis::Rows, divider)?,
                    children: Some((*axis, Box::new(a), Box::new(b))),
                })
            }
        }
    }

    fn span(&self, axis: Axis) -> Span {
        match axis {
            Axis::Columns => self.width,
            Axis::Rows => self.height,
        }
    }

    /// Outer extent required when every lane receives at least `unit` pixels.
    /// Perpendicular children share an extent, parallel children add a divider.
    fn required(&self, axis: Axis, unit: f64, divider: f64) -> f64 {
        let span = self.span(axis);
        if unit >= span.largest_cell {
            return span.lanes as f64 * unit + (span.lanes - 1) as f64 * divider;
        }
        let Some((split, a, b)) = &self.children else {
            return span.minimum.max(unit);
        };
        let a = a.required(axis, unit, divider);
        let b = b.required(axis, unit, divider);
        if *split == axis {
            a + divider + b
        } else {
            a.max(b)
        }
    }

    fn arrange(&self, layout: &mut Layout, width: f64, height: f64, divider: f64) -> Option<bool> {
        if width < self.width.minimum || height < self.height.minimum {
            return None;
        }
        let Layout::Split {
            axis,
            ratio,
            first,
            second,
        } = layout
        else {
            return Some(false);
        };
        let (_, a, b) = self.children.as_ref()?;
        let outer = if *axis == Axis::Columns {
            width
        } else {
            height
        };
        let available = outer - divider;
        let lanes = self.span(*axis).lanes;
        let mut unit = (outer - (lanes - 1) as f64 * divider) / lanes as f64;
        // Normally the closed form above fits all cells. When a large-font pane
        // needs more, distribute the remaining space equally across the other
        // lanes, even when the tree groups that pane with a small sibling.
        if unit < self.span(*axis).largest_cell && self.required(*axis, unit, divider) > outer {
            let mut low = 0.0;
            let mut high = unit;
            for _ in 0..48 {
                let mid = low + (high - low) * 0.5;
                if self.required(*axis, mid, divider) <= outer {
                    low = mid;
                } else {
                    high = mid;
                }
            }
            unit = low;
        }
        let min_first = a.span(*axis).minimum;
        let max_first = available - b.span(*axis).minimum;
        if min_first > max_first {
            return None;
        }
        let first_px = a
            .required(*axis, unit, divider)
            .round()
            .clamp(min_first, max_first);
        let second_px = available - first_px;
        let old_px = *ratio * available;
        let changed = old_px.round() != first_px
            || *ratio < min_first / available
            || *ratio > max_first / available;
        if changed {
            let new_ratio = first_px / available;
            if !(new_ratio > 0.0 && new_ratio < 1.0) {
                return None;
            }
            *ratio = new_ratio;
        }
        let (aw, ah, bw, bh) = match axis {
            Axis::Columns => (first_px, height, second_px, height),
            Axis::Rows => (width, first_px, width, second_px),
        };
        let first_changed = a.arrange(first, aw, ah, divider)?;
        let second_changed = b.arrange(second, bw, bh, divider)?;
        Some(changed || first_changed || second_changed)
    }
}

impl Layout {
    /// Compute a replacement using physical outer pane sizes and one-cell minima.
    /// A single pane, an infeasible or unmeasured tree, and a pixel-equivalent layout
    /// all produce no change.
    pub(crate) fn balanced(
        &self,
        divider: f64,
        mut measure: impl FnMut(u64, Axis) -> Option<(f64, f64)>,
    ) -> Option<Self> {
        if matches!(self, Self::Leaf(_)) || !divider.is_finite() || divider < 0.0 {
            return None;
        }
        let tree = Measured::read(self, divider, &mut measure)?;
        let mut layout = self.clone();
        tree.arrange(
            &mut layout,
            tree.width.current,
            tree.height.current,
            divider.ceil(),
        )?
        .then_some(layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn three(axis: Axis, nested_first: bool) -> Layout {
        let mut layout = Layout::Leaf(1);
        if nested_first {
            layout.split(1, 3, axis);
            layout.split(1, 2, axis);
        } else {
            layout.split(1, 2, axis);
            layout.split(2, 3, axis);
        }
        layout
    }

    // The same star allocation used by the native grids, rounded to physical pixels.
    fn sizes(layout: &Layout, width: f64, height: f64, divider: f64) -> BTreeMap<u64, [f64; 2]> {
        fn visit(
            layout: &Layout,
            w: f64,
            h: f64,
            divider: f64,
            result: &mut BTreeMap<u64, [f64; 2]>,
        ) {
            match layout {
                Layout::Leaf(id) => {
                    result.insert(*id, [w, h]);
                }
                Layout::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } => {
                    let (aw, ah, bw, bh) = match axis {
                        Axis::Columns => {
                            let a = ((w - divider) * ratio).round();
                            (a, h, w - divider - a, h)
                        }
                        Axis::Rows => {
                            let a = ((h - divider) * ratio).round();
                            (w, a, w, h - divider - a)
                        }
                    };
                    visit(first, aw, ah, divider, result);
                    visit(second, bw, bh, divider, result);
                }
            }
        }
        let mut result = BTreeMap::new();
        visit(layout, width, height, divider, &mut result);
        result
    }

    #[test]
    fn nested_same_axis_balances_physical_lanes_with_both_tree_orders() {
        for axis in [Axis::Columns, Axis::Rows] {
            for nested_first in [false, true] {
                let layout = three(axis, nested_first);
                let balanced = layout
                    .balanced(6.0, |id, dimension| {
                        Some((
                            if dimension == axis {
                                [222.0, 53.0, 25.0][id as usize - 1]
                            } else {
                                54.0
                            },
                            12.0,
                        ))
                    })
                    .unwrap();
                let (width, height, expected) = match axis {
                    Axis::Columns => (312.0, 54.0, [100.0, 54.0]),
                    Axis::Rows => (54.0, 312.0, [54.0, 100.0]),
                };
                let actual = sizes(&balanced, width, height, 6.0);
                assert!(actual.values().all(|&size| size == expected));
                assert!(
                    balanced
                        .balanced(6.0, |id, axis| Some((
                            actual[&id][usize::from(axis == Axis::Rows)],
                            12.0
                        )))
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn perpendicular_stacks_share_the_same_columns_and_full_width_rows() {
        let mut layout = Layout::Leaf(1);
        // Columns: [1] beside [2 | 4] over [3].
        layout.split(1, 2, Axis::Columns);
        layout.split(2, 3, Axis::Rows);
        layout.split(2, 4, Axis::Columns);
        let inputs = [[150.0, 206.0], [50.0, 130.0], [156.0, 70.0], [100.0, 130.0]];
        let balanced = layout
            .balanced(6.0, |id, axis| {
                Some((
                    inputs[(id - 1) as usize][usize::from(axis == Axis::Rows)],
                    16.0,
                ))
            })
            .unwrap();
        assert_eq!(
            sizes(&balanced, 312.0, 206.0, 6.0),
            BTreeMap::from([
                (1, [100.0, 206.0]),
                (2, [100.0, 100.0]),
                (3, [206.0, 100.0]),
                (4, [100.0, 100.0]),
            ])
        );
    }

    #[test]
    fn large_font_minima_share_remaining_pixels_regardless_of_grouping() {
        for nested_first in [true, false] {
            let layout = three(Axis::Columns, nested_first);
            let balanced = layout
                .balanced(6.0, |id, axis| {
                    Some(match axis {
                        Axis::Columns => (
                            [140.0, 35.0, 23.0][id as usize - 1],
                            if id == 1 { 89.5 } else { 14.0 },
                        ),
                        Axis::Rows => (60.0, 60.0),
                    })
                })
                .unwrap();
            assert_eq!(
                sizes(&balanced, 210.0, 60.0, 6.0),
                BTreeMap::from([(1, [90.0, 60.0]), (2, [54.0, 60.0]), (3, [54.0, 60.0]),])
            );
        }
    }

    #[test]
    fn missing_invalid_or_infeasible_leaf_measurements_reject_the_entire_edit() {
        let tree = three(Axis::Columns, false);
        for invalid in [
            None,
            Some((0.0, 2.0)),
            Some((f64::NAN, 2.0)),
            Some((f64::INFINITY, 2.0)),
            Some((40.0, 0.0)),
            Some((40.0, f64::NAN)),
            Some((40.0, -1.0)),
            Some((40.0, 40.01)), // footer/cell wouldn't fit the shared row.
        ] {
            assert!(
                tree.balanced(7.5, |id, axis| if id == 3 && axis == Axis::Rows {
                    invalid
                } else {
                    Some((40.9, 4.0))
                })
                .is_none()
            );
        }
        for divider in [f64::NAN, f64::INFINITY, -0.1] {
            assert!(tree.balanced(divider, |_, _| Some((40.0, 4.0))).is_none());
        }
        assert!(tree.balanced(7.5, |_, _| Some((41.0, 41.0))).is_none());
    }

    #[test]
    fn fractional_dpi_rounds_minima_up_and_repeated_balance_does_no_work() {
        for divider in [7.5_f64, 8.25] {
            // WinUI may round the physical divider to either neighboring pixel.
            for actual_divider in [divider.floor(), divider.ceil()] {
                for width in 300..340 {
                    let mut layout = three(Axis::Columns, false);
                    layout.set_ratio(&[], 0.72);
                    let current = sizes(&layout, f64::from(width), 96.0, actual_divider);
                    let measure = |sizes: &BTreeMap<u64, [f64; 2]>, id, axis| {
                        Some((
                            sizes[&id][usize::from(axis == Axis::Rows)],
                            if axis == Axis::Rows { 70.25 } else { 12.5 },
                        ))
                    };
                    let balanced = layout
                        .balanced(divider, |id, axis| measure(&current, id, axis))
                        .unwrap();
                    let actual = sizes(&balanced, f64::from(width), 96.0, actual_divider);
                    let widths = actual
                        .values()
                        .map(|size| size[0] as u32)
                        .collect::<Vec<_>>();
                    assert!(widths.iter().max().unwrap() - widths.iter().min().unwrap() <= 2);
                    assert!(
                        balanced
                            .balanced(divider, |id, axis| measure(&actual, id, axis))
                            .is_none(),
                        "{width} at {divider}, actual {actual_divider}"
                    );
                }
            }
        }
    }
}
