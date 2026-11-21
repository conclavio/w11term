use super::{LIMIT, MAX_QUERY_BYTES, Match, Search, literal};
use crate::interaction::Point;
use std::sync::Arc;
use wezterm_term::{Line, Terminal};

#[derive(Clone, Debug, Default)]
pub struct PaneSearchPreview {
    pub total: usize,
    pub truncated: bool,
    pub updating: bool,
    pub first: Option<(String, PaneSearchAnchor)>,
}

/// A hit in one connection's active buffer. The row revisions catch rewrites
/// even if the same text reappears at the same coordinates.
#[derive(Clone, Debug)]
pub struct PaneSearchAnchor {
    origin: Arc<()>,
    query: String,
    cols: usize,
    rows: usize,
    alternate: bool,
    matched: Match,
    revisions: Vec<usize>,
}

impl PaneSearchAnchor {
    pub fn start(&self) -> Point {
        self.matched.start()
    }

    pub(crate) fn prepare_reveal(
        self,
        terminal: &Terminal,
        origin: &Arc<()>,
        query: String,
        offset: usize,
        cancelled: impl Fn() -> bool,
    ) -> Option<(Search, usize)> {
        let screen = terminal.screen();
        if !Arc::ptr_eq(origin, &self.origin)
            || query != self.query
            || self.cols != screen.physical_cols
            || self.rows != screen.physical_rows
            || self.alternate != terminal.is_alt_screen_active()
            || terminal.synchronized_output()
            || cancelled()
        {
            return None;
        }
        if !hit_rows(terminal, &self.matched)?
            .map(Line::current_seqno)
            .eq(self.revisions)
        {
            return None;
        }
        // Build ordinary Find off to the side. Only the worker that delivers
        // the success acknowledgement will install it and change the viewport.
        let mut search = Search {
            open: true,
            query,
            alternate: self.alternate,
            ..Search::default()
        };
        search.collect_literal(terminal, &cancelled);
        if cancelled() {
            return None;
        }
        search.selected = Some(search.matches.iter().position(|hit| *hit == self.matched)?);
        let offset = search.reveal(terminal, offset);
        Some((search, offset))
    }
}

fn hit_rows<'a>(terminal: &'a Terminal, hit: &Match) -> Option<impl Iterator<Item = &'a Line>> {
    let screen = terminal.screen();
    let first = screen.stable_row_to_phys(hit.start().row)?;
    let last = screen.stable_row_to_phys(hit.spans.last()?.0.row)?;
    Some(screen.physical_lines().skip(first).take(last - first + 1))
}

impl PaneSearchPreview {
    pub(crate) fn scan(
        terminal: &Terminal,
        origin: &Arc<()>,
        query: String,
        cancelled: impl Fn() -> bool,
    ) -> Self {
        let mut preview = Self::default();
        if query.is_empty() || query.len() > MAX_QUERY_BYTES || cancelled() {
            return preview;
        }
        if terminal.synchronized_output() {
            preview.updating = true;
            return preview;
        }
        let mut first = None;
        preview.truncated = literal::scan(terminal, &query, false, &cancelled, |cells| {
            if preview.total == LIMIT {
                return false;
            }
            preview.total += 1;
            if first.is_none() {
                first = Match::from_cells(cells.iter());
            }
            true
        });
        if let Some(matched) = first
            && !cancelled()
        {
            let mut revisions = Vec::with_capacity(matched.spans.len());
            let rows = hit_rows(terminal, &matched).unwrap().inspect(|line| {
                revisions.push(line.current_seqno());
            });
            let text = snippet(rows, &matched, &query);
            let screen = terminal.screen();
            preview.first = Some((
                text,
                PaneSearchAnchor {
                    origin: Arc::clone(origin),
                    query,
                    cols: screen.physical_cols,
                    rows: screen.physical_rows,
                    alternate: terminal.is_alt_screen_active(),
                    matched,
                    revisions,
                },
            ));
        }
        preview
    }
}

fn snippet<'a>(rows: impl Iterator<Item = &'a Line>, hit: &Match, query: &str) -> String {
    // Keep the original spelling of the match and a little context on either
    // side. A single terminal cell can contain arbitrarily many combining
    // marks, so bound bytes as well as columns before copying to the UI.
    const CONTEXT: usize = 24;
    const MAX_CONTEXT_BYTES: usize = CONTEXT * 4;
    let mut before = String::new();
    let mut body = String::new();
    let mut after = String::new();
    let mut oversize = false;
    for (line, (point, right)) in rows.zip(&hit.spans) {
        for cell in line.visible_cells() {
            let col = cell.cell_index();
            if col >= right.saturating_add(CONTEXT) {
                break;
            }
            let text = cell.str();
            if col < point.col {
                if point.col - col <= CONTEXT {
                    if text.len() > MAX_CONTEXT_BYTES {
                        before.clear();
                    } else {
                        before.push_str(text);
                        if before.len() > MAX_CONTEXT_BYTES {
                            let keep = before.ceil_char_boundary(before.len() - MAX_CONTEXT_BYTES);
                            before.drain(..keep);
                        }
                    }
                }
            } else if col < *right {
                if !oversize && body.len() + text.len() <= MAX_QUERY_BYTES * 4 {
                    body.push_str(text);
                } else {
                    oversize = true;
                }
            } else if after.len() < MAX_CONTEXT_BYTES {
                let end = text.floor_char_boundary(MAX_CONTEXT_BYTES - after.len());
                after.push_str(&text[..end]);
            }
        }
    }
    format!(
        "{}{}{}",
        before.trim_start(),
        if oversize { query } else { &body },
        after.trim_end()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal;
    use tokio::sync::mpsc;
    use wezterm_term::TerminalSize;

    fn screen(cols: usize, rows: usize, text: &str) -> Terminal {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut terminal = terminal::create(
            TerminalSize {
                cols,
                rows,
                ..Default::default()
            },
            tx,
        );
        terminal.advance_bytes(text);
        terminal
    }

    fn scan(t: &Terminal, id: &Arc<()>, query: &str) -> PaneSearchPreview {
        PaneSearchPreview::scan(t, id, query.into(), || false)
    }

    #[test]
    fn pane_preview_and_reveal_join_wrapped_unicode_at_stable_cells() {
        let id = Arc::new(());
        let mut term = screen(8, 3, "prefix界e\u{301}AbC x\r\nthird\r\nlast\r\n");
        let query = "界E\u{301}abc";
        let before = term.current_seqno();
        let preview = scan(&term, &id, query);
        assert_eq!(term.current_seqno(), before);
        assert_eq!(preview.total, 1);
        let (snippet, anchor) = preview.first.unwrap();
        assert_eq!(snippet, "prefix界e\u{301}AbC x");
        assert_eq!(anchor.start(), Point { row: 0, col: 6 });
        assert_eq!(anchor.matched.spans.len(), 2);
        assert!(scan(&term, &id, "xthird").first.is_none());
        term.advance_bytes("later output\r\n");
        let (find, offset) = anchor
            .prepare_reveal(&term, &id, query.into(), 0, || false)
            .unwrap();
        assert!(offset > 0);
        assert_eq!(find.selected, Some(0));
        assert_eq!(find.matches.len(), 1);
        assert_eq!(find.matches[0].start(), Point { row: 0, col: 6 });
    }

    #[test]
    fn pane_reveal_rejects_changed_origin_buffer_geometry_query_and_row_revision() {
        let id = Arc::new(());
        let mut term = screen(15, 3, "test needle");
        let anchor = scan(&term, &id, "needle").first.unwrap().1;
        let fails = |t: &Terminal, id: &Arc<()>, query: &str| {
            anchor
                .clone()
                .prepare_reveal(t, id, query.into(), 0, || false)
                .is_none()
        };
        assert!(fails(&term, &Arc::new(()), "needle"));
        assert!(fails(&term, &id, "test"));
        assert!(
            anchor
                .clone()
                .prepare_reveal(&term, &id, "needle".into(), 0, || true)
                .is_none()
        );
        term.advance_bytes("\x1b[?1049h\x1b[Htest needle");
        assert!(fails(&term, &id, "needle"));
        let alt = scan(&term, &id, "needle").first.unwrap().1;
        term.advance_bytes("\x1b[?1049l");
        assert!(
            alt.prepare_reveal(&term, &id, "needle".into(), 0, || false)
                .is_none()
        );
        let size = term.get_size();
        term.resize(TerminalSize { cols: 14, ..size });
        assert!(fails(&term, &id, "needle"));
        let anchor = scan(&term, &id, "needle").first.unwrap().1;
        term.advance_bytes("\rtest needle");
        assert!(
            anchor
                .prepare_reveal(&term, &id, "needle".into(), 0, || false)
                .is_none()
        );
    }

    #[test]
    fn pane_reveal_rejects_evicted_history_and_preview_conceals_in_progress_updates() {
        let id = Arc::new(());
        let mut term = screen(12, 2, "needle");
        let anchor = scan(&term, &id, "needle").first.unwrap().1;
        term.advance_bytes("\r\n".repeat(10_010));
        assert!(
            anchor
                .prepare_reveal(&term, &id, "needle".into(), 0, || false)
                .is_none()
        );
        term.advance_bytes("\x1b[?2026hprivate");
        let held = scan(&term, &id, "private");
        assert!(held.updating && !held.truncated && held.first.is_none());
        term.advance_bytes("\x1b[?2026l");
        let committed = scan(&term, &id, "private");
        assert!(!committed.updating && committed.total == 1);
    }

    #[test]
    fn pane_preview_caps_real_hits_preserves_long_needles_and_stops_cancelled_work() {
        let id = Arc::new(());
        let mut term = screen(80, 3, &"z".repeat(LIMIT));
        let exact = scan(&term, &id, "z");
        assert_eq!(exact.total, LIMIT);
        assert!(!exact.truncated);
        term.advance_bytes("z");
        let capped = scan(&term, &id, "z");
        assert_eq!(capped.total, LIMIT);
        assert!(capped.truncated && !capped.updating);
        assert!(capped.first.unwrap().0.len() < 100);
        let long = "z".repeat(MAX_QUERY_BYTES);
        assert!(scan(&term, &id, &long).first.unwrap().0.contains(&long));
        assert!(scan(&term, &id, &(long + "z")).first.is_none());
        assert!(scan(&term, &id, "").first.is_none());
        let checks = std::cell::Cell::new(0);
        let cancelled = PaneSearchPreview::scan(&term, &id, "z".into(), || {
            checks.set(checks.get() + 1);
            checks.get() > 3
        });
        assert!(cancelled.total < LIMIT && cancelled.first.is_none());
    }
}
