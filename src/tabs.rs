//! Ordered tab headers, each with a split layout and a remembered focused entry.
use crate::pane_layout::{Axis, Direction, Layout};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

pub type TabId = u64;

// A pane can change windows without changing its tab or layout IDs.
fn next_id() -> TabId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A user-assigned display name owned by a tab group, independent of its panes.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct TabTitle(String);

impl TabTitle {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for TabTitle {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.trim() != value
            || value.chars().any(char::is_control)
            || value.chars().count() > 128
        {
            return Err("Use 1–128 characters with no control characters or surrounding spaces");
        }
        Ok(Self(value))
    }
}

pub struct Tab<T> {
    pub id: TabId,
    pub value: T,
    group: TabId,
}

struct Group {
    id: TabId,
    title: Option<TabTitle>,
    focused: TabId,
    layout: Layout,
    zoomed: bool,
}

pub struct Tabs<T> {
    entries: Vec<Tab<T>>,
    groups: Vec<Group>,
    active_group: Option<TabId>,
}

impl<T> Default for Tabs<T> {
    fn default() -> Self {
        Self {
            entries: vec![],
            groups: vec![],
            active_group: None,
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
    pub fn active_group_id(&self) -> Option<TabId> {
        self.active_group
    }
    pub fn active_id(&self) -> Option<TabId> {
        Some(self.groups.get(self.active_index()?)?.focused)
    }
    pub fn title_for(&self, entry: TabId) -> Option<&TabTitle> {
        let group = self.group_id(entry)?;
        self.groups.iter().find(|g| g.id == group)?.title.as_ref()
    }

    /// Use the stable group ID so delayed edits cannot follow a moved pane.
    pub fn set_group_title(&mut self, group: TabId, title: Option<TabTitle>) {
        if let Some(g) = self.groups.iter_mut().find(|g| g.id == group) {
            g.title = title;
        }
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
        let id = next_id();
        self.insert_group(id, id, value);
        id
    }

    fn insert_group(&mut self, id: TabId, entry_id: TabId, value: T) {
        self.entries.push(Tab {
            id: entry_id,
            group: id,
            value,
        });
        self.groups.push(Group {
            id,
            title: None,
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
        self.insert_group(next_id(), id, value);
        true
    }

    /// Move the existing entry beside a leaf in another group, then focus it.
    pub fn move_to_existing_tab(&mut self, source: TabId, target: TabId, side: Direction) -> bool {
        let Some(target_group) = self.group_id(target) else {
            return false;
        };
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == source) else {
            return false;
        };
        let source_group = entry.group;
        if source_group == target_group {
            return false;
        }
        let destination = self
            .groups
            .iter_mut()
            .find(|g| g.id == target_group)
            .expect("entry has a group");
        if !destination.layout.insert_beside(target, source, side) {
            return false;
        }
        entry.group = target_group;
        destination.focused = source;
        destination.zoomed = false;
        self.active_group = Some(target_group);
        self.remove_from_layout(source_group, source);
        true
    }

    pub fn split(&mut self, target: TabId, value: T, axis: Axis) -> Option<TabId> {
        let group_id = self.group_id(target)?;
        let group = self.groups.iter_mut().find(|g| g.id == group_id)?;
        let id = next_id();
        if !group.layout.split(target, id, axis) {
            return None;
        }
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
        self.remove_from_layout(removed.group, id);
        Some(removed.value)
    }
    fn remove_from_layout(&mut self, group_id: TabId, id: TabId) {
        let group_index = self
            .groups
            .iter()
            .position(|g| g.id == group_id)
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
    }
    fn remove_header(&mut self, index: usize) -> Group {
        let group = self.groups.remove(index);
        if self.active_group == Some(group.id) {
            self.active_group = self
                .groups
                .get(index)
                .or_else(|| self.groups.last())
                .map(|g| g.id);
        }
        group
    }

    /// Move the owned values and layout into a new window's tab collection.
    /// The stable group ID also works after its original pane moved or closed.
    pub fn take_group(&mut self, group_id: TabId) -> Option<Self> {
        let index = self.groups.iter().position(|group| group.id == group_id)?;
        let group = self.remove_header(index);
        let entries = self
            .entries
            .extract_if(.., |entry| entry.group == group_id)
            .collect();
        Some(Self {
            entries,
            groups: vec![group],
            active_group: Some(group_id),
        })
    }
    /// Return an unclaimed transfer to its previous header position.
    pub fn restore_group(&mut self, mut moved: Self, index: usize, select: bool) {
        for (offset, group) in moved.groups.drain(..).enumerate() {
            if select || self.active_group.is_none() {
                self.active_group = Some(group.id);
            }
            self.groups
                .insert((index + offset).min(self.groups.len()), group);
        }
        self.entries.append(&mut moved.entries);
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
    fn moving_a_group_between_windows_keeps_its_live_values_and_layout() {
        let mut source = Tabs::default();
        let side = source.insert(Box::new("monitor"));
        let group = source.insert(Box::new("SSH"));
        let local = source
            .split(group, Box::new("PowerShell"), Axis::Columns)
            .unwrap();
        let extra = source.split(group, Box::new("logs"), Axis::Rows).unwrap();
        let actor = std::ptr::from_ref(source.get(local).unwrap().as_ref());
        source.remove(group);
        source.select(local);
        assert!(source.set_ratio(&[], 0.375));
        assert!(source.toggle_zoom());
        source.set_group_title(group, Some("Desk".to_owned().try_into().unwrap()));
        let layout = source.active_layout().unwrap().clone();
        let mut target = source.take_group(group).unwrap();
        assert_eq!(source.active_id(), Some(side));
        assert!(source.get(local).is_none() && source.get(extra).is_none());
        assert_eq!(target.active_id(), Some(local));
        assert_eq!(target.group_id(local), Some(group));
        assert_eq!(target.title_for(extra).unwrap().as_str(), "Desk");
        assert_eq!(target.active_layout(), Some(&layout));
        assert!(target.zoomed());
        assert!(std::ptr::eq(actor, target.get(local).unwrap().as_ref()));
        assert!(source.take_group(group).is_none());
        assert_ne!(
            source.insert(Box::new("new")),
            target.insert(Box::new("new"))
        );
        assert!(source.take_group(side).unwrap().get(side).is_some());
    }

    #[test]
    fn returning_an_unclaimed_group_restores_header_order_and_preserves_later_focus() {
        let mut tabs = Tabs::default();
        let first = tabs.insert("first");
        let middle = tabs.insert("middle");
        let last = tabs.insert("last");
        let group = tabs.group_id(middle).unwrap();
        tabs.select(middle);
        let offer = tabs.take_group(group).unwrap();
        tabs.select(first);
        tabs.restore_group(offer, 1, false);
        assert_eq!(tabs.active_id(), Some(first));
        assert_eq!(
            tabs.headers().iter().map(|tab| tab.id).collect::<Vec<_>>(),
            [first, middle, last]
        );
        let offer = tabs.take_group(group).unwrap();
        tabs.restore_group(offer, 1, true);
        assert_eq!(tabs.active_id(), Some(middle));
    }

    #[test]
    fn titles_follow_groups_through_moves_and_closing_the_original_pane() {
        let mut tabs = Tabs::default();
        let source = tabs.insert("SSH");
        let sibling = tabs.split(source, "PowerShell", Axis::Columns).unwrap();
        let destination = tabs.insert("SSH");
        let title = |s: &str| Some(TabTitle::try_from(s.to_owned()).unwrap());
        tabs.set_group_title(source, title("Build"));
        tabs.set_group_title(destination, title("Deploy"));
        assert_eq!(tabs.title_for(sibling), title("Build").as_ref());
        assert!(tabs.move_to_existing_tab(source, destination, Direction::Left));
        assert_eq!(tabs.title_for(source), title("Deploy").as_ref());
        assert_eq!(tabs.title_for(sibling), title("Build").as_ref());
        tabs.remove(destination);
        tabs.set_group_title(destination, None);
        assert!(tabs.title_for(source).is_none());
        assert_eq!(tabs.get(source), Some(&"SSH"));
    }

    #[test]
    fn workspace_titles_accept_unicode_and_validate_on_deserialization() {
        for invalid in ["", "  ", " padded", "a\nb", &"x".repeat(129)] {
            assert!(serde_json::from_value::<TabTitle>(invalid.into()).is_err());
        }
        let name = "界".repeat(128);
        let title: TabTitle = serde_json::from_value(name.clone().into()).unwrap();
        assert_eq!(serde_json::to_value(title).unwrap(), name);
    }
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
    fn moving_a_singleton_preserves_its_entry_and_the_destination_header() {
        for (side, toward_target) in [
            (Direction::Left, Direction::Right),
            (Direction::Right, Direction::Left),
            (Direction::Up, Direction::Down),
            (Direction::Down, Direction::Up),
        ] {
            let mut tabs = Tabs::default();
            let source = tabs.insert(Box::new("SSH"));
            let unrelated = tabs.insert(Box::new("another tab"));
            let target = tabs.insert(Box::new("PowerShell"));
            let identity = std::ptr::from_ref(tabs.get(source).unwrap().as_ref());
            tabs.select(source);
            assert!(tabs.move_to_existing_tab(source, target, side));
            assert_eq!(tabs.active_id(), Some(source));
            assert_eq!(tabs.group_id(source), Some(target));
            assert_eq!(tabs.group_id(target), Some(target));
            assert_eq!(
                tabs.headers().iter().map(|t| t.id).collect::<Vec<_>>(),
                [unrelated, source]
            );
            assert_eq!(tabs.entries().len(), 3);
            assert!(std::ptr::eq(identity, tabs.get(source).unwrap().as_ref()));
            assert!(tabs.visible(source) && tabs.visible(target));
            assert!(tabs.focus_neighbor(toward_target));
            assert_eq!(tabs.active_id(), Some(target));
        }
    }

    #[test]
    fn moving_a_nested_pane_collapses_only_its_old_split_and_reveals_its_new_neighbor() {
        let mut tabs = Tabs::default();
        let old_left = tabs.insert("left");
        let source = tabs.split(old_left, "moving", Axis::Columns).unwrap();
        let old_bottom = tabs.split(source, "bottom", Axis::Rows).unwrap();
        assert!(tabs.set_ratio(&[], 0.375));
        tabs.select(source);
        assert!(tabs.toggle_zoom());

        let target_top = tabs.insert("top");
        let target = tabs.split(target_top, "target", Axis::Rows).unwrap();
        assert!(tabs.set_ratio(&[], 0.75));
        tabs.select(target_top);
        assert!(tabs.toggle_zoom());
        tabs.select(source);
        assert!(tabs.move_to_existing_tab(source, target, Direction::Left));

        let layout = tabs.active_layout().unwrap();
        assert_eq!(layout.leaves(), [target_top, source, target]);
        assert_eq!(layout.bounds(target_top).unwrap().bottom, 0.75);
        assert_eq!(layout.bounds(source).unwrap().right, 0.5);
        assert_eq!(tabs.active_id(), Some(source));
        assert!(!tabs.zoomed());
        let old = tabs.layout_for(old_left).unwrap();
        assert_eq!(old.leaves(), [old_left, old_bottom]);
        assert_eq!(old.bounds(old_left).unwrap().right, 0.375);
        assert!(tabs.select_index(0));
        assert_eq!(tabs.active_id(), Some(old_left));
        assert!(tabs.zoomed());
    }

    #[test]
    fn invalid_moves_preserve_both_groups_focus_and_zoom() {
        let mut tabs = Tabs::default();
        let target = tabs.insert("other");
        let source = tabs.insert("source");
        let sibling = tabs.split(source, "sibling", Axis::Rows).unwrap();
        assert!(tabs.toggle_zoom());
        let original = tabs.active_layout().unwrap().clone();
        for (from, to) in [
            (source, source),
            (source, sibling),
            (99, target),
            (source, 99),
        ] {
            assert!(!tabs.move_to_existing_tab(from, to, Direction::Right));
            assert_eq!(tabs.active_layout(), Some(&original));
            assert_eq!(tabs.layout_for(target), Some(&Layout::Leaf(target)));
            assert_eq!(tabs.active_id(), Some(sibling));
            assert!(tabs.zoomed());
        }
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
