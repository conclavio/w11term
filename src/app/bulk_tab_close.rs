use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Scope {
    Other,
    Right,
}

impl Scope {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Other => "Close other tabs",
            Self::Right => "Close tabs to the right",
        }
    }

    pub(super) fn from_label(label: &str) -> Option<Self> {
        [Self::Other, Self::Right]
            .into_iter()
            .find(|scope| scope.label() == label)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Offer {
    scope: Scope,
    anchor: TabId,
    targets: Vec<TabId>,
}

impl Offer {
    fn capture<T>(tabs: &Tabs<T>, scope: Scope) -> Option<Self> {
        let anchor = tabs.active_group_id()?;
        let start = match scope {
            Scope::Other => 0,
            Scope::Right => tabs.active_index()? + 1,
        };
        let targets: Vec<_> = tabs
            .headers()
            .iter()
            .skip(start)
            .map(|tab| tab.group_id())
            .filter(|&group| group != anchor && !tabs.group_is_pinned(group))
            .collect();
        (!targets.is_empty()).then_some(Self {
            scope,
            anchor,
            targets,
        })
    }

    fn still_matches<T>(&self, tabs: &Tabs<T>) -> bool {
        Self::capture(tabs, self.scope).as_ref() == Some(self)
    }
}

impl TerminalApp {
    pub(super) fn bulk_close_offer(&self, scope: Scope) -> Option<Offer> {
        Offer::capture(&self.tabs, scope)
    }

    pub(super) fn close_tabs_from_offer(&mut self, offer: Offer, context: &ComponentContext<Self>) {
        // An open menu may outlive its selection or a change to the tab strip.
        if !offer.still_matches(&self.tabs) {
            return;
        }
        let entries: Vec<_> = self
            .tabs
            .headers()
            .iter()
            .filter(|tab| offer.targets.contains(&tab.group_id()))
            .map(|tab| (tab.group_id(), tab.id))
            .collect();
        for &(group, _) in &entries {
            self.remember_closed_tab(group);
        }
        self.close_tabs(entries.into_iter().map(|(_, id)| id), context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_follows_strip_order_and_skips_pins_and_the_selected_tab() {
        let mut tabs = Tabs::default();
        let earlier = tabs.insert("earlier");
        let anchor = tabs.insert("anchor");
        let later = tabs.insert("later");
        let pinned = tabs.insert("pinned");
        tabs.set_group_pinned(pinned, true);
        tabs.select(anchor);
        assert_eq!(
            Offer::capture(&tabs, Scope::Other).unwrap().targets,
            [earlier, later]
        );
        assert_eq!(
            Offer::capture(&tabs, Scope::Right).unwrap().targets,
            [later]
        );
        tabs.select(later);
        assert!(Offer::capture(&tabs, Scope::Right).is_none());
    }

    #[test]
    fn a_stale_offer_never_closes_new_tabs_or_tabs_that_have_been_pinned() {
        let mut tabs = Tabs::default();
        let anchor = tabs.insert("anchor");
        let later = tabs.insert("later");
        tabs.select(anchor);
        let offer = Offer::capture(&tabs, Scope::Other).unwrap();
        assert!(offer.still_matches(&tabs));
        let new = tabs.insert("new");
        assert!(!offer.still_matches(&tabs));
        tabs.select(anchor);
        assert!(!offer.still_matches(&tabs));
        tabs.set_group_pinned(new, true);
        assert!(offer.still_matches(&tabs));
        tabs.set_group_pinned(later, true);
        assert!(!offer.still_matches(&tabs));
    }
}
