//! Stable tab identities survive selection changes, closing, and drag reordering.
pub type TabId = u64;

pub struct Tab<T> {
    pub id: TabId,
    pub value: T,
}

pub struct Tabs<T> {
    entries: Vec<Tab<T>>,
    active: Option<TabId>,
    next_id: TabId,
}

impl<T> Default for Tabs<T> {
    fn default() -> Self {
        Self {
            entries: vec![],
            active: None,
            next_id: 1,
        }
    }
}

impl<T> Tabs<T> {
    pub fn entries(&self) -> &[Tab<T>] {
        &self.entries
    }
    pub fn active_id(&self) -> Option<TabId> {
        self.active
    }
    pub fn active_index(&self) -> Option<usize> {
        self.entries.iter().position(|t| Some(t.id) == self.active)
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
        self.get(self.active?)
    }
    pub fn insert(&mut self, value: T) -> TabId {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(Tab { id, value });
        self.active = Some(id);
        id
    }
    pub fn select(&mut self, id: TabId) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        self.active = Some(id);
        true
    }
    pub fn cycle(&mut self, offset: isize) {
        if let Some(index) = self.active_index() {
            let next = (index as isize + offset).rem_euclid(self.entries.len() as isize) as usize;
            self.active = Some(self.entries[next].id);
        }
    }
    pub fn remove(&mut self, id: TabId) -> Option<T> {
        let index = self.entries.iter().position(|t| t.id == id)?;
        let removed = self.entries.remove(index);
        if self.active == Some(id) {
            self.active = self
                .entries
                .get(index.min(self.entries.len().saturating_sub(1)))
                .map(|t| t.id);
        }
        Some(removed.value)
    }
    pub fn reorder(&mut self, order: &[TabId]) -> bool {
        let mut unique = std::collections::HashSet::new();
        if order.len() != self.entries.len()
            || !order
                .iter()
                .all(|id| unique.insert(*id) && self.get(*id).is_some())
        {
            return false;
        }
        let mut entries: std::collections::HashMap<_, _> =
            self.entries.drain(..).map(|t| (t.id, t)).collect();
        self.entries = order.iter().map(|id| entries.remove(id).unwrap()).collect();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
