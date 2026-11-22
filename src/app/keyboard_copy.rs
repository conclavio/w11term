use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use w11term::copy_mode::{CopyKey, CopyMode};

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
const MAX_QUEUED_KEYS: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    Word(u64),
    AwaitingFrame(u64, Point),
    Clipboard(u64),
    Stopped,
}

pub(super) struct ActiveCopy {
    pub pane: TabId,
    pub state: CopyMode,
    pending: Option<Pending>,
    queued: VecDeque<CopyKey>,
    error: Option<String>,
}

impl ActiveCopy {
    pub fn instructions(&self) -> &str {
        self.error
            .as_deref()
            .unwrap_or_else(|| self.state.instructions())
    }

    fn enqueue(&mut self, key: CopyKey) -> bool {
        if matches!(self.pending, Some(Pending::Clipboard(_) | Pending::Stopped)) {
            return false;
        }
        if self.queued.len() == MAX_QUEUED_KEYS {
            // An incomplete selection must never be copied after dropping a key.
            self.queued.clear();
            self.pending = Some(Pending::Stopped);
            self.error = Some("Copy mode · Too many queued keys · Esc to start again".into());
            return false;
        }
        self.queued.push_back(key);
        self.pending.is_none()
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
            queued: VecDeque::new(),
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

    pub(super) fn refresh_copy_mode(&mut self, id: TabId, ctx: &ComponentContext<Self>) {
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
            if let Some(Pending::AwaitingFrame(request, point)) = mode.pending {
                drop(active);
                self.finish_copy_word(id, request, Some(point), ctx);
                return;
            }
            let top = mode.state.top.max(frame.oldest_stable_row);
            if top != frame.first_stable_row {
                let _ = tab.session.input.send(Command::ScrollTo(top));
            } else if mode.pending.is_none() && !mode.queued.is_empty() {
                drop(active);
                self.drain_copy_keys(ctx);
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
        let ready = self
            .copy_mode
            .borrow_mut()
            .as_mut()
            .is_some_and(|m| m.enqueue(key));
        if ready {
            self.drain_copy_keys(ctx);
        }
    }

    fn drain_copy_keys(&mut self, ctx: &ComponentContext<Self>) {
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
        mode.error = None;
        let previous = mode.state.top;
        let pane = mode.pane;
        while let Some(key) = mode.queued.pop_front() {
            match key {
                CopyKey::Word(motion) => {
                    let request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
                    mode.pending = Some(Pending::Word(request));
                    let (reply, result) = tokio::sync::oneshot::channel();
                    let _ = tab.session.input.send(Command::CopyWord {
                        screen: mode.state.screen,
                        cursor: mode.state.cursor,
                        motion,
                        delimiters: self
                            .appearance
                            .read()
                            .unwrap()
                            .interaction
                            .word_delimiters
                            .clone(),
                        reply,
                    });
                    ctx.spawn_background_with_rejection(
                        move |_| {
                            Message::CopyWordResult(
                                pane,
                                request,
                                result.blocking_recv().ok().flatten(),
                            )
                        },
                        Message::CopyWordResult(pane, request, None),
                    );
                    break;
                }
                CopyKey::Copy => {
                    let Some(selection) = mode.state.selection() else {
                        continue;
                    };
                    let request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
                    mode.pending = Some(Pending::Clipboard(request));
                    mode.queued.clear();
                    let (reply, rx) = tokio::sync::oneshot::channel();
                    let _ = tab.session.input.send(Command::CopyRange {
                        screen: mode.state.screen,
                        selection,
                        trim_block: self
                            .appearance
                            .read()
                            .unwrap()
                            .interaction
                            .trim_block_selection,
                        reply,
                    });
                    ctx.spawn_background_with_rejection(
                        move |_| {
                            let result = rx
                                .blocking_recv()
                                .unwrap_or(Err("Pane closed before copying"))
                                .map_err(str::to_owned)
                                .and_then(|s| {
                                    w11term::clipboard::write(&s).map_err(|e| e.to_string())
                                });
                            Message::CopyResult(pane, request, result)
                        },
                        Message::CopyResult(
                            pane,
                            request,
                            Err("Unable to receive the copied text".into()),
                        ),
                    );
                    break;
                }
                // Horizontal movement needs the actual row to skip wide graphemes.
                CopyKey::Left | CopyKey::Right
                    if !(frame.first_stable_row
                        ..frame.first_stable_row + frame.rows.len() as isize)
                        .contains(&mode.state.cursor.row) =>
                {
                    mode.queued.push_front(key);
                    break;
                }
                _ => mode.state.navigate(key, &frame),
            }
        }
        if mode.state.top != previous {
            let _ = tab.session.input.send(Command::ScrollTo(mode.state.top));
        }
        tab.surface.invalidator.invalidate();
    }

    pub(super) fn finish_copy_word(
        &mut self,
        pane: TabId,
        request: u64,
        point: Option<Point>,
        ctx: &ComponentContext<Self>,
    ) {
        let mut active = self.copy_mode.borrow_mut();
        let Some(mode) = active.as_mut().filter(|m| {
            m.pane == pane
                && matches!(m.pending,
                Some(Pending::Word(id) | Pending::AwaitingFrame(id, _)) if id == request)
        }) else {
            return;
        };
        let Some(AppTab {
            page: Page::Terminal(tab),
            ..
        }) = self.tabs.get(pane)
        else {
            return;
        };
        let frame = tab.session.frames.load();
        let Some(point) = point.filter(|p| {
            frame.prompt.is_none() && mode.state.valid(&frame) && p.row >= frame.oldest_stable_row
        }) else {
            mode.queued.clear();
            mode.pending = Some(Pending::Stopped);
            mode.error =
                Some("Copy mode · Could not locate the word in this screen · Esc to exit".into());
            return;
        };
        // The worker can have read output that WinUI has yet to paint. Wait for
        // its frame rather than clamping this destination to an unrelated row.
        if point.row
            >= frame.first_stable_row + frame.scroll_offset as isize + frame.rows.len() as isize
        {
            mode.pending = Some(Pending::AwaitingFrame(request, point));
            return;
        }
        mode.pending = None;
        let previous = mode.state.top;
        mode.state.move_to(point, &frame);
        if mode.state.top != previous {
            let _ = tab.session.input.send(Command::ScrollTo(mode.state.top));
        }
        tab.surface.invalidator.invalidate();
        if !mode.queued.is_empty() {
            drop(active);
            self.drain_copy_keys(ctx);
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
            .filter(|m| m.pane == pane && m.pending == Some(Pending::Clipboard(request)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use w11term::terminal::Snapshot;

    #[test]
    fn overflowing_pending_word_keys_cannot_copy_a_partial_selection() {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let terminal = w11term::terminal::create(TerminalSize::default(), tx);
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        let mut mode = ActiveCopy {
            pane: 1,
            state: CopyMode::new(&frame),
            pending: Some(Pending::Word(17)),
            queued: VecDeque::new(),
            error: None,
        };
        for _ in 0..MAX_QUEUED_KEYS {
            assert!(!mode.enqueue(CopyKey::Right));
        }
        assert!(!mode.enqueue(CopyKey::Copy));
        assert!(matches!(mode.pending, Some(Pending::Stopped)));
        assert!(mode.queued.is_empty());
        assert!(!mode.enqueue(CopyKey::Copy));
        assert!(mode.instructions().contains("Esc"));
    }
}
