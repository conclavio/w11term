//! Geometry of the terminal panes in one tab. All coordinates are relative to the tab.

mod balance;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// A vertical divider, with panes arranged from left to right.
    Columns,
    /// A horizontal divider, with panes arranged from top to bottom.
    Rows,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Columns,
            Self::Up | Self::Down => Axis::Rows,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Bounds {
    pub const FULL: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

    fn divide(self, axis: Axis, ratio: f64) -> (Self, Self) {
        match axis {
            Axis::Columns => {
                let x = self.left + ratio * (self.right - self.left);
                (Self { right: x, ..self }, Self { left: x, ..self })
            }
            Axis::Rows => {
                let y = self.top + ratio * (self.bottom - self.top);
                (Self { bottom: y, ..self }, Self { top: y, ..self })
            }
        }
    }

    /// Length of the shared edge and distance between centers along that edge.
    fn adjacency(self, other: Self, direction: Direction) -> Option<(f64, f64)> {
        let (start, end, other_start, other_end) = match direction {
            Direction::Left if self.left == other.right => {
                (self.top, self.bottom, other.top, other.bottom)
            }
            Direction::Right if self.right == other.left => {
                (self.top, self.bottom, other.top, other.bottom)
            }
            Direction::Up if self.top == other.bottom => {
                (self.left, self.right, other.left, other.right)
            }
            Direction::Down if self.bottom == other.top => {
                (self.left, self.right, other.left, other.right)
            }
            _ => return None,
        };
        let overlap = end.min(other_end) - start.max(other_start);
        let distance = ((start + end) - (other_start + other_end)).abs() / 2.0;
        (overlap > 0.0).then_some((overlap, distance))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Layout {
    Leaf(u64),
    Split {
        axis: Axis,
        /// Fraction of this node's extent assigned to its first child.
        ratio: f64,
        first: Box<Layout>,
        second: Box<Layout>,
    },
}

impl Layout {
    /// Path and immediate parent of the leaf. Ancestor dividers are unaffected by rotation.
    pub(crate) fn parent_of(&self, pane: u64) -> Option<(Vec<bool>, &Layout)> {
        let Self::Split { first, second, .. } = self else {
            return None;
        };
        if matches!(first.as_ref(), Self::Leaf(id) if *id == pane)
            || matches!(second.as_ref(), Self::Leaf(id) if *id == pane)
        {
            return Some((Vec::new(), self));
        }
        for (side, child) in [(false, first), (true, second)] {
            if let Some((mut path, parent)) = child.parent_of(pane) {
                path.insert(0, side);
                return Some((path, parent));
            }
        }
        None
    }

    pub(crate) fn node_mut(&mut self, path: &[bool]) -> Option<&mut Self> {
        let mut node = self;
        for &second_child in path {
            let Self::Split { first, second, .. } = node else {
                return None;
            };
            node = if second_child { second } else { first };
        }
        Some(node)
    }

    fn leaf_path(&self, pane: u64, path: &mut Vec<bool>) -> bool {
        match self {
            Self::Leaf(id) => *id == pane,
            Self::Split { first, second, .. } => {
                for (side, child) in [(false, first), (true, second)] {
                    path.push(side);
                    if child.leaf_path(pane, path) {
                        return true;
                    }
                    path.pop();
                }
                false
            }
        }
    }

    fn node_at(&self, path: &[bool]) -> Option<&Self> {
        let mut node = self;
        for &second_child in path {
            let Self::Split { first, second, .. } = node else {
                return None;
            };
            node = if second_child { second } else { first };
        }
        Some(node)
    }

    /// Find the split where the two panes first diverge. The flag reports
    /// whether the requested pane is on the second side of this divider.
    pub fn divider_between(&self, pane: u64, other: u64) -> Option<(Vec<bool>, &Self, bool)> {
        let mut path = Vec::new();
        let mut other_path = Vec::new();
        if !self.leaf_path(pane, &mut path) || !self.leaf_path(other, &mut other_path) {
            return None;
        }
        let depth = path.iter().zip(&other_path).position(|(a, b)| a != b)?;
        let second_side = path[depth];
        path.truncate(depth);
        let divider = self.node_at(&path)?;
        Some((path, divider, second_side))
    }

    /// Check an exact divider weight against the current outer extent and each
    /// leaf's minimum. Project the latest ancestor weights in case their
    /// WinUI measurements have yet to catch up with a preceding resize.
    pub fn split_ratio_fits(
        &self,
        path: &[bool],
        ratio: f64,
        divider: f64,
        mut measure: impl FnMut(u64, Axis) -> Option<(f64, f64)>,
    ) -> Option<bool> {
        let Self::Split {
            axis,
            first,
            second,
            ..
        } = self.node_at(path)?
        else {
            return None;
        };
        if !(ratio > 0.0 && ratio < 1.0 && divider.is_finite() && divider >= 0.0) {
            return Some(false);
        }
        let axis = *axis;
        let (mut span, _) = self.axis_extents(axis, divider, &mut measure)?;
        let mut node = self;
        for &second_side in path {
            let Self::Split {
                axis: ancestor_axis,
                ratio,
                first,
                second,
            } = node
            else {
                return None;
            };
            if *ancestor_axis == axis {
                let fraction = if second_side { 1.0 - ratio } else { *ratio };
                span = ((span - divider.ceil()) * fraction).floor();
            }
            node = if second_side { second } else { first };
        }
        let available = span - divider.ceil();
        let (_, a) = first.axis_extents(axis, divider, &mut measure)?;
        let (_, b) = second.axis_extents(axis, divider, &mut measure)?;
        Some((available * ratio).floor() >= a && (available * (1.0 - ratio)).floor() >= b)
    }

    /// Check the proposed axis of the focused leaf's immediate split. Measurements are
    /// physical pixels: each leaf supplies its current outer extent and its one-cell minimum
    /// on that axis. The existing ratios are applied without adjusting any nested divider.
    pub fn rotation_fits(
        &self,
        pane: u64,
        divider: f64,
        mut measure: impl FnMut(u64, Axis) -> Option<(f64, f64)>,
    ) -> bool {
        let Some((
            _,
            Self::Split {
                axis,
                ratio,
                first,
                second,
            },
        )) = self.parent_of(pane)
        else {
            return false;
        };
        if !divider.is_finite() || divider < 0.0 {
            return false;
        }
        let across = match axis {
            Axis::Columns => Axis::Rows,
            Axis::Rows => Axis::Columns,
        };

        let Some((a, min_a)) = first.axis_extents(across, divider, &mut measure) else {
            return false;
        };
        let Some((b, min_b)) = second.axis_extents(across, divider, &mut measure) else {
            return false;
        };
        Self::divided_minimum(min_a, min_b, *ratio, divider) <= a.min(b)
    }

    // Each star-sized child can round down to a whole display pixel. Round its required
    // parent span up before applying the next ancestor's ratio.
    fn divided_minimum(a: f64, b: f64, ratio: f64, divider: f64) -> f64 {
        if ratio > 0.0 && ratio < 1.0 {
            (a / ratio).max(b / (1.0 - ratio)).ceil() + divider.ceil()
        } else {
            f64::INFINITY
        }
    }
    fn axis_extents(
        &self,
        across: Axis,
        divider: f64,
        measure: &mut impl FnMut(u64, Axis) -> Option<(f64, f64)>,
    ) -> Option<(f64, f64)> {
        match self {
            Layout::Leaf(id) => {
                let (current, minimum) = measure(*id, across)?;
                (current.is_finite() && current > 0.0 && minimum.is_finite() && minimum > 0.0)
                    .then_some((current.floor(), minimum.ceil()))
            }
            Layout::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (a, min_a) = first.axis_extents(across, divider, measure)?;
                let (b, min_b) = second.axis_extents(across, divider, measure)?;
                Some(if *axis == across {
                    (
                        a + divider.floor() + b,
                        Self::divided_minimum(min_a, min_b, *ratio, divider),
                    )
                } else {
                    // These extents normally match. Prefer the smaller one if the two
                    // canvases have reported different stages of a window resize.
                    (a.min(b), min_a.max(min_b))
                })
            }
        }
    }

    /// Closest divider that changes this leaf's extent on the given axis.
    /// The last flag tells whether the leaf belongs to its second child.
    pub fn divider_for(&self, id: u64, axis: Axis) -> Option<(Vec<bool>, &Layout, bool)> {
        let Self::Split {
            axis: here,
            first,
            second,
            ..
        } = self
        else {
            return None;
        };
        let in_second = second.bounds(id).is_some();
        let child = if in_second { second } else { first };
        if let Some((mut path, node, side)) = child.divider_for(id, axis) {
            path.insert(0, in_second);
            return Some((path, node, side));
        }
        (*here == axis && child.bounds(id).is_some()).then(|| (Vec::new(), self, in_second))
    }

    /// Space this subtree needs to leave one usable cell in every leaf at its current ratios.
    fn minimum_extent(&self, dimension: Axis, leaf: f64, divider: f64) -> f64 {
        match self {
            Self::Leaf(_) => leaf,
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let a = first.minimum_extent(dimension, leaf, divider);
                let b = second.minimum_extent(dimension, leaf, divider);
                if *axis == dimension {
                    (a / ratio).max(b / (1.0 - ratio)) + divider
                } else {
                    a.max(b)
                }
            }
        }
    }

    /// Move the nearest matching divider by five percent of its own span.
    /// Arrow direction describes divider motion from either side of the split.
    pub fn keyboard_resize(
        &self,
        id: u64,
        direction: Direction,
        leaf_minimum: f64,
        divider: f64,
        measure: impl FnOnce(&Self) -> Option<f64>,
    ) -> Option<(Vec<bool>, f64)> {
        let axis = direction.axis();
        let (path, node, _) = self.divider_for(id, axis)?;
        let Self::Split {
            ratio,
            first,
            second,
            ..
        } = node
        else {
            return None;
        };
        let available = measure(node)? - divider;
        if !available.is_finite() || available <= 0.0 {
            return None;
        }
        let next = match direction {
            Direction::Left | Direction::Up => {
                let minimum = first.minimum_extent(axis, leaf_minimum, divider) / available;
                (ratio - 0.05).max(minimum).min(*ratio)
            }
            Direction::Right | Direction::Down => {
                let maximum = 1.0 - second.minimum_extent(axis, leaf_minimum, divider) / available;
                (ratio + 0.05).min(maximum).max(*ratio)
            }
        };
        (next != *ratio).then_some((path, next))
    }

    fn visit(&self, bounds: Bounds, visit: &mut impl FnMut(u64, Bounds)) {
        match self {
            Self::Leaf(id) => visit(*id, bounds),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (a, b) = bounds.divide(*axis, *ratio);
                first.visit(a, visit);
                second.visit(b, visit);
            }
        }
    }

    pub fn leaves(&self) -> Vec<u64> {
        let mut panes = Vec::new();
        self.visit(Bounds::FULL, &mut |id, _| panes.push(id));
        panes
    }

    pub(crate) fn replace_leaves(&mut self, replace: &mut impl FnMut(u64) -> u64) {
        match self {
            Self::Leaf(id) => *id = replace(*id),
            Self::Split { first, second, .. } => {
                first.replace_leaves(replace);
                second.replace_leaves(replace);
            }
        }
    }

    pub fn bounds(&self, target: u64) -> Option<Bounds> {
        let mut found = None;
        self.visit(Bounds::FULL, &mut |id, bounds| {
            if id == target {
                found = Some(bounds);
            }
        });
        found
    }

    fn leaf_mut(&mut self, target: u64) -> Option<&mut Self> {
        match self {
            Self::Leaf(id) if *id == target => Some(self),
            Self::Leaf(_) => None,
            Self::Split { first, second, .. } => {
                first.leaf_mut(target).or_else(|| second.leaf_mut(target))
            }
        }
    }

    /// Put a new pane immediately to the right of or below the target.
    pub fn split(&mut self, target: u64, new_id: u64, axis: Axis) -> bool {
        let side = match axis {
            Axis::Columns => Direction::Right,
            Axis::Rows => Direction::Down,
        };
        self.insert_beside(target, new_id, side)
    }

    pub(crate) fn insert_beside(&mut self, target: u64, new_id: u64, side: Direction) -> bool {
        if self.bounds(new_id).is_some() {
            return false;
        }
        let Some(target_node) = self.leaf_mut(target) else {
            return false;
        };
        let (first, second) = match side {
            Direction::Left | Direction::Up => (new_id, target),
            Direction::Right | Direction::Down => (target, new_id),
        };
        *target_node = Self::Split {
            axis: side.axis(),
            ratio: 0.5,
            first: Box::new(Self::Leaf(first)),
            second: Box::new(Self::Leaf(second)),
        };
        true
    }

    /// Collapse the closed pane's parent, preserving all other divider ratios.
    pub fn remove(self, target: u64) -> Option<Self> {
        match self {
            Self::Leaf(id) => (id != target).then_some(Self::Leaf(id)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.remove(target), second.remove(target)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (first, second) => first.or(second),
            },
        }
    }

    /// Resolve a divider from the root. Each path element selects the first
    /// (false) or second (true) child. The view enforces minimum sizes in pixels.
    pub fn set_ratio(&mut self, path: &[bool], value: f64) -> bool {
        if !(value > 0.0 && value < 1.0) {
            return false;
        }
        let Some(Self::Split { ratio, .. }) = self.node_mut(path) else {
            return false;
        };
        *ratio = value;
        true
    }

    /// Focus the pane with the greatest shared edge in the chosen direction.
    /// Ties prefer the closest center, then layout order (top or left first).
    pub fn neighbor(&self, target: u64, direction: Direction) -> Option<u64> {
        let source = self.bounds(target)?;
        let mut best: Option<(u64, f64, f64)> = None;
        self.visit(Bounds::FULL, &mut |id, bounds| {
            if let Some((overlap, distance)) = source.adjacency(bounds, direction)
                && best.is_none_or(|(_, old_overlap, old_distance)| {
                    overlap > old_overlap || (overlap == old_overlap && distance < old_distance)
                })
            {
                best = Some((id, overlap, distance));
            }
        });
        best.map(|(id, _, _)| id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pane_pair_resolves_its_lowest_shared_divider_and_side() {
        let mut layout = Layout::Leaf(1);
        assert!(layout.split(1, 2, Axis::Columns));
        assert!(layout.split(2, 3, Axis::Rows));
        assert!(layout.split(3, 4, Axis::Columns));
        let (path, divider, second) = layout.divider_between(1, 4).unwrap();
        assert!(path.is_empty() && !second);
        assert_eq!(divider, &layout);
        let (path, divider, second) = layout.divider_between(4, 2).unwrap();
        assert_eq!(path, [true]);
        assert!(second);
        assert!(matches!(
            divider,
            Layout::Split {
                axis: Axis::Rows,
                ..
            }
        ));
        let (path, _, second) = layout.divider_between(4, 3).unwrap();
        assert_eq!(path, [true, true]);
        assert!(second);
        assert!(layout.divider_between(4, 4).is_none());
        assert!(layout.divider_between(4, 99).is_none());
    }

    #[test]
    fn exact_divider_resize_honors_ancestor_weights_and_nested_cell_minima() {
        let mut layout = Layout::Leaf(1);
        assert!(layout.split(1, 2, Axis::Columns));
        assert!(layout.split(2, 3, Axis::Columns));
        assert!(layout.split(3, 4, Axis::Rows));
        let measure = |pane, axis| {
            Some(match (axis, pane) {
                (Axis::Columns, 1) => (300.0, 40.0),
                (Axis::Columns, 2) => (150.0, 80.0),
                (Axis::Columns, _) => (150.0, 60.0),
                (Axis::Rows, 1 | 2) => (400.0, 40.0),
                (Axis::Rows, _) => (197.0, 40.0),
            })
        };
        assert_eq!(layout.split_ratio_fits(&[], 0.8, 6.0, measure), Some(false));
        assert_eq!(layout.split_ratio_fits(&[], 0.6, 6.0, measure), Some(true));
        assert_eq!(
            layout.split_ratio_fits(&[true], 0.75, 6.0, measure),
            Some(true)
        );
        assert!(layout.set_ratio(&[], 0.7));
        // The old measurements have yet to reflect the new outer weight.
        assert_eq!(
            layout.split_ratio_fits(&[true], 0.75, 6.0, measure),
            Some(false)
        );
        assert_eq!(
            layout.split_ratio_fits(&[true], 0.55, 6.0, measure),
            Some(true)
        );
        assert_eq!(
            layout.split_ratio_fits(&[true, true], 0.75, 6.0, measure),
            Some(true)
        );
        assert_eq!(layout.split_ratio_fits(&[], 0.5, 6.0, |_, _| None), None);
    }

    #[test]
    fn rotation_size_preserves_an_asymmetric_ratio_and_checks_both_measured_extents() {
        let mut layout = Layout::Leaf(1);
        layout.split(1, 2, Axis::Rows);
        layout.set_ratio(&[], 0.25);
        let before = layout.clone();
        let fits = |width_second: f64| {
            layout.rotation_fits(2, 6.0, |pane, axis| {
                assert_eq!(axis, Axis::Columns);
                Some(if pane == 1 {
                    (306.0, 75.0)
                } else {
                    (width_second, 60.0)
                })
            })
        };
        assert!(fits(306.0));
        // Redistributing one pixel would make this fit, but rotation preserves 25:75.
        assert!(!fits(305.0));
        assert_eq!(before, layout);
    }

    #[test]
    fn rotation_size_checks_nested_dividers_and_each_leafs_own_cell_only_in_the_affected_subtree() {
        let mut layout = Layout::Leaf(9);
        layout.split(9, 1, Axis::Rows);
        layout.split(1, 2, Axis::Columns);
        layout.split(2, 3, Axis::Rows);
        layout.set_ratio(&[true, true], 0.25);
        layout.split(3, 4, Axis::Columns);
        let fits = |row_minimum| {
            layout.rotation_fits(1, 6.0, |pane, axis| {
                assert_eq!(axis, Axis::Rows);
                Some(match pane {
                    1 => (506.0, 75.0),
                    2 => (125.0, row_minimum),
                    3 => (375.0, 60.0),
                    4 => (375.0, 90.0),
                    _ => panic!("measured a pane outside the rotating parent"),
                })
            })
        };
        // After rotation: each branch has 250 px; nested pane 2 gets 25% of (250 - 6).
        assert!(fits(61.0));
        assert!(!fits(62.0));
    }

    #[test]
    fn rotation_size_accounts_for_pixel_rounding_and_requires_valid_measurements() {
        let mut layout = Layout::Leaf(1);
        assert!(!layout.rotation_fits(1, 7.5, |_, _| panic!()));
        layout.split(1, 2, Axis::Columns);
        // At 125% scale, a 6-DIP divider can occupy 8 px, and each child needs 35 px.
        assert!(layout.rotation_fits(1, 7.5, |_, _| Some((78.0, 34.5))));
        assert!(!layout.rotation_fits(1, 7.5, |_, _| Some((77.9, 34.5))));
        assert!(!layout.rotation_fits(1, 7.5, |pane, _| (pane == 1).then_some((100.0, 34.5))));
        for invalid in [0.0, f64::NAN, f64::INFINITY] {
            assert!(!layout.rotation_fits(1, 7.5, |_, _| Some((invalid, 34.5))));
            assert!(!layout.rotation_fits(1, 7.5, |_, _| Some((100.0, invalid))));
        }
    }

    #[test]
    fn arrow_moves_the_closest_matching_divider_from_either_side() {
        let mut layout = Layout::Leaf(1);
        layout.split(1, 2, Axis::Columns);
        layout.split(2, 3, Axis::Rows);
        layout.split(3, 4, Axis::Columns);
        layout.set_ratio(&[true, true], 0.7);
        for pane in [3, 4] {
            for (direction, path, ratio) in [
                (Direction::Left, vec![true, true], 0.65),
                (Direction::Right, vec![true, true], 0.75),
                (Direction::Up, vec![true], 0.45),
                (Direction::Down, vec![true], 0.55),
            ] {
                let (actual, value) = layout
                    .keyboard_resize(pane, direction, 40.0, 6.0, |_| Some(600.0))
                    .unwrap();
                assert_eq!(actual, path);
                assert!((value - ratio).abs() < 1e-9);
            }
        }
        assert!(
            layout
                .keyboard_resize(1, Direction::Up, 40.0, 6.0, |_| Some(600.0))
                .is_none()
        );
        assert!(
            layout
                .keyboard_resize(99, Direction::Left, 40.0, 6.0, |_| Some(600.0))
                .is_none()
        );
        assert!(
            layout
                .keyboard_resize(1, Direction::Left, 40.0, 6.0, |_| None)
                .is_none()
        );
    }

    #[test]
    fn resizing_an_ancestor_keeps_the_smallest_nested_leaf_usable() {
        let mut layout = Layout::Leaf(1);
        layout.split(1, 2, Axis::Rows);
        layout.split(2, 3, Axis::Rows);
        layout.set_ratio(&[true], 0.25);
        // The second branch needs 166 pixels for its 40-pixel first leaf and divider.
        layout.set_ratio(&[], 0.55);
        let nudge =
            |layout: &Layout, dir| layout.keyboard_resize(1, dir, 40.0, 6.0, |_| Some(406.0));
        let (path, limit) = nudge(&layout, Direction::Down).unwrap();
        assert!((limit - (1.0 - 166.0 / 400.0)).abs() < 1e-9);
        assert!(layout.set_ratio(&path, limit));
        assert!(nudge(&layout, Direction::Down).is_none());
        assert!(nudge(&layout, Direction::Up).unwrap().1 < limit);
        // A small window or a previous drag can already violate a minimum.
        layout.set_ratio(&[], 0.8);
        assert!(nudge(&layout, Direction::Down).is_none());
        assert_eq!(nudge(&layout, Direction::Up).unwrap().1, 0.75);
    }

    #[test]
    fn nested_split_and_resize_affect_only_the_chosen_subtree() {
        let mut layout = Layout::Leaf(7);
        assert!(layout.split(7, 8, Axis::Columns));
        assert!(layout.split(8, 9, Axis::Rows));
        assert!(layout.set_ratio(&[], 0.375));
        assert!(layout.set_ratio(&[true], 0.75));
        assert_eq!(layout.leaves(), [7, 8, 9]);
        assert_eq!(
            layout.bounds(7),
            Some(Bounds {
                right: 0.375,
                ..Bounds::FULL
            })
        );
        assert_eq!(
            layout.bounds(8),
            Some(Bounds {
                left: 0.375,
                bottom: 0.75,
                ..Bounds::FULL
            })
        );
        assert_eq!(
            layout.bounds(9),
            Some(Bounds {
                left: 0.375,
                top: 0.75,
                ..Bounds::FULL
            })
        );
    }

    #[test]
    fn invalid_requests_leave_layout_intact() {
        let mut layout = Layout::Leaf(1);
        layout.split(1, 2, Axis::Columns);
        let original = layout.clone();
        assert!(!layout.split(7, 3, Axis::Rows));
        assert!(!layout.split(2, 1, Axis::Rows));
        for value in [-1.0, 0.0, 1.0, 2.0, f64::INFINITY, f64::NAN] {
            assert!(!layout.set_ratio(&[], value));
        }
        assert!(!layout.set_ratio(&[false], 0.5));
        assert!(!layout.set_ratio(&[true, false], 0.5));
        assert_eq!(layout, original);
        assert_eq!(layout.remove(99), Some(original));
    }

    #[test]
    fn closing_singleton_and_nested_leaves_preserves_remaining_layout() {
        let mut layout = Layout::Leaf(1);
        layout.split(1, 2, Axis::Columns);
        layout.split(2, 3, Axis::Rows);
        layout.set_ratio(&[true], 0.25);
        let remaining = layout.remove(1).unwrap();
        assert_eq!(remaining.leaves(), [2, 3]);
        assert_eq!(
            remaining.bounds(2),
            Some(Bounds {
                bottom: 0.25,
                ..Bounds::FULL
            })
        );
        let singleton = remaining.remove(2).unwrap();
        assert_eq!(singleton.bounds(3), Some(Bounds::FULL));
        assert_eq!(singleton.remove(3), None);
    }

    #[test]
    fn directional_focus_chooses_adjacent_panes_without_wrapping() {
        let mut layout = Layout::Leaf(1);
        layout.split(1, 2, Axis::Columns);
        layout.split(2, 3, Axis::Rows);
        assert_eq!(layout.neighbor(1, Direction::Right), Some(2));
        assert!(layout.set_ratio(&[true], 0.25));
        assert_eq!(layout.neighbor(1, Direction::Right), Some(3));
        assert_eq!(layout.neighbor(2, Direction::Left), Some(1));
        assert_eq!(layout.neighbor(2, Direction::Down), Some(3));
        assert_eq!(layout.neighbor(3, Direction::Up), Some(2));
        assert_eq!(layout.neighbor(1, Direction::Down), None);
        assert_eq!(layout.neighbor(9, Direction::Left), None);
        layout.split(1, 4, Axis::Rows);
        layout.set_ratio(&[false], 0.25);
        assert_eq!(layout.neighbor(1, Direction::Right), Some(2));
        assert_eq!(layout.neighbor(4, Direction::Right), Some(3));
    }
}
