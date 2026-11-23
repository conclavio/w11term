use super::*;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use w11term::copy_mode::{CopyKey, CopyMode};

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);
const MAX_QUEUED_KEYS: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    SelectAll(u64),
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
    find_origin: Option<clipboard_paste::Origin>,
    error: Option<String>,
}

impl ActiveCopy {
    fn begin_clipboard(&mut self, request: u64) {
        self.pending = Some(Pending::Clipboard(request));
        self.queued.clear();
    }

    pub fn instructions(&self) -> &str {
        self.error
            .as_deref()
            .unwrap_or_else(|| self.state.instructions())
    }

    fn enqueue(&mut self, key: CopyKey) -> bool {
        if self.find_origin.is_some() {
            return false;
        }
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
    pub(super) fn request_copy_mode_find(&mut self, ctx: &ComponentContext<Self>) -> bool {
        let mut active = self.copy_mode.borrow_mut();
        let Some(mode) = active.as_mut() else {
            return false;
        };
        if Some(mode.pane) != self.tabs.active_id() {
            return false;
        }
        if mode.find_origin.is_some() {
            return true;
        }
        // Finish the motions already accepted before opening Find. Preserve the
        // intent across an earlier explicit Copy, which consumes the key queue.
        mode.find_origin = self.paste_origin();
        let pane = mode.pane;
        if mode.pending == Some(Pending::Stopped) {
            drop(active);
            self.finish_copy_or_find(pane, None, ctx);
        } else if mode.pending.is_none() {
            drop(active);
            self.drain_copy_keys(ctx);
        }
        true
    }

    pub(super) fn copy_mode_target(&self) -> Option<(TabId, &LiveTab)> {
        let id = self.tabs.active_id()?;
        let Page::Terminal(tab) = &self.tabs.get(id)?.page else {
            return None;
        };
        (!tab.find.open
            && tab.find_available()
            && !tab.files.entry_dialog_open()
            && !tab.files.filter_editing()
            && self.title_editor.is_none()
            && !self.clipboard.is_open())
        .then_some((id, tab))
    }

    pub(super) fn open_copy_mode(&mut self) -> bool {
        self.close_hints();
        let Some((id, tab)) = self
            .copy_mode_target()
            .filter(|_| self.picker.is_none() && self.switcher.is_none())
        else {
            self.keyboard.set_copy_mode(false);
            return false;
        };
        let frame = tab.session.frames.load();
        if self.copy_mode.borrow().is_some() {
            return true;
        }
        tab.selection.borrow_mut().take();
        self.keyboard.set_selection(false);
        *self.copy_mode.borrow_mut() = Some(ActiveCopy {
            pane: id,
            state: CopyMode::new(&frame),
            pending: None,
            queued: VecDeque::new(),
            find_origin: None,
            error: None,
        });
        self.keyboard.set_copy_mode(true);
        tab.surface.invalidator.invalidate();
        true
    }

    pub(super) fn select_all(
        &mut self,
        origin: Option<clipboard_paste::Origin>,
        ctx: &ComponentContext<Self>,
    ) {
        if origin.is_none() || self.paste_origin() != origin {
            // A pane switch can overtake a queued shortcut. Release its input
            // capture without changing an overlay already open on the new pane.
            if self.copy_mode.borrow().is_none() && self.hints.borrow().is_none() {
                self.keyboard.set_copy_mode(false);
            }
            return;
        }
        if !self.open_copy_mode() {
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
        if mode.find_origin.is_some()
            || matches!(
                mode.pending,
                Some(Pending::Clipboard(_) | Pending::SelectAll(_))
            )
        {
            return;
        }
        let request = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        mode.pending = Some(Pending::SelectAll(request));
        mode.queued.clear();
        mode.error = None;
        let (reply, result) = tokio::sync::oneshot::channel();
        let _ = tab.session.input.send(Command::SelectAll {
            screen: mode.state.screen,
            reply,
        });
        let id = mode.pane;
        ctx.spawn_background_with_rejection(
            move |_| {
                Message::SelectAllResult(
                    id,
                    request,
                    result
                        .blocking_recv()
                        .unwrap_or(Err("Pane closed before selecting")),
                )
            },
            Message::SelectAllResult(id, request, Err("Unable to receive the selection")),
        );
    }

    pub(super) fn finish_select_all(
        &mut self,
        id: TabId,
        request: u64,
        result: Result<CopyMode, &'static str>,
        ctx: &ComponentContext<Self>,
    ) {
        let mut active = self.copy_mode.borrow_mut();
        let Some(mode) = active
            .as_mut()
            .filter(|m| m.pane == id && m.pending == Some(Pending::SelectAll(request)))
        else {
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
        if self.tabs.active_id() != Some(id)
            || self.copy_mode_target().is_none()
            || mode.state.screen != (&*frame).into()
        {
            drop(active);
            self.finish_copy_or_find(id, None, ctx);
            return;
        }
        match result {
            Ok(mut selected) if selected.valid(&frame) => {
                // Preserve the viewport and its restore position when a user
                // selects everything after navigating in keyboard copy mode.
                selected.original_top = mode.state.original_top;
                selected.top = mode.state.top.max(frame.oldest_stable_row);
                mode.state = selected;
                mode.pending = None;
            }
            outcome => {
                let requested_find = mode.find_origin.is_some();
                mode.pending = Some(Pending::Stopped);
                mode.queued.clear();
                let reason = outcome
                    .err()
                    .unwrap_or("The terminal changed while selecting");
                mode.error = Some(format!("Copy mode · {reason} · Esc cancel"));
                if requested_find {
                    drop(active);
                    self.finish_copy_or_find(id, None, ctx);
                    return;
                }
            }
        }
        tab.surface.invalidator.invalidate();
        if mode.find_origin.is_some() || !mode.queued.is_empty() {
            drop(active);
            self.drain_copy_keys(ctx);
        }
    }

    pub(super) fn close_copy_mode(&mut self) {
        let Some(mode) = self.copy_mode.borrow_mut().take() else {
            return;
        };
        self.header.borrow_mut().take();
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

    fn finish_copy_or_find(
        &mut self,
        pane: TabId,
        selection: Option<w11term::interaction::FindSelection>,
        ctx: &ComponentContext<Self>,
    ) {
        let origin = self
            .copy_mode
            .borrow()
            .as_ref()
            .filter(|m| m.pane == pane)
            .and_then(|m| m.find_origin);
        if Find::same_origin(origin, self.paste_origin())
            && self.copy_mode_target().is_some_and(|(id, _)| id == pane)
        {
            self.show_find(false, None, selection, ctx);
        } else {
            self.close_copy_mode();
        }
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
            } else if mode.pending.is_none()
                && (mode.find_origin.is_some() || !mode.queued.is_empty())
            {
                drop(active);
                self.drain_copy_keys(ctx);
            }
            return;
        }
        drop(active);
        self.finish_copy_or_find(id, None, ctx);
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
            let pane = mode.pane;
            drop(active);
            self.finish_copy_or_find(pane, None, ctx);
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
                    mode.begin_clipboard(request);
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
        if mode.find_origin.is_some() && mode.pending.is_none() && mode.queued.is_empty() {
            let selection = mode.state.selection().and_then(|s| s.find_target(&frame));
            drop(active);
            self.finish_copy_or_find(pane, selection, ctx);
            return;
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
            let requested_find = mode.find_origin.is_some();
            mode.queued.clear();
            mode.pending = Some(Pending::Stopped);
            mode.error =
                Some("Copy mode · Could not locate the word in this screen · Esc to exit".into());
            if requested_find {
                drop(active);
                self.finish_copy_or_find(pane, None, ctx);
            }
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
        if mode.find_origin.is_some() || !mode.queued.is_empty() {
            drop(active);
            self.drain_copy_keys(ctx);
        }
    }

    pub(super) fn finish_keyboard_copy(
        &mut self,
        pane: TabId,
        request: u64,
        result: Result<(), String>,
        ctx: &ComponentContext<Self>,
    ) {
        let mut active = self.copy_mode.borrow_mut();
        let Some(mode) = active
            .as_mut()
            .filter(|m| m.pane == pane && m.pending == Some(Pending::Clipboard(request)))
        else {
            return;
        };
        mode.pending = None;
        if mode.find_origin.is_some() {
            drop(active);
            self.finish_copy_or_find(pane, None, ctx);
        } else if let Err(e) = result {
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
    fn find_selected_text_after_a_word_and_copy_remembers_the_original_pane() {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let terminal = w11term::terminal::create(TerminalSize::default(), tx);
        let frame = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        let mut mode = ActiveCopy {
            pane: 2,
            state: CopyMode::new(&frame),
            pending: Some(Pending::Word(7)),
            queued: VecDeque::new(),
            find_origin: None,
            error: None,
        };
        assert!(!mode.enqueue(CopyKey::Right));
        assert!(!mode.enqueue(CopyKey::Copy));
        let origin = clipboard_paste::Origin {
            pane: 2,
            group: 4,
            connection: 6,
        };
        mode.find_origin = Some(origin);
        assert!(!mode.enqueue(CopyKey::Left));
        assert_eq!(mode.queued.pop_front(), Some(CopyKey::Right));
        assert_eq!(mode.queued.pop_front(), Some(CopyKey::Copy));
        mode.begin_clipboard(8);
        assert_eq!(mode.find_origin, Some(origin));
        assert!(mode.queued.is_empty());
    }

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
            find_origin: None,
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
