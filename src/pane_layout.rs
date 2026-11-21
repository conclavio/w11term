//! Geometry of the terminal panes in one tab. All coordinates are relative to the tab.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// A vertical divider, with panes arranged from left to right.
    Columns,
    /// A horizontal divider, with panes arranged from top to bottom.
    Rows,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        let mut node = self;
        for &take_second in path {
            let Self::Split { first, second, .. } = node else {
                return false;
            };
            node = if take_second { second } else { first };
        }
        let Self::Split { ratio, .. } = node else {
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
