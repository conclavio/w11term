use super::*;
use w11term::copy_mode::{CopyKey, CopyMode};

pub(super) struct ActiveCopy {
    pub pane: TabId,
    pub state: CopyMode,
    pending: Option<u64>,
    error: Option<String>,
}

impl ActiveCopy {
    pub fn instructions(&self) -> &str {
        self.error
            .as_deref()
            .unwrap_or_else(|| self.state.instructions())
    }
}

impl TerminalApp {
    pub(super) fn open_copy_mode(&mut self) {
        self.close_hints();
        let Some((id, tab)) =
            self.tabs
                .active_id()
                .zip(self.tabs.active())
                .and_then(|(id, tab)| match &tab.page {
                    Page::Terminal(tab) => Some((id, tab)),
                    _ => None,
                })
        else {
            self.keyboard.set_copy_mode(false);
            return;
        };
        let frame = tab.session.frames.load();
        if frame.prompt.is_some()
            || tab.find.open
            || tab.browsing_sessions()
            || tab.device_busy
            || self.picker.is_some()
            || self.switcher.is_some()
        {
            self.keyboard.set_copy_mode(false);
            return;
        }
        if self.copy_mode.borrow().is_some() {
            return;
        }
        tab.selection.borrow_mut().take();
        self.keyboard.set_selection(false);
        *self.copy_mode.borrow_mut() = Some(ActiveCopy {
            pane: id,
            state: CopyMode::new(&frame),
            pending: None,
            error: None,
        });
        self.keyboard.set_copy_mode(true);
        tab.surface.invalidator.invalidate();
    }

    pub(super) fn close_copy_mode(&mut self) {
        let Some(mode) = self.copy_mode.borrow_mut().take() else {
            return;
        };
        if let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get(mode.pane)
        {
            if mode.state.screen == (&*tab.session.frames.load()).into() {
                let restore = mode
                    .state
                    .original_top
                    .map_or(Command::ScrollEdge(false), Command::ScrollTo);
                let _ = tab.session.input.send(restore);
            }
            tab.surface.invalidator.invalidate();
        }
        self.keyboard.set_copy_mode(false);
    }

    pub(super) fn refresh_copy_mode(&mut self, id: TabId) {
        let active = self.copy_mode.borrow();
        let Some(mode) = active.as_ref().filter(|m| m.pane == id) else {
            return;
        };
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get(id)
        else {
            return;
        };
        let frame = tab.session.frames.load();
        if frame.prompt.is_none() && mode.state.valid(&frame) {
            let top = mode.state.top.max(frame.oldest_stable_row);
            if top != frame.first_stable_row {
                let _ = tab.session.input.send(Command::ScrollTo(top));
            }
            return;
        }
        drop(active);
        self.close_copy_mode();
    }

    pub(super) fn copy_key(&mut self, key: CopyKey, ctx: &ComponentContext<Self>) {
        if key == CopyKey::Cancel {
            self.close_copy_mode();
            return;
        }
        let mut active = self.copy_mode.borrow_mut();
        let Some(mode) = active.as_mut() else { return };
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get(mode.pane)
        else {
            return;
        };
        let frame = tab.session.frames.load();
        if frame.prompt.is_some() || !mode.state.valid(&frame) {
            drop(active);
            self.close_copy_mode();
            return;
        }
        if mode.pending.is_some() {
            return;
        }
        mode.error = None;
        if key == CopyKey::Copy {
            let Some(selection) = mode.state.selection() else {
                return;
            };
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = tab
                .session
                .input
                .send(Command::CopyRange(mode.state.screen, selection, tx));
            static REQUEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let request = REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            mode.pending = Some(request);
            let pane = mode.pane;
            ctx.spawn_background(move |_| {
                let result = rx
                    .blocking_recv()
                    .unwrap_or(Err("Pane closed before copying"))
                    .map_err(str::to_owned)
                    .and_then(|s| w11term::clipboard::write(&s).map_err(|e| e.to_string()));
                Message::CopyResult(pane, request, result)
            });
        } else {
            let previous = mode.state.top;
            mode.state.navigate(key, &frame);
            if mode.state.top != previous {
                let _ = tab.session.input.send(Command::ScrollTo(mode.state.top));
            }
            tab.surface.invalidator.invalidate();
        }
    }

    pub(super) fn finish_keyboard_copy(
        &mut self,
        pane: TabId,
        request: u64,
        result: Result<(), String>,
    ) {
        let mut active = self.copy_mode.borrow_mut();
        let Some(mode) = active
            .as_mut()
            .filter(|m| m.pane == pane && m.pending == Some(request))
        else {
            return;
        };
        mode.pending = None;
        if let Err(e) = result {
            mode.error = Some(format!("Copy mode · {e} · Esc cancel"));
        } else {
            drop(active);
            self.close_copy_mode();
        }
    }
}
