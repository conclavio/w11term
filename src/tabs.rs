//! Ordered tab headers, each with a split layout and a remembered focused entry.
use crate::pane_layout::{Axis, Direction, Layout};
use std::collections::HashMap;

pub type TabId = u64;

pub struct Tab<T> {
    pub id: TabId,
    pub value: T,
    group: TabId,
}

struct Group {
    id: TabId,
    focused: TabId,
    layout: Layout,
    zoomed: bool,
}

pub struct Tabs<T> {
    entries: Vec<Tab<T>>,
    groups: Vec<Group>,
    active_group: Option<TabId>,
    next_id: TabId,
}

impl<T> Default for Tabs<T> {
    fn default() -> Self {
        Self {
            entries: vec![],
            groups: vec![],
            active_group: None,
            next_id: 1,
        }
    }
}

impl<T> Tabs<T> {
    /// Includes every entry, even when another pane in its group is zoomed.
    pub fn entries(&self) -> &[Tab<T>] {
        &self.entries
    }
    /// The focused entry supplies each tab header's current label and content.
    pub fn headers(&self) -> Vec<&Tab<T>> {
        self.groups
            .iter()
            .map(|group| {
                self.entries
                    .iter()
                    .find(|entry| entry.id == group.focused)
                    .expect("a group always has a focused entry")
            })
            .collect()
    }
    pub fn group_id(&self, entry: TabId) -> Option<TabId> {
        self.entries
            .iter()
            .find(|tab| tab.id == entry)
            .map(|tab| tab.group)
    }
    pub fn active_id(&self) -> Option<TabId> {
        Some(self.groups.get(self.active_index()?)?.focused)
    }
    pub fn active_index(&self) -> Option<usize> {
        let id = self.active_group?;
        self.groups.iter().position(|g| g.id == id)
    }
    pub fn active_layout(&self) -> Option<&Layout> {
        Some(&self.groups.get(self.active_index()?)?.layout)
    }
    pub fn layout_for(&self, entry: TabId) -> Option<&Layout> {
        let id = self.group_id(entry)?;
        self.groups.iter().find(|g| g.id == id).map(|g| &g.layout)
    }
    pub fn zoomed(&self) -> bool {
        self.active_index()
            .is_some_and(|index| self.groups[index].zoomed)
    }
    pub fn visible(&self, entry: TabId) -> bool {
        self.active_group.is_some()
            && self.group_id(entry) == self.active_group
            && (!self.zoomed() || self.active_id() == Some(entry))
    }
    pub fn focus_neighbor(&mut self, direction: Direction) -> bool {
        let Some(index) = self.active_index() else {
            return false;
        };
        let group = &mut self.groups[index];
        let Some(next) = group.layout.neighbor(group.focused, direction) else {
            return false;
        };
        group.focused = next;
        true
    }
    pub fn toggle_zoom(&mut self) -> bool {
        let Some(index) = self.active_index() else {
            return false;
        };
        let group = &mut self.groups[index];
        if matches!(group.layout, Layout::Leaf(_)) {
            return false;
        }
        group.zoomed = !group.zoomed;
        true
    }
    pub fn set_ratio(&mut self, path: &[bool], ratio: f64) -> bool {
        let Some(index) = self.active_index() else {
            return false;
        };
        let group = &mut self.groups[index];
        !group.zoomed && group.layout.set_ratio(path, ratio)
    }
    pub fn get(&self, id: TabId) -> Option<&T> {
        self.entries.iter().find(|t| t.id == id).map(|t| &t.value)
    }
    pub fn get_mut(&mut self, id: TabId) -> Option<&mut T> {
        self.entries
            .iter_mut()
            .find(|t| t.id == id)
            .map(|t| &mut t.value)
    }
    pub fn active(&self) -> Option<&T> {
        self.get(self.active_id()?)
    }
    pub fn insert(&mut self, value: T) -> TabId {
        let id = self.next_id;
        self.insert_group(id, value);
        id
    }

    fn insert_group(&mut self, entry_id: TabId, value: T) {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(Tab {
            id: entry_id,
            group: id,
            value,
        });
        self.groups.push(Group {
            id,
            focused: entry_id,
            layout: Layout::Leaf(entry_id),
            zoomed: false,
        });
        self.active_group = Some(id);
    }

    pub fn move_active_to_new_tab(&mut self) -> bool {
        if !matches!(self.active_layout(), Some(Layout::Split { .. })) {
            return false;
        }
        let id = self.active_id().expect("a split has a focused pane");
        let value = self.remove(id).expect("focused pane exists");
        self.insert_group(id, value);
        true
    }
    pub fn split(&mut self, target: TabId, value: T, axis: Axis) -> Option<TabId> {
        let group_id = self.group_id(target)?;
        let group = self.groups.iter_mut().find(|g| g.id == group_id)?;
        let id = self.next_id;
        if !group.layout.split(target, id, axis) {
            return None;
        }
        self.next_id += 1;
        self.entries.push(Tab {
            id,
            group: group_id,
            value,
        });
        group.focused = id;
        group.zoomed = false;
        self.active_group = Some(group_id);
        Some(id)
    }
    pub fn select(&mut self, id: TabId) -> bool {
        let Some(group_id) = self.group_id(id) else {
            return false;
        };
        let group = self
            .groups
            .iter_mut()
            .find(|g| g.id == group_id)
            .expect("entry has a group");
        group.focused = id;
        self.active_group = Some(group_id);
        true
    }
    pub fn select_index(&mut self, index: usize) -> bool {
        let Some(group) = self.groups.get(index) else {
            return false;
        };
        self.active_group = Some(group.id);
        true
    }
    pub fn cycle(&mut self, offset: isize) {
        if let Some(index) = self.active_index() {
            let len = self.groups.len();
            let next = (index + offset.rem_euclid(len as isize) as usize) % len;
            self.select_index(next);
        }
    }
    pub fn remove(&mut self, id: TabId) -> Option<T> {
        let index = self.entries.iter().position(|t| t.id == id)?;
        let removed = self.entries.remove(index);
        let group_index = self
            .groups
            .iter()
            .position(|g| g.id == removed.group)
            .expect("entry has a group");
        let group = &mut self.groups[group_index];
        let layout = std::mem::replace(&mut group.layout, Layout::Leaf(id));
        let focus = if group.focused == id {
            [
                Direction::Right,
                Direction::Left,
                Direction::Down,
                Direction::Up,
            ]
            .into_iter()
            .find_map(|direction| layout.neighbor(id, direction))
        } else {
            Some(group.focused)
        };
        if let Some(remaining) = layout.remove(id) {
            group.focused = focus.unwrap_or_else(|| remaining.leaves()[0]);
            group.zoomed &= matches!(remaining, Layout::Split { .. });
            group.layout = remaining;
        } else {
            self.remove_header(group_index);
        }
        Some(removed.value)
    }
    fn remove_header(&mut self, index: usize) {
        let group = self.groups.remove(index);
        if self.active_group == Some(group.id) {
            self.active_group = self
                .groups
                .get(index)
                .or_else(|| self.groups.last())
                .map(|g| g.id);
        }
    }
    /// Close the tab containing the supplied entry, including every split.
    pub fn remove_group(&mut self, entry: TabId) -> bool {
        let Some(id) = self.group_id(entry) else {
            return false;
        };
        let group = self
            .groups
            .iter()
            .position(|g| g.id == id)
            .expect("entry has a group");
        self.entries.retain(|entry| entry.group != id);
        self.remove_header(group);
        true
    }
    pub fn reorder(&mut self, order: &[TabId]) -> bool {
        if order.len() != self.groups.len() {
            return false;
        }
        let mut positions = HashMap::with_capacity(order.len());
        for (position, &entry) in order.iter().enumerate() {
            let Some(group) = self.group_id(entry) else {
                return false;
            };
            if positions.insert(group, position).is_some() {
                return false;
            }
        }
        self.groups.sort_by_key(|group| positions[&group.id]);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn moving_the_original_leaf_keeps_its_identity_and_the_previous_tab() {
        let mut tabs = Tabs::default();
        let first = tabs.insert("left");
        let second = tabs.split(first, "right", Axis::Columns).unwrap();
        tabs.select(first);
        assert!(tabs.move_active_to_new_tab());
        assert_eq!(tabs.active_id(), Some(first));
        assert_eq!(tabs.get(first), Some(&"left"));
        assert_eq!(tabs.group_id(second), Some(first));
        assert_ne!(tabs.group_id(first), Some(first));
        assert!(tabs.visible(first));
        assert!(!tabs.visible(second));
        assert!(tabs.remove_group(first));
        assert_eq!(tabs.active_id(), Some(second));
    }
    #[test]
    fn close_and_reorder_preserve_the_selected_session() {
        let mut tabs = Tabs::default();
        let a = tabs.insert("root");
        let b = tabs.insert("play");
        let c = tabs.insert("settings");
        tabs.select(b);
        tabs.remove(a);
        assert_eq!(tabs.active(), Some(&"play"));
        assert!(tabs.reorder(&[c, b]));
        assert_eq!(tabs.active_id(), Some(b));
        assert_eq!(tabs.active_index(), Some(1));
        tabs.remove(b);
        assert_eq!(tabs.active(), Some(&"settings"));
        tabs.remove(c);
        assert!(tabs.active().is_none());
    }
    #[test]
    fn cycling_wraps_and_invalid_reorders_cannot_drop_tabs() {
        let mut tabs = Tabs::default();
        let a = tabs.insert(1);
        let b = tabs.insert(2);
        tabs.cycle(1);
        assert_eq!(tabs.active_id(), Some(a));
        tabs.cycle(-1);
        assert_eq!(tabs.active_id(), Some(b));
        assert!(!tabs.reorder(&[a, a]));
        assert!(!tabs.reorder(&[a, 99]));
        assert_eq!(tabs.entries().len(), 2);
    }

    #[test]
    fn closing_original_leaf_keeps_group_identity_and_other_panes() {
        let mut tabs = Tabs::default();
        let root = tabs.insert("root");
        let second = tabs.split(root, "build", Axis::Rows).unwrap();
        let third = tabs.split(second, "log", Axis::Columns).unwrap();
        tabs.select(root);
        assert_eq!(tabs.remove(root), Some("root"));
        assert_eq!(tabs.group_id(second), Some(root));
        assert_eq!(tabs.group_id(third), Some(root));
        assert_eq!(tabs.active_layout().unwrap().leaves(), [second, third]);
        assert_eq!(tabs.active_id(), Some(second));
        assert!(tabs.visible(second) && tabs.visible(third));
        let after = tabs.insert("unrelated");
        assert!(tabs.remove_group(third));
        assert_eq!(tabs.active_id(), Some(after));
        assert!(tabs.get(second).is_none() && tabs.get(third).is_none());
        assert_eq!(tabs.headers().len(), 1);
    }

    #[test]
    fn zoom_and_keyboard_focus_survive_visiting_another_tab() {
        let mut tabs = Tabs::default();
        let left = tabs.insert("left");
        let right = tabs.split(left, "right", Axis::Columns).unwrap();
        assert!(tabs.set_ratio(&[], 0.375));
        assert!(tabs.focus_neighbor(Direction::Left));
        assert!(tabs.toggle_zoom());
        assert!(!tabs.set_ratio(&[], 0.75));
        assert!(!tabs.visible(right));
        assert!(tabs.focus_neighbor(Direction::Right));
        assert!(tabs.visible(right) && !tabs.visible(left));
        let other = tabs.insert("settings");
        assert!(!tabs.zoomed() && !tabs.visible(right));
        tabs.cycle(-1);
        assert_eq!(tabs.active_id(), Some(right));
        assert!(tabs.zoomed() && !tabs.visible(other));
        assert!(tabs.toggle_zoom());
        assert_eq!(
            tabs.active_layout().unwrap().bounds(left).unwrap().right,
            0.375
        );
        assert!(tabs.visible(left));
    }

    #[test]
    fn reordering_tabs_uses_header_membership_and_remembers_pane_focus() {
        let mut tabs = Tabs::default();
        let a = tabs.insert("a");
        let b = tabs.insert("b");
        let c = tabs.split(a, "c", Axis::Columns).unwrap();
        assert!(!tabs.reorder(&[a, c]));
        assert!(tabs.reorder(&[b, c]));
        assert_eq!(tabs.active_index(), Some(1));
        assert_eq!(tabs.active_id(), Some(c));
        tabs.select(a);
        assert_eq!(
            tabs.headers().iter().map(|t| t.id).collect::<Vec<_>>(),
            [b, a]
        );
        assert_eq!(tabs.active_index(), Some(1));
        tabs.select_index(0);
        assert_eq!(tabs.active_id(), Some(b));
        assert!(tabs.remove_group(b));
        assert_eq!(tabs.active_id(), Some(a));
        assert!(tabs.toggle_zoom());
        assert_eq!(tabs.remove(c), Some("c"));
        assert!(!tabs.zoomed());
        assert!(!tabs.toggle_zoom());
        assert!(tabs.visible(a));
    }
}
