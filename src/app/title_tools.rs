use super::*;

const BUTTON_WIDTH: f64 = 36.0;
pub(super) const INBOX_WIDTH: f64 = 50.0;
const GAP: f64 = 2.0;
const CAPTION_RESERVE: f64 = 180.0;
const DRAG_RESERVE: f64 = 28.0;
const TAB_ACTIONS_WIDTH: f64 = 2.0 * BUTTON_WIDTH;
const FULL_TOOLS_WIDTH: f64 = 4.0 * (BUTTON_WIDTH + GAP) + INBOX_WIDTH;
const COMPACT_TOOLS_WIDTH: f64 = BUTTON_WIDTH + GAP + INBOX_WIDTH;
const TAB_CHROME_RESERVE: f64 = 390.0;
const MIN_TAB_STRIP_WIDTH: f64 = 140.0;

pub(super) const SHOW_FILES: &str = "Show SFTP files";
pub(super) const HIDE_FILES: &str = "Hide SFTP files";
pub(super) const FIND_SCROLLBACK: &str = "Find in scrollback";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tools {
    Full,
    Compact,
    Hidden,
}

pub(super) struct Layout {
    pub(super) content_width: f64,
    pub(super) tab_width: f64,
    tools: Tools,
}

impl Layout {
    pub(super) fn new(window_width: f64, tabs: usize) -> Self {
        let width = (window_width - CAPTION_RESERVE).max(TAB_ACTIONS_WIDTH);
        let tab_width = tabs as f64 * 110.0 + 16.0;
        // Keep the former tab-strip budget when the tabs start to crowd. WinUI
        // can leave a newly selected tab outside the realized header range.
        let tab_budget =
            tab_width.min((window_width - TAB_CHROME_RESERVE).max(MIN_TAB_STRIP_WIDTH));
        let room_for_tools = width - TAB_ACTIONS_WIDTH - DRAG_RESERVE - tab_budget;
        let (tools, tool_width) = if room_for_tools >= FULL_TOOLS_WIDTH {
            (Tools::Full, FULL_TOOLS_WIDTH)
        } else if room_for_tools >= COMPACT_TOOLS_WIDTH {
            (Tools::Compact, COMPACT_TOOLS_WIDTH)
        } else {
            (Tools::Hidden, 0.0)
        };
        let content_width = width - tool_width;
        let tabs_available = (content_width - TAB_ACTIONS_WIDTH - DRAG_RESERVE).max(0.0);
        Self {
            content_width,
            tab_width: tab_width.min(tabs_available),
            tools,
        }
    }
}

pub(super) fn button(name: &str) -> Button {
    Button::new()
        .style(ButtonStyle::Subtle)
        .padding(0.0)
        .horizontal_content_alignment(HorizontalAlignment::Center)
        .vertical_content_alignment(VerticalAlignment::Center)
        .width(BUTTON_WIDTH)
        .height(36.0)
        .automation_name(name)
}

pub(super) fn below(text: impl Into<String>) -> Tooltip {
    Tooltip::text(text).placement(TooltipPlacement::Bottom)
}

impl TerminalApp {
    pub(super) fn scrollback_find_item(&self) -> MenuItem {
        self.binding_menu_enabled(
            "find-scrollback",
            FIND_SCROLLBACK,
            KeyAction::Find,
            matches!(
                self.tabs.active().map(|tab| &tab.page),
                Some(Page::Terminal(live)) if live.find_available()
            ),
        )
    }

    fn pane_layout_items(&self, arrangement: &PaneArrangement) -> Vec<MenuItem> {
        let mut items = Vec::from(self.split_items());
        if self.active_layout_is_split() {
            items.push(MenuItem::separator("layout-adjustments"));
            items.push(self.pane_zoom_item());
            items.extend(self.split_arrangement_items(arrangement));
        }
        items.extend([MenuItem::separator("pane-name"), self.rename_pane_item()]);
        items
    }

    pub(super) fn title_tools(
        &self,
        layout: &Layout,
        arrangement: &PaneArrangement,
        menu_action: Callback<String>,
        context: &ViewContext<Self>,
    ) -> View {
        if layout.tools == Tools::Hidden {
            return View::empty();
        }
        let live = self.tabs.active().and_then(|tab| match &tab.page {
            Page::Terminal(live) => Some(live),
            _ => None,
        });
        let ssh = live.filter(|live| matches!(live.source, Source::Ssh(_)));
        let tools = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .horizontal_alignment(HorizontalAlignment::Right)
            .spacing(GAP);
        if layout.tools == Tools::Compact {
            let files_label = if ssh.is_some_and(|live| live.files.open) {
                HIDE_FILES
            } else {
                SHOW_FILES
            };
            let panes = if live.is_some() {
                MenuItem::submenu(
                    "pane-layout",
                    "Pane layout",
                    self.pane_layout_items(arrangement),
                )
            } else {
                MenuItem::disabled("pane-layout", "Pane layout")
            };
            let compact = button("Terminal tools")
                .automation_id("w11term-title-terminal-tools")
                .content(
                    FontIcon::new()
                        .glyph("\u{e700}")
                        .tooltip_with(below("Terminal tools")),
                )
                .menu(Menu::new(
                    [
                        command_menu_item("sftp-files", files_label, ssh.is_some()),
                        panes,
                        self.scrollback_find_item(),
                        self.binding_menu(
                            "command-palette",
                            "Command palette…",
                            KeyAction::CommandPalette,
                        ),
                    ],
                    menu_action,
                ));
            return tools.keyed_children([
                KeyedView::new("compact", compact),
                KeyedView::new("inbox", self.inbox_button(context)),
            ]);
        }
        let pane = ssh.map(|live| live.session.pane_id);
        let files = ToggleButton::new()
            .padding(0.0)
            .width(BUTTON_WIDTH)
            .height(36.0)
            .automation_name("Toggle SFTP file drawer")
            .automation_id("w11term-title-files")
            .is_enabled(pane.is_some())
            .is_checked(ssh.is_some_and(|live| live.files.open))
            .on_is_checked_changed(context.callback(move |checked| {
                pane.map_or(Message::ControlIdle, |id| {
                    Message::SetTitleFiles(id, checked)
                })
            }))
            .content(FontIcon::new().glyph("\u{e8b7}"))
            .tooltip_with(below("SFTP files"));

        let panes = button("Pane layout")
            .automation_id("w11term-title-layout")
            .is_enabled(live.is_some())
            .content(
                FontIcon::new()
                    .glyph("\u{e7c4}")
                    .tooltip_with(below("Pane layout")),
            )
            .menu(Menu::new(self.pane_layout_items(arrangement), menu_action));
        let find_binding = KeyAction::Find.binding();
        let palette_binding = KeyAction::CommandPalette.binding();
        let shortcut_hint = |binding| {
            self.appearance
                .read()
                .unwrap()
                .shortcuts
                .get(binding)
                .map_or_else(
                    || binding.label.to_owned(),
                    |key| format!("{} ({key})", binding.label),
                )
        };
        let find = button("Find in terminal scrollback")
            .automation_id("w11term-title-find")
            .is_enabled(live.is_some_and(LiveTab::find_available))
            .on_click(context.callback(|_| Message::Shortcut(Shortcut::Find)))
            .content(FontIcon::new().glyph("\u{e721}"))
            .tooltip_with(below(shortcut_hint(find_binding)));
        let palette = button("Command palette")
            .automation_id("w11term-title-palette")
            .on_click(context.callback(|_| Message::PickerFromMenu(Shortcut::CommandPalette)))
            .content(FontIcon::new().glyph("\u{e756}"))
            .tooltip_with(below(shortcut_hint(palette_binding)));

        tools.keyed_children([
            KeyedView::new("files", files),
            KeyedView::new("panes", panes),
            KeyedView::new("find", find),
            KeyedView::new("palette", palette),
            KeyedView::new("inbox", self.inbox_button(context)),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_yields_space_to_a_crowded_tab_strip() {
        let single = Layout::new(1120.0, 1);
        assert_eq!(single.tools, Tools::Full);
        assert_eq!(single.tab_width, 126.0);

        let crowded = Layout::new(1120.0, 8);
        assert_eq!(crowded.tools, Tools::Compact);
        assert!(crowded.tab_width >= 730.0);

        let narrow = Layout::new(440.0, 8);
        assert_eq!(narrow.tools, Tools::Hidden);
        assert!(narrow.tab_width >= MIN_TAB_STRIP_WIDTH);
    }
}
