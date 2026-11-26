use super::*;

pub(super) const INCREASE: &str = "Increase pane text size";
pub(super) const DECREASE: &str = "Decrease pane text size";
pub(super) const RESET: &str = "Reset pane text size";

#[derive(Clone, Copy)]
pub enum Change {
    Increase,
    Decrease,
    Reset,
}

impl Change {
    fn next(self, base: f32, current: Option<f32>) -> Option<f32> {
        let size = current.unwrap_or(base);
        match self {
            Self::Increase if size < 72.0 => Some((size + 1.0).min(72.0)),
            Self::Decrease if size > 6.0 => Some((size - 1.0).max(6.0)),
            Self::Reset => None,
            _ => current,
        }
    }
}

pub(super) fn capture(
    appearance: Arc<RwLock<Appearance>>,
    font_size: Option<f32>,
) -> w11term::control::CapturePng {
    Arc::new(move |frame, size, path| {
        let mut appearance = appearance.read().unwrap().clone();
        if let Some(font_size) = font_size {
            appearance.font_size = font_size;
        }
        crate::renderer::screenshot_with_font(frame, size, path, &appearance)
    })
}

impl LiveTab {
    pub(super) fn remeasure_font(&self, renderer: &mut Renderer) {
        self.selection.borrow_mut().take();
        self.hover.borrow_mut().take();
        if let Some(size) = self.surface.remeasure(renderer) {
            self.last_size.set(Some(size));
            let _ = self.session.input.send(Command::Resize(size));
        }
        self.surface.release();
        self.surface.invalidator.invalidate();
    }
}

impl TerminalApp {
    pub(super) fn pane_text_size_menu(&self) -> MenuItem {
        let Some(Page::Terminal(tab)) = self.tabs.active().map(|tab| &tab.page) else {
            return MenuItem::disabled("pane-text-size", "Pane text size");
        };
        let base = self.appearance.read().unwrap().font_size;
        let current = tab.surface.font_size.get();
        MenuItem::submenu(
            "pane-text-size",
            "Pane text size",
            [
                ("increase-pane-text", INCREASE, Change::Increase),
                ("decrease-pane-text", DECREASE, Change::Decrease),
                ("reset-pane-text", RESET, Change::Reset),
            ]
            .map(|(key, label, change)| {
                command_menu_item(key, label, change.next(base, current) != current)
            }),
        )
    }

    pub(super) fn change_pane_text_size(&mut self, change: Change) {
        let Some(Page::Terminal(tab)) = self.tabs.active().map(|tab| &tab.page) else {
            return;
        };
        let appearance = self.appearance.read().unwrap();
        let current = tab.surface.font_size.get();
        let next = change.next(appearance.font_size, current);
        if next == current {
            return;
        }
        let mut renderer = match Renderer::with_size(
            &appearance.font_family,
            appearance.ligatures,
            appearance.terminal_font_size_for(next),
        ) {
            Ok(renderer) => renderer,
            Err(error) => {
                self.window_error = Some(format!("Could not change pane text size. {error}"));
                return;
            }
        };
        drop(appearance);
        tab.surface.font_size.set(next);
        tab.remeasure_font(&mut renderer);
        self.keyboard.set_selection(false);
        self.publish_panes();
    }
}
