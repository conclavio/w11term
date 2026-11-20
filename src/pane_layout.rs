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
        if self.bounds(new_id).is_some() {
            return false;
        }
        let Some(target_node) = self.leaf_mut(target) else {
            return false;
        };
        *target_node = Self::Split {
            axis,
            ratio: 0.5,
            first: Box::new(Self::Leaf(target)),
            second: Box::new(Self::Leaf(new_id)),
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
