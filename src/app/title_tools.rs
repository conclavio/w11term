use super::*;

const BUTTON_WIDTH: f64 = 36.0;
pub(super) const INBOX_WIDTH: f64 = 50.0;
const GAP: f64 = 2.0;
const CAPTION_RESERVE: f64 = 180.0;
const MIN_TAB_WIDTH: f64 = 144.0;
const DRAG_RESERVE: f64 = 28.0;
const TOOLS_WIDTH: f64 = 4.0 * (BUTTON_WIDTH + GAP) + INBOX_WIDTH;
const TAB_ACTIONS_WIDTH: f64 = 2.0 * BUTTON_WIDTH;

pub(super) struct Layout {
    pub(super) content_width: f64,
    pub(super) tab_width: f64,
    pub(super) show_tools: bool,
}

impl Layout {
    pub(super) fn new(window_width: f64, tabs: usize) -> Self {
        let width = (window_width - CAPTION_RESERVE).max(TAB_ACTIONS_WIDTH);
        let show_tools = width >= MIN_TAB_WIDTH + TAB_ACTIONS_WIDTH + TOOLS_WIDTH + DRAG_RESERVE;
        let content_width = width - if show_tools { TOOLS_WIDTH } else { 0.0 };
        let tabs_available = (content_width - TAB_ACTIONS_WIDTH - DRAG_RESERVE).max(0.0);
        Self {
            content_width,
            tab_width: (tabs as f64 * 110.0 + 16.0).min(tabs_available),
            show_tools,
        }
    }
}

pub(super) fn button(name: &str) -> Button {
    Button::new()
        .style(ButtonStyle::Subtle)
        .width(BUTTON_WIDTH)
        .height(36.0)
        .automation_name(name)
}

pub(super) fn below(text: impl Into<String>) -> Tooltip {
    Tooltip::text(text).placement(TooltipPlacement::Bottom)
}

impl TerminalApp {
    pub(super) fn title_tools(
        &self,
        layout: &Layout,
        menu_action: Callback<String>,
        context: &ViewContext<Self>,
    ) -> View {
        if !layout.show_tools {
            return View::empty();
        }
        let live = self.tabs.active().and_then(|tab| match &tab.page {
            Page::Terminal(live) => Some(live),
            _ => None,
        });
        let ssh = live.filter(|live| matches!(live.source, Source::Ssh(_)));
        let pane = ssh.map(|live| live.session.pane_id);
        let files = ToggleButton::new()
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
            .menu(Menu::new(self.split_items(), menu_action));
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

        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .horizontal_alignment(HorizontalAlignment::Right)
            .spacing(GAP)
            .keyed_children([
                KeyedView::new("files", files),
                KeyedView::new("panes", panes),
                KeyedView::new("find", find),
                KeyedView::new("palette", palette),
                KeyedView::new("inbox", self.inbox_button(context)),
            ])
    }
}
