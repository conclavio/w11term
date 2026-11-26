//! Ordered tab headers, each with a split layout and a remembered focused entry.
use crate::options::TabColor;
use crate::pane_layout::{Axis, Direction, Layout};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub type TabId = u64;

/// The divider removed when closing one leaf of a split.
#[derive(Debug)]
pub struct ClosedPaneLocation {
    group: TabId,
    path: Vec<bool>,
    sibling_panes: Vec<TabId>,
    axis: Axis,
    ratio: f64,
    was_second: bool,
}

/// The exact split tree that was visible when the balance action was offered.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneBalance {
    group: TabId,
    layout: Arc<Layout>,
}

pub struct BalancedPanes {
    expected: PaneBalance,
    layout: Layout,
}

impl PaneBalance {
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Supply the current outer extent and one-cell minimum in physical pixels.
    pub fn measure(
        &self,
        divider_px: f64,
        measure: impl FnMut(TabId, Axis) -> Option<(f64, f64)>,
    ) -> Option<BalancedPanes> {
        Some(BalancedPanes {
            layout: self.layout.balanced(divider_px, measure)?,
            expected: self.clone(),
        })
    }
}

impl BalancedPanes {
    pub fn layout(&self) -> &Layout {
        &self.layout
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneRotation {
    group: TabId,
    focus: TabId,
    axis: Axis,
    path: Vec<bool>,
    panes: Vec<TabId>,
}

/// A directional swap offered for a specific pair of visible panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneSwap {
    group: TabId,
    source: TabId,
    neighbor: TabId,
    direction: Direction,
}

impl PaneSwap {
    pub fn panes(self) -> [TabId; 2] {
        [self.source, self.neighbor]
    }
}

// A pane can change windows without changing its tab or layout IDs.
fn next_id() -> TabId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// A user-assigned display name for a tab group or an individual pane.
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
    visit: Option<PaneVisit>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PaneVisit {
    window: u64,
    session: u64,
    order: u64,
}

impl<T> Tab<T> {
    pub fn group_id(&self) -> TabId {
        self.group
    }
}

#[derive(Clone)]
struct Group {
    id: TabId,
    title: Option<TabTitle>,
    color: Option<TabColor>,
    pinned: bool,
    focused: TabId,
    layout: Layout,
    zoomed: bool,
}

/// Layout and launch recipes for a tab. The old pane IDs only identify its
/// layout slots and focus. Opening the recipe allocates all new IDs.
pub struct TabLayoutRecipe<P> {
    index: usize,
    group: Group,
    panes: Vec<P>,
    strip: Vec<TabId>,
}

impl<P> TabLayoutRecipe<P> {
    pub fn panes(&self) -> &[P] {
        &self.panes
    }

    pub fn original_index(&self) -> usize {
        self.index
    }

    /// Reopened tabs have new IDs. Update their slots in other closed layouts.
    pub fn replace_neighbor(&mut self, closed: TabId, reopened: TabId) {
        for id in &mut self.strip {
            if *id == closed {
                *id = reopened;
            }
        }
    }
}

pub struct Tabs<T> {
    entries: Vec<Tab<T>>,
    groups: Vec<Group>,
    active_group: Option<TabId>,
    window: u64,
    visit_order: u64,
}

impl<T> Default for Tabs<T> {
    fn default() -> Self {
        static NEXT_WINDOW: AtomicU64 = AtomicU64::new(1);
        Self {
            entries: vec![],
            groups: vec![],
            active_group: None,
            window: NEXT_WINDOW.fetch_add(1, Ordering::Relaxed),
            visit_order: 0,
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

    /// Record a committed terminal focus, after any background launch or layout
    /// restoration has finished choosing the active pane.
    pub fn record_pane_focus(&mut self, pane: impl Fn(&T) -> Option<u64>) {
        let Some(id) = self.active_id() else { return };
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return;
        };
        let Some(session) = pane(&entry.value) else {
            return;
        };
        let last = PaneVisit {
            window: self.window,
            session,
            order: self.visit_order,
        };
        if entry.visit == Some(last) {
            return;
        }
        self.visit_order += 1;
        entry.visit = Some(PaneVisit {
            order: self.visit_order,
            ..last
        });
    }

    /// A utility page can return to the most recent terminal; a terminal skips
    /// itself. Resolving a command or rendering its availability is read-only.
    pub fn previous_pane(&self, pane: impl Fn(&T) -> Option<u64>) -> Option<TabId> {
        let current = self.active_id();
        self.entries
            .iter()
            .filter_map(|entry| {
                let visit = entry.visit?;
                // A transferred pane can return if its new window fails to open.
                // Its original visit counts only in that window, and only while
                // its session still occupies the same tab entry.
                (Some(entry.id) != current
                    && visit.window == self.window
                    && pane(&entry.value) == Some(visit.session))
                .then_some((visit.order, entry.id))
            })
            .max()
            .map(|(_, id)| id)
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

    /// No override follows the focused pane's profile. An explicit None color hides it.
    pub fn group_color(&self, group: TabId) -> Option<TabColor> {
        self.groups.iter().find(|g| g.id == group)?.color
    }

    pub fn set_group_color(&mut self, group: TabId, color: Option<TabColor>) -> bool {
        let Some(group) = self.groups.iter_mut().find(|g| g.id == group) else {
            return false;
        };
        if group.color == color {
            return false;
        }
        group.color = color;
        true
    }

    pub fn group_is_pinned(&self, group: TabId) -> bool {
        self.groups.iter().any(|g| g.id == group && g.pinned)
    }

    pub fn set_group_pinned(&mut self, group: TabId, pinned: bool) -> bool {
        let Some(index) = self.groups.iter().position(|g| g.id == group) else {
            return false;
        };
        if self.groups[index].pinned == pinned {
            return false;
        }
        let mut group = self.groups.remove(index);
        group.pinned = pinned;
        self.groups.insert(self.pinned_group_count(), group);
        true
    }

    fn pinned_group_count(&self) -> usize {
        self.groups.partition_point(|group| group.pinned)
    }

    fn group_insert_index(&self, index: usize, pinned: bool) -> usize {
        let boundary = self.pinned_group_count();
        if pinned {
            index.min(boundary)
        } else {
            index.clamp(boundary, self.groups.len())
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
        self.group_layout(self.group_id(entry)?)
    }
    pub fn group_layout(&self, group: TabId) -> Option<&Layout> {
        self.groups
            .iter()
            .find(|g| g.id == group)
            .map(|g| &g.layout)
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

    pub fn pane_swap(&self, direction: Direction) -> Option<PaneSwap> {
        let group = self.groups.get(self.active_index()?)?;
        if group.zoomed {
            return None;
        }
        Some(PaneSwap {
            group: group.id,
            source: group.focused,
            neighbor: group.layout.neighbor(group.focused, direction)?,
            direction,
        })
    }

    pub fn pane_rotation(&self) -> Option<PaneRotation> {
        let group = self.groups.get(self.active_index()?)?;
        if group.zoomed {
            return None;
        }
        let (path, parent) = group.layout.parent_of(group.focused)?;
        let Layout::Split { axis, .. } = parent else {
            return None;
        };
        Some(PaneRotation {
            group: group.id,
            focus: group.focused,
            axis: *axis,
            path,
            panes: parent.leaves(),
        })
    }

    /// Keep both child trees and their divider ratio. Return the panes whose geometry changed.
    pub fn rotate_split(&mut self, expected: PaneRotation) -> Option<Vec<TabId>> {
        if self.pane_rotation().as_ref() != Some(&expected) {
            return None;
        }
        let index = self.active_index()?;
        let Layout::Split { axis, .. } = self.groups[index].layout.node_mut(&expected.path)? else {
            return None;
        };
        *axis = match *axis {
            Axis::Columns => Axis::Rows,
            Axis::Rows => Axis::Columns,
        };
        Some(expected.panes)
    }

    pub fn pane_balance(&self) -> Option<PaneBalance> {
        let group = self.groups.get(self.active_index()?)?;
        (!group.zoomed && matches!(group.layout, Layout::Split { .. })).then(|| PaneBalance {
            group: group.id,
            layout: Arc::new(group.layout.clone()),
        })
    }

    pub fn balance_panes(&mut self, plan: BalancedPanes) -> bool {
        let Some(index) = self.active_index() else {
            return false;
        };
        let group = &mut self.groups[index];
        if group.zoomed || group.id != plan.expected.group || group.layout != *plan.expected.layout
        {
            return false;
        }
        group.layout = plan.layout;
        true
    }

    pub fn swap_panes(&mut self, target: PaneSwap) -> bool {
        if self.pane_swap(target.direction) != Some(target) {
            return false;
        }
        let index = self.active_index().expect("validated tab is active");
        self.groups[index].layout.replace_leaves(&mut |id| {
            if id == target.source {
                target.neighbor
            } else if id == target.neighbor {
                target.source
            } else {
                id
            }
        });
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
        self.insert_group(id, id, value, None);
        id
    }

    /// Capture a group before closing it, even if the header is inactive or
    /// its original pane has moved. An ineligible leaf rejects the whole group.
    pub fn capture_group<P, E>(
        &self,
        group: TabId,
        mut capture: impl FnMut(&T) -> Result<P, E>,
    ) -> Result<Option<TabLayoutRecipe<P>>, E> {
        let Some(index) = self.groups.iter().position(|g| g.id == group) else {
            return Ok(None);
        };
        let group = &self.groups[index];
        let panes = group
            .layout
            .leaves()
            .into_iter()
            .map(|id| capture(self.get(id).expect("layout leaf belongs to the group")))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(TabLayoutRecipe {
            index,
            group: group.clone(),
            panes,
            strip: self.groups.iter().map(|group| group.id).collect(),
        }))
    }

    /// Recheck all saved launches before starting any pane. Borrowing the recipe
    /// lets the caller keep it for a retry if preflight fails.
    pub fn reopen_group<P, L, E>(
        &mut self,
        recipe: &TabLayoutRecipe<P>,
        prepare: impl FnMut(&P) -> Result<L, E>,
        launch: impl FnMut(L) -> T,
    ) -> Result<TabId, E> {
        let panes = recipe
            .panes
            .iter()
            .map(prepare)
            .collect::<Result<Vec<_>, _>>()?;
        let indices: HashMap<_, _> = self
            .groups
            .iter()
            .enumerate()
            .map(|(index, group)| (group.id, index))
            .collect();
        let left = recipe.strip[..recipe.index]
            .iter()
            .rev()
            .find_map(|id| indices.get(id).copied());
        let right = recipe.strip[recipe.index + 1..]
            .iter()
            .find_map(|id| indices.get(id).copied());
        let index = match (left, right) {
            (Some(left), Some(right)) if left >= right => recipe.index,
            (_, Some(right)) => right,
            (Some(left), None) => left + 1,
            (None, None) => recipe.index,
        };
        Ok(self.insert_fresh_group(index, recipe.group.clone(), panes, launch))
    }

    pub fn duplicate_active_group<P, E>(
        &mut self,
        prepare: impl FnMut(&T) -> Result<P, E>,
        launch: impl FnMut(P) -> T,
    ) -> Result<Option<TabId>, E> {
        let Some(group) = self.active_group_id() else {
            return Ok(None);
        };
        self.duplicate_group(group, prepare, launch)
    }

    pub fn duplicate_group<P, E>(
        &mut self,
        group: TabId,
        prepare: impl FnMut(&T) -> Result<P, E>,
        launch: impl FnMut(P) -> T,
    ) -> Result<Option<TabId>, E> {
        let Some(recipe) = self.capture_group(group, prepare)? else {
            return Ok(None);
        };
        Ok(Some(self.insert_fresh_group(
            recipe.index + 1,
            recipe.group,
            recipe.panes,
            launch,
        )))
    }

    fn insert_fresh_group<P>(
        &mut self,
        index: usize,
        source_group: Group,
        panes: Vec<P>,
        mut launch: impl FnMut(P) -> T,
    ) -> TabId {
        let mut recipes = panes.into_iter();
        let mut layout = source_group.layout;
        let mut group = None;
        let mut focused = None;
        layout.replace_leaves(&mut |source| {
            let id = next_id();
            let group = *group.get_or_insert(id);
            if source_group.focused == source {
                focused = Some(id);
            }
            self.entries.push(Tab {
                id,
                group,
                value: launch(recipes.next().expect("one recipe per leaf")),
                visit: None,
            });
            id
        });
        let id = group.expect("every tab group has a leaf");
        // Duplicate and reopen create fresh tabs. Pinning protects a specific live tab.
        self.groups.insert(
            self.group_insert_index(index, false),
            Group {
                id,
                title: source_group.title,
                color: source_group.color,
                pinned: false,
                focused: focused.expect("the focused entry belongs to this layout"),
                layout,
                zoomed: source_group.zoomed,
            },
        );
        self.active_group = Some(id);
        id
    }

    fn insert_group(&mut self, id: TabId, entry_id: TabId, value: T, visit: Option<PaneVisit>) {
        self.entries.push(Tab {
            id: entry_id,
            group: id,
            value,
            visit,
        });
        self.groups.push(Group {
            id,
            title: None,
            color: None,
            pinned: false,
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
        let entry = self.take_entry(id).expect("focused pane exists");
        self.insert_group(next_id(), id, entry.value, entry.visit);
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
            visit: None,
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
        self.take_entry(id).map(|entry| entry.value)
    }

    pub fn closed_pane_location(&self, pane: TabId) -> Option<ClosedPaneLocation> {
        let group = self.group_id(pane)?;
        let (path, parent) = self.layout_for(pane)?.parent_of(pane)?;
        let Layout::Split {
            first,
            second,
            axis,
            ratio,
        } = parent
        else {
            return None;
        };
        let was_second = matches!(second.as_ref(), Layout::Leaf(id) if *id == pane);
        let sibling = if was_second { first } else { second };
        Some(ClosedPaneLocation {
            group,
            path,
            sibling_panes: sibling.leaves(),
            axis: *axis,
            ratio: *ratio,
            was_second,
        })
    }

    /// Restore the removed split if the surviving subtree is still there.
    /// Open a separate tab if reopening would split an unrelated set of panes.
    pub fn reopen_pane(&mut self, location: &ClosedPaneLocation, value: T) -> TabId {
        let original = self.groups.iter_mut().find(|g| g.id == location.group);
        if let Some(group) = original
            && let Some(node) = group.layout.node_mut(&location.path)
            && node.leaves() == location.sibling_panes
        {
            let id = next_id();
            let old = Box::new(std::mem::replace(node, Layout::Leaf(id)));
            let new = Box::new(Layout::Leaf(id));
            let (first, second) = if location.was_second {
                (old, new)
            } else {
                (new, old)
            };
            *node = Layout::Split {
                axis: location.axis,
                ratio: location.ratio,
                first,
                second,
            };
            self.entries.push(Tab {
                id,
                group: location.group,
                value,
                visit: None,
            });
            group.focused = id;
            group.zoomed = false;
            self.active_group = Some(location.group);
            return id;
        }
        self.insert(value)
    }

    fn take_entry(&mut self, id: TabId) -> Option<Tab<T>> {
        let index = self.entries.iter().position(|t| t.id == id)?;
        let removed = self.entries.remove(index);
        self.remove_from_layout(removed.group, id);
        Some(removed)
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
            ..Self::default()
        })
    }
    /// Return an unclaimed transfer to its previous header position.
    pub fn restore_group(&mut self, mut moved: Self, index: usize, select: bool) {
        for (offset, group) in moved.groups.drain(..).enumerate() {
            if select || self.active_group.is_none() {
                self.active_group = Some(group.id);
            }
            let index = self.group_insert_index(index + offset, group.pinned);
            self.groups.insert(index, group);
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
        self.groups
            .sort_by_key(|group| (!group.pinned, positions[&group.id]));
        true
    }

    fn active_tab_move(&self, direction: Direction) -> Option<(usize, usize)> {
        let index = self.active_index()?;
        let neighbor = match direction {
            Direction::Left if index > 0 => index - 1,
            Direction::Right if index + 1 < self.groups.len() => index + 1,
            _ => return None,
        };
        (self.groups[index].pinned == self.groups[neighbor].pinned).then_some((index, neighbor))
    }

    pub fn can_move_active_tab(&self, direction: Direction) -> bool {
        self.active_tab_move(direction).is_some()
    }

    pub fn move_active_tab(&mut self, direction: Direction) -> bool {
        let Some((index, neighbor)) = self.active_tab_move(direction) else {
            return false;
        };
        self.groups.swap(index, neighbor);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_color_override_follows_the_group_and_its_layout_recipe() {
        let mut tabs = Tabs::default();
        let group = tabs.insert("SSH");
        let local = tabs.split(group, "PowerShell", Axis::Columns).unwrap();
        assert!(tabs.set_group_color(group, Some(TabColor::Purple)));
        assert_eq!(
            tabs.group_color(tabs.group_id(local).unwrap()),
            Some(TabColor::Purple)
        );
        assert!(!tabs.set_group_color(group, Some(TabColor::Purple)));
        let duplicate = tabs
            .duplicate_active_group(|name| Ok::<_, ()>(*name), |name| name)
            .unwrap()
            .unwrap();
        assert_eq!(tabs.group_color(duplicate), Some(TabColor::Purple));

        let mut other = Tabs::default();
        other.restore_group(tabs.take_group(group).unwrap(), 0, true);
        assert_eq!(other.active_id(), Some(local));
        assert_eq!(other.group_color(group), Some(TabColor::Purple));
        assert!(other.set_group_color(group, Some(TabColor::None)));
        assert_eq!(other.group_color(group), Some(TabColor::None));
        assert!(other.set_group_color(group, None));
        assert_eq!(other.group_color(group), None);
        assert_eq!(tabs.group_color(duplicate), Some(TabColor::Purple));
    }

    #[test]
    fn pinned_groups_stay_in_front_during_moves_reorder_transfer_and_duplicate() {
        fn order(tabs: &Tabs<&str>) -> Vec<TabId> {
            tabs.headers().iter().map(|tab| tab.id).collect()
        }

        let mut tabs = Tabs::default();
        let alpha = tabs.insert("alpha");
        let beta = tabs.insert("beta");
        let gamma = tabs.insert("gamma");
        assert!(tabs.set_group_pinned(gamma, true));
        assert!(tabs.set_group_pinned(beta, true));
        assert!(!tabs.set_group_pinned(beta, true));
        assert_eq!(tabs.active_id(), Some(gamma));
        assert_eq!(order(&tabs), [gamma, beta, alpha]);
        assert!(tabs.move_active_tab(Direction::Right));
        assert!(!tabs.move_active_tab(Direction::Right));
        assert_eq!(order(&tabs), [beta, gamma, alpha]);
        assert!(tabs.reorder(&[alpha, gamma, beta]));
        assert_eq!(order(&tabs), [gamma, beta, alpha]);

        let duplicate = tabs
            .duplicate_active_group(|value| Ok::<_, ()>(*value), |value| value)
            .unwrap()
            .unwrap();
        assert!(!tabs.group_is_pinned(duplicate));
        assert_eq!(order(&tabs), [gamma, beta, duplicate, alpha]);
        assert!(tabs.set_group_pinned(beta, false));
        assert_eq!(order(&tabs), [gamma, beta, duplicate, alpha]);

        let mut other = Tabs::default();
        let pinned = other.insert("builds");
        other.set_group_pinned(pinned, true);
        let regular = other.insert("logs");
        other.restore_group(tabs.take_group(gamma).unwrap(), 2, true);
        assert_eq!(order(&other), [pinned, gamma, regular]);
        assert_eq!(other.active_id(), Some(gamma));
        assert!(other.group_is_pinned(gamma));
    }

    #[test]
    fn previous_pane_toggles_committed_terminal_visits_across_tabs_and_splits() {
        let mut tabs = Tabs::default();
        let pane = |value: &Option<u64>| *value;
        let ssh = tabs.insert(Some(101));
        tabs.record_pane_focus(pane);
        assert_eq!(tabs.previous_pane(pane), None);
        let _unvisited = tabs.split(ssh, Some(102), Axis::Rows).unwrap();
        let shell = tabs.insert(Some(103));
        tabs.record_pane_focus(pane);
        tabs.record_pane_focus(pane);
        for expected in [ssh, shell, ssh] {
            assert_eq!(tabs.previous_pane(pane), Some(expected));
            assert!(tabs.select(expected));
            tabs.record_pane_focus(pane);
        }
        let _settings = tabs.insert(None);
        tabs.record_pane_focus(pane);
        assert_eq!(tabs.previous_pane(pane), Some(ssh));
        assert!(tabs.select(ssh));
        tabs.record_pane_focus(pane);
        assert_eq!(tabs.previous_pane(pane), Some(shell));
        // A new session occupies the old entry but hasn't received focus.
        *tabs.get_mut(shell).unwrap() = Some(104);
        assert_eq!(tabs.previous_pane(pane), None);
    }

    #[test]
    fn previous_pane_falls_back_in_visit_order_and_stays_in_its_window() {
        let mut tabs = Tabs::default();
        let pane = |value: &u64| Some(*value);
        let a = tabs.insert(10);
        tabs.record_pane_focus(pane);
        let b = tabs.split(a, 20, Axis::Columns).unwrap();
        tabs.record_pane_focus(pane);
        let c = tabs.insert(30);
        tabs.record_pane_focus(pane);
        let d = tabs.split(c, 40, Axis::Rows).unwrap();
        tabs.record_pane_focus(pane);
        assert!(tabs.move_active_to_new_tab());
        assert_eq!(tabs.previous_pane(pane), Some(c));
        assert!(tabs.move_to_existing_tab(d, a, Direction::Right));
        assert_eq!(tabs.previous_pane(pane), Some(c));
        tabs.remove(c);
        assert_eq!(tabs.previous_pane(pane), Some(b));
        let mut moved = tabs.take_group(tabs.group_id(a).unwrap()).unwrap();
        assert_eq!(tabs.previous_pane(pane), None);
        assert_eq!(moved.previous_pane(pane), None);
        moved.record_pane_focus(pane);
        assert!(moved.select(a));
        moved.record_pane_focus(pane);
        assert_eq!(moved.previous_pane(pane), Some(d));
        assert!(moved.remove_group(a));
        assert_eq!(moved.previous_pane(pane), None);
    }

    #[test]
    fn previous_pane_recovers_visits_after_an_unclaimed_window_transfer() {
        let mut tabs = Tabs::default();
        let pane = |value: &u64| Some(*value);
        let a = tabs.insert(10);
        tabs.record_pane_focus(pane);
        let b = tabs.insert(20);
        tabs.record_pane_focus(pane);
        let c = tabs.insert(30);
        tabs.record_pane_focus(pane);
        let offered = tabs.take_group(tabs.group_id(b).unwrap()).unwrap();
        assert_eq!(tabs.previous_pane(pane), Some(a));
        assert!(tabs.select(a));
        tabs.record_pane_focus(pane);
        tabs.restore_group(offered, 1, false);
        assert_eq!(tabs.previous_pane(pane), Some(c));
        tabs.remove(c);
        assert_eq!(tabs.previous_pane(pane), Some(b));
    }

    #[test]
    fn balancing_preserves_live_panes_and_focus_and_rejects_stale_actions() {
        let mut tabs = Tabs::default();
        let a = tabs.insert("SSH");
        assert!(tabs.pane_balance().is_none());
        let b = tabs.split(a, "PowerShell", Axis::Columns).unwrap();
        let c = tabs.split(b, "Log", Axis::Columns).unwrap();
        let target = tabs.pane_balance().unwrap();
        let plan = || target.measure(6.0, |_, _| Some((100.0, 12.0))).unwrap();

        let other = tabs.insert("Other tab");
        assert!(!tabs.balance_panes(plan()));
        tabs.select(a);
        tabs.toggle_zoom();
        assert!(tabs.pane_balance().is_none());
        assert!(!tabs.balance_panes(plan()));
        tabs.toggle_zoom();
        tabs.set_ratio(&[true], 0.68);
        assert!(!tabs.balance_panes(plan()));
        tabs.set_ratio(&[true], 0.5);
        assert_eq!(tabs.active_layout(), Some(target.layout()));

        // Focusing a different leaf of the same group keeps the user's focus.
        tabs.select(b);
        assert!(tabs.balance_panes(plan()));
        assert_eq!(tabs.active_id(), Some(b));
        assert_eq!(tabs.active_layout().unwrap().leaves(), [a, b, c]);
        assert_eq!(
            [tabs.get(a), tabs.get(b), tabs.get(c)],
            [Some(&"SSH"), Some(&"PowerShell"), Some(&"Log")]
        );
        assert_eq!(tabs.layout_for(other), Some(&Layout::Leaf(other)));
        assert!(!tabs.balance_panes(plan()));
    }

    #[test]
    fn rotate_only_the_nearest_split_preserves_sessions_focus_and_ratios() {
        let mut tabs = Tabs::default();
        let left = tabs.insert("SSH");
        let upper = tabs.split(left, "log", Axis::Columns).unwrap();
        let lower = tabs.split(upper, "PowerShell", Axis::Rows).unwrap();
        tabs.set_ratio(&[], 0.6);
        tabs.set_ratio(&[true], 0.3);
        let original = tabs.active_layout().unwrap().clone();
        let target = tabs.pane_rotation().unwrap();
        assert_eq!(tabs.rotate_split(target.clone()), Some(vec![upper, lower]));
        assert_eq!(tabs.active_id(), Some(lower));
        let layout = tabs.active_layout().unwrap();
        assert_eq!(layout.bounds(left), original.bounds(left));
        assert_eq!(layout.neighbor(lower, Direction::Left), Some(upper));
        assert_eq!(layout.neighbor(lower, Direction::Up), None);
        assert_eq!(tabs.rotate_split(target), None, "stale pre-rotation action");
        assert!(tabs.rotate_split(tabs.pane_rotation().unwrap()).is_some());
        assert_eq!(tabs.active_layout(), Some(&original));

        // Rotating from the outer leaf carries its entire sibling subtree intact.
        tabs.select(left);
        let outer = tabs.pane_rotation().unwrap();
        assert_eq!(tabs.rotate_split(outer), Some(vec![left, upper, lower]));
        let Layout::Split {
            axis,
            ratio,
            second,
            ..
        } = tabs.active_layout().unwrap()
        else {
            unreachable!()
        };
        assert_eq!((*axis, *ratio), (Axis::Rows, 0.6));
        let Layout::Split {
            second: original_subtree,
            ..
        } = &original
        else {
            unreachable!()
        };
        assert_eq!(second, original_subtree);
    }

    #[test]
    fn rotate_rejects_stale_focus_zoom_and_sibling_membership() {
        let mut tabs = Tabs::default();
        let a = tabs.insert(0);
        assert!(tabs.pane_rotation().is_none());
        let b = tabs.split(a, 1, Axis::Columns).unwrap();
        tabs.select(a);
        let pending = tabs.pane_rotation().unwrap();
        tabs.select(b);
        assert!(tabs.rotate_split(pending.clone()).is_none());
        tabs.select(a);
        tabs.toggle_zoom();
        assert!(tabs.pane_rotation().is_none());
        assert!(tabs.rotate_split(pending.clone()).is_none());
        tabs.toggle_zoom();
        tabs.split(b, 2, Axis::Rows);
        tabs.select(a);
        assert!(tabs.rotate_split(pending).is_none());
    }

    #[test]
    fn swap_exchanges_only_neighboring_rectangles_and_keeps_the_focused_session() {
        let mut tabs = Tabs::default();
        let ssh = tabs.insert("SSH");
        let logs = tabs.split(ssh, "logs", Axis::Columns).unwrap();
        let shell = tabs.split(logs, "PowerShell", Axis::Rows).unwrap();
        tabs.set_ratio(&[], 0.62);
        tabs.set_ratio(&[true], 0.3);
        tabs.select(ssh);
        let before = tabs.active_layout().unwrap().clone();
        let target = tabs.pane_swap(Direction::Right).unwrap();
        assert_eq!(target.neighbor, shell); // The lower pane has the longer shared edge.
        assert!(tabs.swap_panes(target));
        let after = tabs.active_layout().unwrap();
        assert_eq!(after.bounds(ssh), before.bounds(shell));
        assert_eq!(after.bounds(shell), before.bounds(ssh));
        assert_eq!(after.bounds(logs), before.bounds(logs));
        assert_eq!(after.leaves(), [shell, logs, ssh]);
        assert_eq!(tabs.active(), Some(&"SSH"));
        assert_eq!(tabs.active_id(), Some(ssh));
        assert_eq!(tabs.get(shell), Some(&"PowerShell"));
        assert!(!tabs.swap_panes(target));
        let reverse = tabs.pane_swap(Direction::Left).unwrap();
        assert!(tabs.swap_panes(reverse));
        assert_eq!(tabs.active_layout(), Some(&before));
    }

    #[test]
    fn swap_requires_the_same_visible_source_and_neighbor_at_dispatch() {
        let mut tabs = Tabs::default();
        let a = tabs.insert("a");
        assert!(tabs.pane_swap(Direction::Right).is_none());
        let b = tabs.split(a, "b", Axis::Columns).unwrap();
        let target = tabs.pane_swap(Direction::Left).unwrap();
        let before = tabs.active_layout().unwrap().clone();
        assert!(tabs.pane_swap(Direction::Right).is_none());

        tabs.select(a);
        assert!(!tabs.swap_panes(target));
        tabs.select(b);
        assert!(tabs.toggle_zoom());
        assert!(tabs.pane_swap(Direction::Left).is_none());
        assert!(!tabs.swap_panes(target));
        assert!(tabs.toggle_zoom());
        let elsewhere = tabs.insert("another tab");
        assert!(!tabs.swap_panes(target));
        assert_eq!(tabs.active_id(), Some(elsewhere));
        tabs.select(b);
        assert_eq!(tabs.active_layout(), Some(&before));

        // The same source can acquire a different neighbor while a flyout is open.
        let replacement = tabs.split(a, "new neighbor", Axis::Columns).unwrap();
        tabs.select(b);
        assert_eq!(
            tabs.pane_swap(Direction::Left).unwrap().neighbor,
            replacement
        );
        let changed = tabs.active_layout().unwrap().clone();
        assert!(!tabs.swap_panes(target));
        assert_eq!(tabs.active_layout(), Some(&changed));
    }

    #[test]
    fn duplicate_group_preflights_all_leaves_and_preserves_nested_layout_focus_zoom_and_order() {
        let mut tabs = Tabs::default();
        let before = tabs.insert(0);
        let a = tabs.insert(1);
        let b = tabs.split(a, 2, Axis::Rows).unwrap();
        let c = tabs.split(b, 3, Axis::Columns).unwrap();
        let after = tabs.insert(4);
        tabs.select(b);
        assert!(tabs.set_ratio(&[], 0.4123));
        assert!(tabs.set_ratio(&[true], 0.7331));
        assert!(tabs.toggle_zoom());
        let title = TabTitle::try_from("Operations".to_owned()).unwrap();
        tabs.set_group_title(a, Some(title.clone()));
        let original = tabs.active_layout().unwrap().clone();
        let rejected = tabs.duplicate_active_group(
            |value| {
                if *value == 3 {
                    Err("unsupported pane")
                } else {
                    Ok(*value)
                }
            },
            |_| panic!("no shell may start before preflight succeeds"),
        );
        assert_eq!(rejected, Err("unsupported pane"));
        assert_eq!(tabs.active_id(), Some(b));
        assert_eq!(tabs.headers().len(), 3);
        let hd = tabs
            .duplicate_active_group(|value| Ok::<_, ()>(*value), |value| value + 10)
            .unwrap()
            .unwrap();
        let ids = tabs.active_layout().unwrap().leaves();
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().all(|id| ![a, b, c].contains(id)));
        assert_eq!(tabs.active_id(), Some(ids[1]));
        assert_eq!(tabs.title_for(ids[1]), Some(&title));
        assert!(tabs.zoomed());
        assert_eq!(
            tabs.headers()
                .iter()
                .map(|tab| tab.group_id())
                .collect::<Vec<_>>(),
            [before, a, hd, after]
        );
        assert_eq!(
            ids.iter()
                .map(|id| *tabs.get(*id).unwrap())
                .collect::<Vec<_>>(),
            [11, 12, 13]
        );
        for (old, new) in original.leaves().into_iter().zip(&ids) {
            assert_eq!(
                tabs.active_layout().unwrap().bounds(*new),
                original.bounds(old)
            );
        }
        tabs.remove_group(ids[1]);
        assert_eq!(tabs.layout_for(a), Some(&original));
        assert_eq!(tabs.get(b), Some(&2));
    }

    #[test]
    fn reopen_preflights_a_background_group_and_restores_its_position_with_new_panes() {
        let mut tabs = Tabs::default();
        let before = tabs.insert(("before", false, 0));
        let group = tabs.insert(("closed leaf", false, 0));
        let ssh = tabs.split(group, ("SSH", true, 1), Axis::Rows).unwrap();
        let local = tabs
            .split(ssh, ("PowerShell", false, 2), Axis::Columns)
            .unwrap();
        tabs.remove(group);
        assert!(tabs.select(ssh));
        assert!(tabs.set_ratio(&[], 0.37));
        let geometry = tabs.active_layout().unwrap().clone();
        assert!(tabs.toggle_zoom());
        let title = TabTitle::try_from("Operations".to_owned()).unwrap();
        tabs.set_group_title(group, Some(title.clone()));
        let after = tabs.insert(("after", false, 0));

        // Header IDs outlive their first pane, and closing a background tab must
        // capture its leaves without switching or changing keyboard focus.
        assert!(tabs.get(group).is_none());
        let rejected = tabs.capture_group(group, |&(profile, _, _)| match profile {
            "PowerShell" => Err("unsupported launch"),
            _ => Ok(profile),
        });
        assert!(matches!(rejected, Err("unsupported launch")));
        let recipe = tabs
            .capture_group(group, |&(profile, locked, _)| {
                Ok::<_, ()>((profile, locked))
            })
            .unwrap()
            .unwrap();
        assert_eq!(tabs.active_id(), Some(after));
        assert!(tabs.remove_group(local));

        let failed = tabs.reopen_group(
            &recipe,
            |&(profile, _)| match profile {
                "PowerShell" => Err("working directory no longer exists"),
                _ => Ok(()),
            },
            |_| panic!("failed preflight started a shell"),
        );
        assert_eq!(failed, Err("working directory no longer exists"));
        assert_eq!(tabs.active_id(), Some(after));
        assert_eq!(tabs.headers().len(), 2);

        let reopened = tabs
            .reopen_group(
                &recipe,
                |pane| Ok::<_, ()>(*pane),
                |(profile, locked)| (profile, locked, 3),
            )
            .unwrap();
        let layout = tabs.active_layout().unwrap();
        let ids = layout.leaves();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| ![group, ssh, local].contains(id)));
        assert_eq!(tabs.active_id(), Some(ids[0]));
        assert_eq!(tabs.get(ids[0]), Some(&("SSH", true, 3)));
        assert_eq!(tabs.get(ids[1]), Some(&("PowerShell", false, 3)));
        assert_eq!(tabs.title_for(ids[0]), Some(&title));
        assert!(tabs.zoomed());
        assert_eq!(layout.bounds(ids[0]), geometry.bounds(ssh));
        assert_eq!(layout.bounds(ids[1]), geometry.bounds(local));
        assert_eq!(
            tabs.headers()
                .iter()
                .map(|t| t.group_id())
                .collect::<Vec<_>>(),
            [before, reopened, after]
        );
    }

    #[test]
    fn reopen_appends_when_intervening_closes_remove_the_old_slot() {
        let mut tabs = Tabs::default();
        let first = tabs.insert(1);
        let intervening = tabs.insert(2);
        let last = tabs.insert(3);
        let recipe = tabs
            .capture_group(last, |&n| Ok::<_, ()>(n))
            .unwrap()
            .unwrap();
        assert!(tabs.remove_group(last));
        assert!(tabs.remove_group(intervening));
        let restored = tabs
            .reopen_group(&recipe, |&n| Ok::<_, ()>(n), |n| n)
            .unwrap();
        assert_eq!(
            tabs.headers().iter().map(|t| t.id).collect::<Vec<_>>(),
            [first, restored]
        );
        assert_eq!(tabs.active(), Some(&3));
    }

    #[test]
    fn reopening_a_batch_out_of_order_recovers_the_original_neighbors() {
        let mut tabs = Tabs::default();
        let [first, middle_left, middle_right, last] =
            ["first", "left", "right", "last"].map(|label| tabs.insert(label));
        let mut left = tabs
            .capture_group(middle_left, |&label| Ok::<_, ()>(label))
            .unwrap()
            .unwrap();
        let right = tabs
            .capture_group(middle_right, |&label| Ok::<_, ()>(label))
            .unwrap()
            .unwrap();
        assert!(tabs.remove_group(middle_left));
        assert!(tabs.remove_group(middle_right));
        let right_restored = tabs
            .reopen_group(&right, |&label| Ok::<_, ()>(label), |label| label)
            .unwrap();
        assert_eq!(
            tabs.headers().iter().map(|t| t.id).collect::<Vec<_>>(),
            [first, right_restored, last]
        );

        left.replace_neighbor(middle_right, right_restored);
        let left_restored = tabs
            .reopen_group(&left, |&label| Ok::<_, ()>(label), |label| label)
            .unwrap();
        assert_eq!(
            tabs.headers().iter().map(|t| t.id).collect::<Vec<_>>(),
            [first, left_restored, right_restored, last]
        );
    }

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
    fn moving_the_active_header_keeps_its_zoomed_split_focus_and_visit_history() {
        use Direction::{Left, Right};
        let mut tabs = Tabs::default();
        let pane = |value: &Option<u64>| *value;
        assert!(!tabs.can_move_active_tab(Right));
        assert!(!tabs.move_active_tab(Left));
        let first = tabs.insert(Some(11));
        tabs.record_pane_focus(pane);
        assert!(!tabs.move_active_tab(Right));
        let ssh = tabs.insert(Some(12));
        let shell = tabs.split(ssh, Some(13), Axis::Columns).unwrap();
        tabs.record_pane_focus(pane);
        tabs.set_group_title(ssh, Some("Builds".to_owned().try_into().unwrap()));
        assert!(tabs.set_ratio(&[], 0.375));
        assert!(tabs.toggle_zoom());
        let layout = tabs.active_layout().unwrap().clone();
        let utility = tabs.insert(None);
        tabs.select(shell);
        let headers =
            |tabs: &Tabs<Option<u64>>| tabs.headers().iter().map(|t| t.id).collect::<Vec<_>>();
        assert!(tabs.can_move_active_tab(Right));
        assert!(tabs.move_active_tab(Right));
        assert_eq!(headers(&tabs), [first, utility, shell]);
        assert!(!tabs.can_move_active_tab(Right));
        assert!(!tabs.move_active_tab(Right));
        assert_eq!(tabs.active_id(), Some(shell));
        assert_eq!(tabs.layout_for(shell), Some(&layout));
        assert_eq!(tabs.title_for(shell).unwrap().as_str(), "Builds");
        assert_eq!(tabs.previous_pane(pane), Some(first));
        assert!(tabs.zoomed());

        tabs.select_index(1);
        assert_eq!(tabs.active_id(), Some(utility));
        assert!(tabs.move_active_tab(Left));
        assert!(!tabs.move_active_tab(Left));
        assert_eq!(headers(&tabs), [utility, first, shell]);
        assert_eq!(tabs.previous_pane(pane), Some(shell));
        tabs.select_index(2);
        assert_eq!(tabs.active_id(), Some(shell));
        assert!(tabs.zoomed());
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
        assert!(!tabs.reorder(&[a, next_id()]));
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
    fn reopening_one_pane_restores_its_divider_without_replacing_surviving_panes() {
        let mut tabs = Tabs::default();
        let closed = tabs.insert("local");
        let ssh = tabs.split(closed, "ssh", Axis::Columns).unwrap();
        let logs = tabs.split(ssh, "logs", Axis::Rows).unwrap();
        tabs.set_ratio(&[], 0.375);
        tabs.set_ratio(&[true], 0.62);
        let location = tabs.closed_pane_location(closed).unwrap();
        assert_eq!(tabs.remove(closed), Some("local"));
        assert!(tabs.set_ratio(&[], 0.7));
        let unrelated = tabs.insert("unrelated");

        let reopened = tabs.reopen_pane(&location, "fresh local");
        assert_ne!(reopened, closed);
        assert_eq!(tabs.active_id(), Some(reopened));
        assert_eq!(tabs.group_id(reopened), Some(closed));
        assert_eq!(
            tabs.active_layout().unwrap().leaves(),
            [reopened, ssh, logs]
        );
        assert_eq!(tabs.get(ssh), Some(&"ssh"));
        assert_eq!(tabs.get(logs), Some(&"logs"));
        assert_eq!(tabs.get(unrelated), Some(&"unrelated"));
        let Layout::Split { ratio, second, .. } = tabs.active_layout().unwrap() else {
            panic!("original root divider was lost");
        };
        assert_eq!(*ratio, 0.375);
        assert!(matches!(second.as_ref(), Layout::Split { ratio: 0.7, .. }));
    }

    #[test]
    fn reopening_after_the_sibling_moved_creates_a_separate_tab() {
        let mut tabs = Tabs::default();
        let ssh = tabs.insert("ssh");
        let closed = tabs.split(ssh, "local", Axis::Rows).unwrap();
        let location = tabs.closed_pane_location(closed).unwrap();
        tabs.remove(closed);
        let other = tabs.insert("other");
        assert!(tabs.move_to_existing_tab(ssh, other, Direction::Right));
        let existing = tabs.active_layout().unwrap().clone();

        let reopened = tabs.reopen_pane(&location, "fresh local");
        assert_eq!(tabs.active_layout(), Some(&Layout::Leaf(reopened)));
        assert_eq!(tabs.layout_for(other), Some(&existing));
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
