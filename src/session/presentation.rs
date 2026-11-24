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
    scrollback_clear_generation: u64,
    scrollback_clear_floor: Option<isize>,
    pub refresh_view: bool,
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
            scrollback_clear_generation: 0,
            scrollback_clear_floor: None,
            refresh_view: false,
            dirty: true,
            deadline: None,
        }
    }
    pub fn scrollback_cleared(&mut self, floor: isize) {
        self.scrollback_clear_generation += 1;
        self.scrollback_clear_floor = Some(floor);
        self.dirty = true;
    }
    /// Only the viewport changes. Prepare the potentially expensive snapshot
    /// before claiming a synchronous reader's bounded request.
    pub fn scrolled_view(&self, terminal: &Terminal, offset: usize, search: &Search) -> Snapshot {
        let old = self.frames.load();
        let mut view = Snapshot::capture(
            terminal,
            Some(&old),
            0,
            offset,
            old.status.clone(),
            old.bytes_received,
        );
        view.connection_generation = old.connection_generation;
        view.prompt = old.prompt.clone();
        view.scrollback_clear_generation = old.scrollback_clear_generation;
        view.scrollback_clear_floor = old.scrollback_clear_floor;
        view.search = search.snapshot_for_frame(&view);
        view
    }
    pub fn publish_scroll(&mut self, mut view: Snapshot) {
        self.generation += 1;
        view.generation = self.generation;
        self.frames.publish(view);
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
        let mut view = Snapshot::capture(terminal, Some(old), 0, offset, status.into(), bytes);
        let mut live = if offset == 0 {
            view.clone()
        } else {
            Snapshot::capture(terminal, Some(old_live), 0, 0, status.into(), bytes)
        };
        // Commit the selection cutoff alongside its rows at the same sync boundary.
        for snapshot in [&mut view, &mut live] {
            snapshot.scrollback_clear_generation = self.scrollback_clear_generation;
            snapshot.scrollback_clear_floor = self.scrollback_clear_floor;
        }
        self.pending = Some((view, live));
    }
    pub fn advance(
        &mut self,
        terminal: &mut Terminal,
        bytes: impl AsRef<[u8]>,
        offset: usize,
        status: &str,
        total: u64,
    ) -> usize {
        self.advance_at(terminal, bytes, offset, status, total, Instant::now())
    }
    pub fn advance_at(
        &mut self,
        terminal: &mut Terminal,
        bytes: impl AsRef<[u8]>,
        offset: usize,
        status: &str,
        total: u64,
        now: Instant,
    ) -> usize {
        let before = terminal.unscrolled_rows();
        let adjusted_offset = |state: &TerminalState| {
            offset.saturating_sub(state.unscrolled_rows().wrapping_sub(before))
        };
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
                adjusted_offset(state),
                status,
                if enabled { committed_bytes } else { total },
            );
            self.deadline = enabled.then(|| now + SYNC_TIMEOUT);
        });
        self.dirty = true;
        adjusted_offset(terminal)
    }
    pub fn expire(&mut self, terminal: &mut Terminal) {
        terminal.end_synchronized_output();
        self.deadline = None;
        self.dirty = true;
    }
    pub fn replay_time(&mut self, terminal: &mut Terminal, now: Instant) {
        terminal.set_kitty_animation_time(Some(now.into_std()));
        if self.deadline.is_some_and(|time| time <= now) {
            self.expire(terminal);
        }
        if terminal
            .kitty_animation_deadline()
            .is_some_and(|time| time <= now.into_std())
        {
            self.dirty |= terminal.advance_kitty_animations(now.into_std());
        }
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
                snapshot.search = search.snapshot_for_frame(snapshot);
            }
        }
        let view_changed =
            std::mem::take(&mut self.refresh_view) || !view.same_presentation(&self.frames.load());
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
    pub fn animation_deadline(&self, terminal: &Terminal) -> Option<Instant> {
        if terminal.synchronized_output() || terminal.kitty_animation_deadline().is_none() {
            return None;
        }
        self.frames
            .load()
            .animation_deadline(terminal)
            .map(Instant::from_std)
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
    fn debounced_find_marks_wait_for_a_rescan_after_reflow_or_screen_reset() {
        let (mut terminal, mut p) = fixture();
        let mut search = Search::default();
        let commit = |p: &mut Presentation, t: &Terminal, search: &Search| {
            p.publish(
                t,
                0,
                Metadata {
                    status: "test",
                    bytes: 0,
                    search,
                    prompt: None,
                    connection: 0,
                },
            );
        };
        p.advance(&mut terminal, b"abcdefghijhit", 0, "test", 0);
        search.configure("hit".into(), true, &terminal, 0);
        commit(&mut p, &terminal, &search);
        let initial = p.frames.load().search.as_ref().unwrap().clone();
        assert_eq!(&*initial.match_rows, &[0]);

        p.advance(&mut terminal, b"\r\nhit", 0, "test", 0);
        commit(&mut p, &terminal, &search);
        let unscanned = p.frames.load();
        let previous = unscanned.search.as_ref().unwrap();
        assert!(std::sync::Arc::ptr_eq(
            &previous.match_rows,
            &initial.match_rows
        ));
        assert_eq!(previous.selected_row, Some(0));
        assert_eq!(previous.total, 1);

        let mut size = terminal.get_size();
        size.cols = 8;
        terminal.resize(size);
        p.dirty = true;
        commit(&mut p, &terminal, &search);
        let scrolled = p.scrolled_view(&terminal, 1, &search);
        for frame in [&*p.frames.load(), &*p.live.load(), &scrolled] {
            let marks = frame.search.as_ref().unwrap();
            assert_eq!(marks.query, "hit");
            assert_eq!((marks.total, marks.selected), (1, Some(0)));
            assert!(marks.match_rows.is_empty());
            assert_eq!(marks.selected_row, None);
        }

        search.refresh(&terminal, 0);
        commit(&mut p, &terminal, &search);
        let reflowed = p.frames.load();
        let fresh = reflowed.search.as_ref().unwrap();
        assert_eq!(&*fresh.match_rows, &[1, 2]);
        assert_eq!(fresh.selected_row, Some(1));

        // Returning to the same-sized primary buffer changes its row identity.
        p.advance(&mut terminal, b"\x1b[?1049h\x1b[?1049l", 0, "test", 0);
        commit(&mut p, &terminal, &search);
        assert!(
            p.frames
                .load()
                .search
                .as_ref()
                .unwrap()
                .match_rows
                .is_empty()
        );
        search.refresh(&terminal, 0);
        commit(&mut p, &terminal, &search);
        assert_eq!(
            &*p.frames.load().search.as_ref().unwrap().match_rows,
            &[1, 2]
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
    fn reverse_screen_commits_once_and_reuses_unchanged_rows() {
        let (mut terminal, mut p) = fixture();
        p.advance(&mut terminal, b"steady", 0, "test", 0);
        publish(&mut p, &terminal, 0);
        let normal = p.frames.load();
        p.advance(&mut terminal, b"\x1b[?2026h\x1b[?5h", 0, "test", 0);
        publish(&mut p, &terminal, 0);
        assert_eq!(p.frames.load().generation, normal.generation);
        assert_eq!(p.live.load().palette, normal.palette);
        p.advance(&mut terminal, b"\x1b[?2026l", 0, "test", 0);
        publish(&mut p, &terminal, 0);
        let reverse = p.frames.load();
        assert_eq!(reverse.generation, normal.generation + 1);
        assert_eq!(reverse.palette.foreground, normal.palette.background);
        assert_eq!(reverse.palette.background, normal.palette.foreground);
        assert_eq!(p.live.load().palette, reverse.palette);
        assert!(std::sync::Arc::ptr_eq(&normal.rows[0], &reverse.rows[0]));
        p.advance(&mut terminal, b"\x1b[?5h\x1b[?5$p", 0, "test", 0);
        publish(&mut p, &terminal, 0);
        assert_eq!(p.frames.load().generation, reverse.generation);
        p.advance(&mut terminal, b"\x1b[?5l", 0, "test", 0);
        publish(&mut p, &terminal, 0);
        assert_eq!(p.frames.load().generation, reverse.generation + 1);
        assert_eq!(p.live.load().palette, normal.palette);
    }

    #[test]
    fn application_title_wake_waits_for_the_committed_synchronized_frame() {
        let (mut terminal, mut p) = fixture();
        let (tx, rx) = std::sync::mpsc::channel();
        let inbox = crate::events::Inbox::new(move || tx.send(()).unwrap());
        p.frames.watch_window(Some((&inbox, 1, true)));
        publish(&mut p, &terminal, 0);
        p.advance(
            &mut terminal,
            b"\x1b[?2026h\x1b]2;partial\x07",
            0,
            "test",
            0,
        );
        publish(&mut p, &terminal, 0);
        assert_eq!(p.frames.load().application_title(), None);
        assert!(rx.try_recv().is_err());
        p.advance(
            &mut terminal,
            b"\x1b]2;finished\x07\x1b[?2026l",
            0,
            "test",
            0,
        );
        publish(&mut p, &terminal, 0);
        assert!(rx.try_recv().is_ok());
        assert_eq!(p.frames.load().application_title(), Some("finished"));
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
        assert_eq!(old_live.last_visible_text_line().as_deref(), Some("five"));
        p.advance(&mut terminal, b"\x1b[?2026h\rnew live", 2, "test", 0);
        publish(&mut p, &terminal, 2);
        assert!(p.frames.load().same_presentation(&old_view));
        assert!(p.capture().0.same_presentation(&old_live));
        assert_eq!(
            p.live.load().last_visible_text_line().as_deref(),
            Some("five")
        );
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
        assert_eq!(
            p.live.load().last_visible_text_line().as_deref(),
            Some("new live")
        );
        assert_ne!(
            p.frames.load().last_visible_text_line().as_deref(),
            Some("new live")
        );
    }
}
