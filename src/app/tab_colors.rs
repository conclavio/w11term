use super::*;

const CURRENT: &str = " (current)";

fn choices() -> impl Iterator<Item = (Option<TabColor>, String)> {
    std::iter::once((None, "Use profile tab color".into())).chain(TabColor::ALL.into_iter().map(
        |color| {
            let label = match color {
                TabColor::None => "Hide tab color".into(),
                color => format!("Set tab color to {}", color.label()),
            };
            (Some(color), label)
        },
    ))
}

pub(super) fn menu(available: bool, current: Option<TabColor>) -> MenuItem {
    if !available {
        return MenuItem::disabled("tab-colors", "Tab color");
    }
    MenuItem::submenu(
        "tab-colors",
        "Tab color",
        choices().map(move |(color, label)| {
            let title = if color == current {
                format!("{label}{CURRENT}")
            } else {
                label.clone()
            };
            MenuItem::item(label, title)
        }),
    )
}

pub(super) fn from_menu(label: &str) -> Option<Option<TabColor>> {
    let label = label.strip_suffix(CURRENT).unwrap_or(label);
    choices().find_map(|(color, title)| (title == label).then_some(color))
}

impl TerminalApp {
    pub(super) fn effective_tab_color(&self, group: TabId, page: &Page) -> TabColor {
        self.tabs.group_color(group).unwrap_or_else(|| match page {
            Page::Terminal(live) => live.source.tab_color(),
            _ => TabColor::None,
        })
    }

    pub(super) fn set_tab_color(&mut self, group: TabId, color: Option<TabColor>) -> bool {
        let changed = self.tabs.set_group_color(group, color);
        if changed {
            self.refresh_open_commands();
        }
        changed
    }

    pub(super) fn control_tab_appearance(
        &mut self,
        tab_id: TabId,
        change: w11term::control::TabAppearanceChange,
        context: &ComponentContext<Self>,
    ) -> anyhow::Result<serde_json::Value> {
        use w11term::control::TabAppearanceChange;
        let window_id = self.registry.window_id();
        let entry_id = self
            .tabs
            .group_focus(tab_id)
            .with_context(|| format!("unknown tab {tab_id} in window {window_id}"))?;
        anyhow::ensure!(
            matches!(
                self.tabs.get(entry_id).map(|entry| &entry.page),
                Some(Page::Terminal(_))
            ),
            "Only terminal tabs can be pinned or colored"
        );
        let changed = match change {
            TabAppearanceChange::Pin(pinned) => self.set_tab_pinned(tab_id, pinned, context),
            TabAppearanceChange::Color(color) => self.set_tab_color(tab_id, color),
        };
        let page = &self
            .tabs
            .get(entry_id)
            .expect("the edited tab remains open")
            .page;
        Ok(serde_json::json!({
            "window_id": window_id, "tab_id": tab_id,
            "pinned": self.tabs.group_is_pinned(tab_id),
            "color_override": self.tabs.group_color(tab_id),
            "color": self.effective_tab_color(tab_id, page),
            "changed": changed,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_distinguishes_an_explicitly_hidden_color_from_the_profile_default() {
        assert_eq!(
            from_menu("Hide tab color (current)"),
            Some(Some(TabColor::None))
        );
        assert_eq!(from_menu("Use profile tab color (current)"), Some(None));
        assert_eq!(from_menu("Set tab color to Red"), Some(Some(TabColor::Red)));
        assert_eq!(from_menu("Rename tab"), None);
    }
}
