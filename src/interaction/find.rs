use super::Selection;
use crate::{copy_mode::ScreenId, search::MAX_QUERY_BYTES, terminal::Snapshot};
use std::ops::Range;
use wezterm_term::TerminalState;

/// The exact stable screen spans selected when Find opened. Retain only row
/// revisions and column ranges, independently of the copy-mode viewport.
#[derive(Debug)]
pub struct FindSelection {
    screen: ScreenId,
    seqno: usize,
    first: isize,
    last: isize,
    live_top: isize,
    rows: Vec<(Range<usize>, Option<usize>)>,
}

impl Selection {
    pub fn find_target(&self, frame: &Snapshot) -> Option<FindSelection> {
        let first = self.anchor.row.min(self.end.row);
        let last = self.anchor.row.max(self.end.row);
        let screen = frame.into();
        if !self.can_copy(frame)
            || self.observed_screen.is_some_and(|id| id != screen)
            || (self.block && first != last)
            || last.abs_diff(first) > MAX_QUERY_BYTES
            || first < frame.oldest_stable_row
            || last >= frame.first_stable_row + (frame.scroll_offset + frame.rows.len()) as isize
        {
            return None;
        }
        let rows = (first..=last)
            .map(|row| {
                let observed = self.lines.get(&row).map(|line| line.current_seqno());
                Some((self.span(row, frame.cols)?, observed))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(FindSelection {
            screen,
            seqno: frame.seqno,
            first,
            last,
            live_top: frame.first_stable_row + frame.scroll_offset as isize,
            rows,
        })
    }
}

impl FindSelection {
    pub fn current(&self, frame: &Snapshot) -> bool {
        let visible = frame.first_stable_row..frame.first_stable_row + frame.rows.len() as isize;
        let mutable_start = self.first.max(self.live_top);
        let hidden_mutable = mutable_start <= self.last
            && !(visible.contains(&mutable_start) && visible.contains(&self.last));
        self.screen == frame.into()
            && (!hidden_mutable || frame.seqno <= self.seqno)
            && self.first >= frame.oldest_stable_row
            && self.last
                < frame.first_stable_row + (frame.scroll_offset + frame.rows.len()) as isize
            && frame.rows.iter().enumerate().all(|(index, line)| {
                let row = frame.first_stable_row + index as isize;
                !(self.first..=self.last).contains(&row) || line.current_seqno() <= self.seqno
            })
    }

    pub(crate) fn read(&self, terminal: &TerminalState, generation: u64) -> Option<String> {
        if terminal.synchronized_output() || !self.screen.matches(terminal, generation) {
            return None;
        }
        let screen = terminal.screen();
        let start = screen.stable_row_to_phys(self.first)?;
        let end = screen.stable_row_to_phys(self.last)?;
        if end.checked_sub(start)?.checked_add(1)? != self.rows.len() {
            return None;
        }
        let mut query = String::new();
        for (index, ((span, observed), line)) in self
            .rows
            .iter()
            .zip(screen.physical_lines().skip(start))
            .enumerate()
        {
            if line.current_seqno() > self.seqno
                || observed.is_some_and(|rev| rev != line.current_seqno())
                || (index + 1 < self.rows.len() && !line.last_cell_was_wrapped())
            {
                return None;
            }
            // Match the literal search engine's stream of graphemes. A wide
            // cell contributes its text once, including from its trailing half.
            for cell in line
                .visible_cells()
                .take_while(|c| c.cell_index() < span.end)
            {
                if cell.cell_index() + cell.width() <= span.start {
                    continue;
                }
                let text = cell.str();
                if text.len() > MAX_QUERY_BYTES - query.len() || text.contains(['\r', '\n']) {
                    return None;
                }
                query.push_str(text);
            }
        }
        (!query.is_empty()).then_some(query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::Point;
    use wezterm_term::{Terminal, TerminalSize};

    fn terminal(cols: usize, text: &str) -> Terminal {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut term = crate::terminal::create(
            TerminalSize {
                cols,
                rows: 3,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes(text);
        term
    }

    fn frame(term: &Terminal, offset: usize) -> Snapshot {
        Snapshot::capture(term, None, 1, offset, String::new(), 0)
    }

    fn selection(view: &Snapshot, start: (isize, usize), end: (isize, usize)) -> Selection {
        let mut selected = Selection::new(
            Point {
                row: start.0,
                col: start.1,
            },
            false,
            view.alternate,
        );
        selected.update(Point {
            row: end.0,
            col: end.1,
        });
        selected.active = true;
        selected
    }

    #[test]
    fn find_selected_text_reads_exact_graphemes_spaces_and_wraps_from_retained_history() {
        let text = "ab [雪e\u{301}]+  z";
        let term = terminal(8, &format!("{text}\r\nlater\r\nlines\r\nend"));
        let old = frame(&term, usize::MAX);
        let first = old.first_stable_row;
        // End on the second row and start inside the two-column wide glyph.
        let mut pointer = selection(&old, (first + 1, 2), (first, 5));
        pointer.observe(&old);
        let target = pointer.find_target(&old).unwrap();
        assert_eq!(target.read(&term, 0).as_deref(), Some("雪e\u{301}]+  "));
        assert!(target.current(&frame(&term, 0)));
        // Keyboard copy mode retains just endpoints when paging over output.
        let mut keyboard = selection(&old, (first, 3), (first + 1, 3));
        keyboard.cols = old.cols;
        let target = keyboard.find_target(&frame(&term, 0)).unwrap();
        assert_eq!(target.read(&term, 0).as_deref(), Some("[雪e\u{301}]+  z"));
        keyboard.block = true;
        assert!(keyboard.find_target(&old).is_none());
        keyboard.end.row = first;
        keyboard.end.col = 5;
        assert_eq!(
            keyboard
                .find_target(&old)
                .unwrap()
                .read(&term, 0)
                .as_deref(),
            Some("[雪")
        );
    }

    #[test]
    fn find_selected_text_rejects_hard_breaks_empty_and_over_budget_without_partial_results() {
        let term = terminal(8, "one\r\ntwo");
        let view = frame(&term, 0);
        let mut selected = selection(&view, (0, 1), (1, 0));
        selected.observe(&view);
        assert!(
            selected
                .find_target(&view)
                .unwrap()
                .read(&term, 0)
                .is_none()
        );
        selected.end = selected.anchor;
        selected.end_exclusive = true;
        assert!(
            selected
                .find_target(&view)
                .unwrap()
                .read(&term, 0)
                .is_none()
        );
        selected.active = false;
        assert!(selected.find_target(&view).is_none());

        let term = terminal(
            MAX_QUERY_BYTES,
            &format!("{}a", "é".repeat(MAX_QUERY_BYTES / 2)),
        );
        let view = frame(&term, 0);
        let mut selected = selection(&view, (0, 0), (0, MAX_QUERY_BYTES / 2 - 1));
        selected.observe(&view);
        assert_eq!(
            selected
                .find_target(&view)
                .unwrap()
                .read(&term, 0)
                .unwrap()
                .len(),
            MAX_QUERY_BYTES
        );
        selected.end.col += 1;
        assert!(
            selected
                .find_target(&view)
                .unwrap()
                .read(&term, 0)
                .is_none()
        );
    }

    #[test]
    fn find_selected_text_rejects_rewritten_rows_buffers_and_uncommitted_output() {
        let mut term = terminal(12, "old");
        let original = frame(&term, 0);
        let mut selected = selection(&original, (0, 0), (0, 2));
        selected.observe(&original);
        let target = selected.find_target(&original).unwrap();
        assert!(target.read(&term, 1).is_none());
        term.advance_bytes(b"\x1b[?2026h");
        assert!(target.read(&term, 0).is_none());
        term.advance_bytes(b"\rnew\x1b[?2026l");
        let changed = frame(&term, 0);
        assert!(!target.current(&changed));
        assert!(
            selected
                .find_target(&changed)
                .unwrap()
                .read(&term, 0)
                .is_none()
        );
        term.advance_bytes(b"\x1b[?1049h");
        assert!(selected.find_target(&frame(&term, 0)).is_none());
        assert!(target.read(&term, 0).is_none());
        term.resize(TerminalSize {
            cols: 15,
            rows: 3,
            ..Default::default()
        });
        assert!(target.read(&term, 0).is_none());
    }

    #[test]
    fn find_selected_text_rechecks_live_rows_hidden_before_the_answer_arrives() {
        let mut term = terminal(16, "first\r\nsecond\r\nthird\r\nfourth");
        let live = frame(&term, 0);
        let last = live.first_stable_row + 2;
        let mut selected = selection(&live, (last, 0), (last, 5));
        selected.observe(&live);
        let target = selected.find_target(&live).unwrap();
        assert_eq!(target.read(&term, 0).as_deref(), Some("fourth"));
        assert!(target.current(&frame(&term, usize::MAX)));
        term.advance_bytes(b"\rhidden");
        assert!(!target.current(&frame(&term, usize::MAX)));
    }
}
