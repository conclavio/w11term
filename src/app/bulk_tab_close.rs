use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Scope {
    Other,
    Right,
}

impl Scope {
    pub(super) const ALL: [Self; 2] = [Self::Other, Self::Right];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Other => "Close other tabs",
            Self::Right => "Close tabs to the right",
        }
    }

    pub(super) fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scope| scope.label() == label)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Origin {
    ActiveTab,
    TabHeader,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Offer {
    scope: Scope,
    anchor: TabId,
    origin: Origin,
    targets: Vec<TabId>,
}

pub(super) fn strip_order<T>(
    tabs: &Tabs<T>,
    headers: &[&w11term::tabs::Tab<T>],
) -> Vec<(TabId, bool)> {
    headers
        .iter()
        .map(|tab| {
            let group = tab.group_id();
            (group, tabs.group_is_pinned(group))
        })
        .collect()
}

impl Offer {
    fn capture<T>(tabs: &Tabs<T>, scope: Scope) -> Option<Self> {
        Self::capture_from(
            &strip_order(tabs, &tabs.headers()),
            tabs.active_group_id()?,
            scope,
            Origin::ActiveTab,
        )
    }

    pub(super) fn from_header(
        strip: &[(TabId, bool)],
        anchor: TabId,
        scope: Scope,
    ) -> Option<Self> {
        Self::capture_from(strip, anchor, scope, Origin::TabHeader)
    }

    fn capture_from(
        strip: &[(TabId, bool)],
        anchor: TabId,
        scope: Scope,
        origin: Origin,
    ) -> Option<Self> {
        let anchor_index = strip.iter().position(|&(group, _)| group == anchor)?;
        let start = match scope {
            Scope::Other => 0,
            Scope::Right => anchor_index + 1,
        };
        let targets: Vec<_> = strip[start..]
            .iter()
            .filter_map(|&(group, pinned)| (group != anchor && !pinned).then_some(group))
            .collect();
        (!targets.is_empty()).then_some(Self {
            scope,
            anchor,
            origin,
            targets,
        })
    }

    fn still_matches<T>(&self, tabs: &Tabs<T>) -> bool {
        if self.origin == Origin::ActiveTab && tabs.active_group_id() != Some(self.anchor) {
            return false;
        }
        Self::capture_from(
            &strip_order(tabs, &tabs.headers()),
            self.anchor,
            self.scope,
            self.origin,
        )
        .as_ref()
            == Some(self)
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
        let headers = self.tabs.headers();
        let entries: Vec<_> = headers
            .iter()
            .filter(|tab| offer.targets.contains(&tab.group_id()))
            .map(|tab| (tab.group_id(), tab.id))
            .collect();
        let anchor = tab_title::switcher_leaf(&headers, offer.anchor);
        for &(group, _) in &entries {
            self.remember_closed_tab(group);
        }
        // The clicked tab takes focus when the current selection is closing.
        if self
            .tabs
            .active_group_id()
            .is_some_and(|group| offer.targets.contains(&group))
            && let Some(anchor) = anchor
        {
            self.tabs.select(anchor);
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

    #[test]
    fn a_header_offer_uses_the_named_tab_and_rechecks_the_original_targets() {
        let mut tabs = Tabs::default();
        let left = tabs.insert("left");
        let anchor = tabs.insert("anchor");
        let right = tabs.insert("right");
        let pinned = tabs.insert("pinned");
        tabs.set_group_pinned(pinned, true);
        tabs.select(left);
        let strip = strip_order(&tabs, &tabs.headers());
        let to_the_right = Offer::from_header(&strip, anchor, Scope::Right).unwrap();
        assert_eq!(to_the_right.targets, [right]);
        assert_eq!(
            Offer::from_header(&strip, anchor, Scope::Other)
                .unwrap()
                .targets,
            [left, right]
        );
        assert!(Offer::from_header(&strip, right, Scope::Right).is_none());
        tabs.select(right);
        assert!(to_the_right.still_matches(&tabs));
        tabs.insert("new");
        assert!(!to_the_right.still_matches(&tabs));
        tabs.remove_group(anchor);
        assert!(!to_the_right.still_matches(&tabs));
    }
}
