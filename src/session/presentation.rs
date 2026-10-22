//! Committed presentation is separate from the parser's working state.
use crate::{
    prompts::Prompt,
    search::Search,
    terminal::{FrameStore, Snapshot},
};
use tokio::time::Instant;
use wezterm_term::{Terminal, TerminalState};
pub(super) const SYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);
pub(super) struct Metadata<'a> {
    pub status: &'a str,
    pub bytes: u64,
    pub search: &'a Search,
    pub prompt: Option<Prompt>,
    pub connection: u64,
}

pub(super) struct Presentation {
    frames: FrameStore,
    live: FrameStore,
    pending: Option<(Snapshot, Snapshot)>,
    generation: u64,
    pub dirty: bool,
    pub deadline: Option<Instant>,
}

impl Presentation {
    pub fn new(frames: FrameStore, live: FrameStore) -> Self {
        let generation = frames.load().generation;
        Self {
            frames,
            live,
            pending: None,
            generation,
            dirty: true,
            deadline: None,
        }
    }
    fn stage(&mut self, terminal: &TerminalState, offset: usize, status: &str, bytes: u64) {
        let previous = self.frames.load();
        let previous_live = self.live.load();
        let old = self.pending.as_ref().map(|p| &p.0).unwrap_or(&previous);
        let old_live = self
            .pending
            .as_ref()
            .map(|p| &p.1)
            .unwrap_or(&previous_live);
        let view = Snapshot::capture(terminal, Some(old), 0, offset, status.into(), bytes);
        let live = if offset == 0 {
            view.clone()
        } else {
            Snapshot::capture(terminal, Some(old_live), 0, 0, status.into(), bytes)
        };
        self.pending = Some((view, live));
    }
    pub fn advance(
        &mut self,
        terminal: &mut Terminal,
        bytes: impl AsRef<[u8]>,
        offset: usize,
        status: &str,
        total: u64,
    ) {
        terminal.advance_bytes_with_sync(bytes, |state, enabled| {
            // Begin commits the preceding unsynchronized prefix. End commits the
            // completed update even when another begin follows in the same read.
            // Counting newly received bytes must not repaint the old image at
            // begin. Publish the counter with the completed visual update.
            let committed_bytes = self.pending.as_ref().map_or_else(
                || self.frames.load().bytes_received,
                |pair| pair.0.bytes_received,
            );
            self.stage(
                state,
                offset,
                status,
                if enabled { committed_bytes } else { total },
            );
            self.deadline = enabled.then(|| Instant::now() + SYNC_TIMEOUT);
        });
        self.dirty = true;
    }
    pub fn expire(&mut self, terminal: &mut Terminal) {
        terminal.end_synchronized_output();
        self.deadline = None;
        self.dirty = true;
    }
    pub fn publish(&mut self, terminal: &Terminal, offset: usize, metadata: Metadata<'_>) {
        let Metadata {
            status,
            bytes,
            search,
            prompt,
            connection,
        } = metadata;
        let synchronized = terminal.synchronized_output();
        if !synchronized {
            self.deadline = None;
        }
        if self.dirty && !synchronized {
            self.stage(terminal, offset, status, bytes);
        }
        let has_new_content = self.pending.is_some();
        let (mut view, mut live) = self
            .pending
            .take()
            .unwrap_or_else(|| ((*self.frames.load()).clone(), (*self.live.load()).clone()));
        for snapshot in [&mut view, &mut live] {
            snapshot.status = status.into();
            snapshot.prompt = prompt.clone();
            snapshot.connection_generation = connection;
            if !synchronized || has_new_content {
                snapshot.search = search.snapshot(
                    snapshot.first_stable_row,
                    snapshot.rows.len(),
                    snapshot.alternate,
                );
            }
        }
        let view_changed = !view.same_presentation(&self.frames.load());
        let live_changed = !live.same_presentation(&self.live.load());
        if view_changed || live_changed {
            self.generation += 1;
        }
        if view_changed {
            view.generation = self.generation;
            self.frames.publish(view);
        }
        if live_changed {
            live.generation = self.generation;
            self.live.publish(live);
        }
        self.dirty = false;
    }
    pub fn capture(&self) -> (Snapshot, wezterm_term::TerminalSize) {
        let frame = (*self.live.load()).clone();
        let size = frame.size;
        (frame, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Terminal, Presentation) {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let terminal = crate::terminal::create(
            wezterm_term::TerminalSize {
                cols: 20,
                rows: 3,
                pixel_width: 200,
                pixel_height: 60,
                dpi: 96,
            },
            tx,
        );
        let snapshot = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
        (
            terminal,
            Presentation::new(FrameStore::new(snapshot.clone()), FrameStore::new(snapshot)),
        )
    }
    fn publish(p: &mut Presentation, t: &Terminal, offset: usize) {
        p.publish(
            t,
            offset,
            Metadata {
                status: "test",
                bytes: 0,
                search: &Search::default(),
                prompt: None,
                connection: 0,
            },
        );
    }
    #[test]
    fn repeated_begins_keep_original_deadline_and_reconnect_discards_hold() {
        let (mut terminal, mut p) = fixture();
        p.advance(&mut terminal, b"\x1b[?2026hpartial", 0, "test", 0);
        let deadline = p.deadline.unwrap();
        for _ in 0..10 {
            p.advance(&mut terminal, b"\x1b[?2026h", 0, "test", 0);
        }
        assert_eq!(p.deadline, Some(deadline));
        terminal.reset_connection();
        p.dirty = true;
        publish(&mut p, &terminal, 0);
        assert!(p.deadline.is_none());
        assert!(!terminal.synchronized_output());
    }
    #[test]
    fn synchronized_byte_counters_do_not_repaint_the_previous_frame() {
        let (mut terminal, mut p) = fixture();
        publish(&mut p, &terminal, 0);
        let generation = p.frames.load().generation;
        p.advance(&mut terminal, b"\x1b[?2026h", 0, "test", 4096);
        p.publish(
            &terminal,
            0,
            Metadata {
                status: "test",
                bytes: 4096,
                search: &Search::default(),
                prompt: None,
                connection: 0,
            },
        );
        assert_eq!(p.frames.load().generation, generation);
        p.advance(&mut terminal, b"new\x1b[?2026l", 0, "test", 8192);
        p.publish(
            &terminal,
            0,
            Metadata {
                status: "test",
                bytes: 8192,
                search: &Search::default(),
                prompt: None,
                connection: 0,
            },
        );
        assert_eq!(p.frames.load().generation, generation + 1);
        assert_eq!(p.frames.load().bytes_received, 8192);
    }
    #[test]
    fn held_scrollback_and_live_capture_remain_independent() {
        let (mut terminal, mut p) = fixture();
        p.advance(
            &mut terminal,
            b"one\r\ntwo\r\nthree\r\nfour\r\nfive",
            0,
            "test",
            0,
        );
        publish(&mut p, &terminal, 0);
        p.dirty = true;
        publish(&mut p, &terminal, 2);
        let old_view = p.frames.load();
        let old_live = p.capture().0;
        assert_ne!(old_view.first_stable_row, old_live.first_stable_row);
        p.advance(&mut terminal, b"\x1b[?2026h\rnew live", 2, "test", 0);
        publish(&mut p, &terminal, 2);
        assert!(p.frames.load().same_presentation(&old_view));
        assert!(p.capture().0.same_presentation(&old_live));
        p.advance(&mut terminal, b"\x1b[?2026l", 2, "test", 0);
        publish(&mut p, &terminal, 2);
        assert!(
            p.capture()
                .0
                .rows
                .iter()
                .any(|r| r.as_str().contains("new live"))
        );
        assert_eq!(p.frames.load().scroll_offset, 2);
    }
}
