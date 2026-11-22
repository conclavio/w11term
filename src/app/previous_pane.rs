use super::*;

/// A native menu or palette row may be invoked after focus changes or a terminal
/// is replaced. Bind both the originating page and the original session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Target {
    from: TabId,
    source_session: Option<u64>,
    to: TabId,
    session: u64,
}

pub(super) fn message(target: Option<Target>) -> Message {
    target.map_or(Message::ControlIdle, Message::FocusPreviousPane)
}

pub(super) fn target(tabs: &Tabs<AppTab>) -> Option<Target> {
    let to = tabs.previous_pane(AppTab::visited_terminal)?;
    let from = tabs.active_id()?;
    Some(Target {
        from,
        source_session: tabs.get(from)?.pane_id(),
        to,
        session: tabs.get(to)?.pane_id()?,
    })
}

impl TerminalApp {
    pub(super) fn focus_previous_pane(&mut self, target: Target, ctx: &ComponentContext<Self>) {
        let changing_tab = self.tabs.active_group_id() != self.tabs.group_id(target.to);
        if self.tabs.select(target.to) {
            if changing_tab {
                self.sync_selection(ctx);
            }
            self.activate(ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_menu_and_palette_keep_the_original_page_and_terminal_session() {
        let mut tabs = Tabs::default();
        let first = tabs.insert(live_tab("first".into(), Source::Demo, false));
        tabs.record_pane_focus(AppTab::visited_terminal);
        let second = tabs.insert(live_tab("second".into(), Source::Demo, false));
        tabs.record_pane_focus(AppTab::visited_terminal);
        let offered = target(&tabs).unwrap();
        assert_eq!((offered.from, offered.to), (second, first));

        let settings = tabs.insert(AppTab {
            label: "Settings".into(),
            page: Page::Settings(None),
        });
        tabs.record_pane_focus(AppTab::visited_terminal);
        let from_settings = target(&tabs).unwrap();
        assert_eq!((from_settings.from, from_settings.to), (settings, second));
        assert_ne!(from_settings, offered);

        tabs.select(second);
        assert_eq!(target(&tabs), Some(offered));
        *tabs.get_mut(first).unwrap() = live_tab("replacement".into(), Source::Demo, false);
        assert_eq!(target(&tabs), None, "a replacement is unvisited");
        tabs.select(first);
        tabs.record_pane_focus(AppTab::visited_terminal);
        tabs.select(second);
        tabs.record_pane_focus(AppTab::visited_terminal);
        let replacement = target(&tabs).unwrap();
        assert_eq!((replacement.from, replacement.to), (second, first));
        assert_ne!(
            command_palette::CommandId::FocusPreviousPane(Some(offered)),
            command_palette::CommandId::FocusPreviousPane(Some(replacement))
        );
        *tabs.get_mut(second).unwrap() = live_tab("new source".into(), Source::Demo, false);
        let new_source = target(&tabs).unwrap();
        assert_eq!((new_source.from, new_source.to), (second, first));
        assert_ne!(new_source, replacement);
    }
}
