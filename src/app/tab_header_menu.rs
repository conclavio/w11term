use super::*;

const RENAME: &str = "Rename tab…";
const PIN: &str = "Pin tab";
const UNPIN: &str = "Unpin tab";
const CLOSE: &str = "Close tab";

impl TerminalApp {
    pub(super) fn tab_header_menu(
        &self,
        group: TabId,
        page: &Page,
        context: &ViewContext<Self>,
    ) -> Menu {
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
        let callback = self
            .tab_header_callbacks
            .borrow_mut()
            .entry(group)
            .or_insert_with(|| context.callback(move |label| Message::TabHeaderMenu(group, label)))
            .clone();
        Menu::new(items, callback)
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
