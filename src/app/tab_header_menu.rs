use super::*;
use bulk_tab_close::{Offer, Scope};

const RENAME: &str = "Rename tab…";
const PIN: &str = "Pin tab";
const UNPIN: &str = "Unpin tab";
const CLOSE: &str = "Close tab";

#[derive(Default)]
pub(super) struct Bindings {
    strip: Vec<(TabId, bool)>,
    by_tab: BTreeMap<TabId, Binding>,
}

impl Bindings {
    pub(super) fn refresh(
        &mut self,
        strip: Vec<(TabId, bool)>,
        context: &ViewContext<TerminalApp>,
    ) {
        if self.strip == strip {
            return;
        }
        self.by_tab
            .retain(|&id, _| strip.iter().any(|&(group, _)| id == group));
        for &(group, _) in &strip {
            let offers = Scope::ALL.map(|scope| Offer::from_header(&strip, group, scope));
            if self
                .by_tab
                .get(&group)
                .is_none_or(|binding| *binding.offers != offers)
            {
                self.by_tab
                    .insert(group, Binding::new(group, offers, context));
            }
        }
        self.strip = strip;
    }
}

struct Binding {
    // WinUI can deliver a click after the strip has changed.
    // Keep the targets that went with the displayed menu.
    offers: Rc<[Option<Offer>; 2]>,
    callback: Callback<String>,
}

impl Binding {
    fn new(group: TabId, offers: [Option<Offer>; 2], context: &ViewContext<TerminalApp>) -> Self {
        let offers = Rc::new(offers);
        let actions = Rc::clone(&offers);
        let callback = context.callback(move |label: String| {
            Scope::ALL
                .into_iter()
                .zip(actions.iter())
                .find(|(scope, _)| scope.label() == label)
                .map_or_else(
                    || Message::TabHeaderMenu(group, label),
                    |(_, offer)| {
                        offer
                            .clone()
                            .map_or(Message::ControlIdle, Message::CloseTabs)
                    },
                )
        });
        Self { offers, callback }
    }
}

impl TerminalApp {
    pub(super) fn tab_header_menu(&self, group: TabId, page: &Page) -> Menu {
        let pinned = self.tabs.group_is_pinned(group);
        let mut items = Vec::new();
        if matches!(page, Page::Terminal(_)) {
            items.extend([
                MenuItem::item("rename", RENAME),
                self.duplicate_tab_item_for(Some(group)),
                tab_colors::menu(true, self.tabs.group_color(group)),
                MenuItem::item("pin", if pinned { UNPIN } else { PIN }),
                MenuItem::separator("close-separator"),
            ]);
        }
        items.push(command_menu_item("close", CLOSE, !pinned));
        let bindings = self.tab_header_bindings.borrow();
        let binding = &bindings.by_tab[&group];
        items.extend(
            Scope::ALL
                .into_iter()
                .zip(binding.offers.iter())
                .map(|(scope, offer)| {
                    command_menu_item(scope.label(), scope.label(), offer.is_some())
                }),
        );
        Menu::new(items, binding.callback.clone())
    }

    pub(super) fn tab_header_action(
        &mut self,
        group: TabId,
        label: &str,
        context: &ComponentContext<Self>,
    ) {
        let Some(entry) = tab_title::switcher_leaf(&self.tabs.headers(), group) else {
            return;
        };
        if label == CLOSE {
            self.close_user_tab(entry, context);
            return;
        }
        if !matches!(
            self.tabs.get(entry).map(|tab| &tab.page),
            Some(Page::Terminal(_))
        ) {
            return;
        }
        match label.split("    ").next().unwrap_or(label) {
            RENAME => self.edit_tab_title(tab_title::Action::ShowGroup(group), context),
            PIN => self.set_tab_pinned(group, true, context),
            UNPIN => self.set_tab_pinned(group, false, context),
            label if label == KeyAction::DuplicateTabLayout.binding().label => {
                self.duplicate_tab_from_menu(group, context);
            }
            label => {
                if let Some(color) = tab_colors::from_menu(label)
                    && self.tabs.set_group_color(group, color)
                {
                    self.refresh_open_commands();
                }
            }
        }
    }

    pub(super) fn set_tab_pinned(
        &mut self,
        group: TabId,
        pinned: bool,
        context: &ComponentContext<Self>,
    ) {
        if self.tabs.set_group_pinned(group, pinned) {
            self.sync_selection(context);
            self.refresh_open_commands();
        }
    }

    pub(super) fn duplicate_tab_from_menu(
        &mut self,
        group: TabId,
        context: &ComponentContext<Self>,
    ) {
        self.window_error = self
            .duplicate_tab_layout(group, context)
            .err()
            .map(|error| format!("Couldn’t duplicate tab layout. {error:#}"));
    }
}
