use super::{Cell, LIMIT, MAX_QUERY_BYTES, Match, Search, compile_regex, literal, scan_regex};
use crate::interaction::Point;
use regex::Regex;
use std::sync::Arc;
use wezterm_term::{Line, Terminal};

#[derive(Clone, Debug, Default)]
pub struct PaneSearchPreview {
    pub total: usize,
    pub truncated: bool,
    pub incomplete: bool,
    pub updating: bool,
    pub first: Option<(String, PaneSearchAnchor)>,
}

#[derive(Clone, Debug)]
pub struct PaneSearchQuery {
    text: String,
    sensitive: bool,
    pattern: Option<Regex>,
}

impl PaneSearchQuery {
    pub fn new(text: String, sensitive: bool, regex: bool) -> Result<Self, String> {
        if text.len() > MAX_QUERY_BYTES {
            return Err(format!("Search text exceeds {MAX_QUERY_BYTES} UTF-8 bytes"));
        }
        let pattern = regex.then(|| compile_regex(&text, sensitive)).transpose()?;
        Ok(Self {
            text,
            sensitive,
            pattern,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

impl From<String> for PaneSearchQuery {
    fn from(text: String) -> Self {
        Self {
            text,
            sensitive: false,
            pattern: None,
        }
    }
}

impl From<&str> for PaneSearchQuery {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}

/// A hit in one connection's active buffer. The row revisions catch rewrites
/// even if the same text reappears at the same coordinates.
#[derive(Clone, Debug)]
pub struct PaneSearchAnchor {
    origin: Arc<()>,
    query: PaneSearchQuery,
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
            || query != self.query.text
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
            sensitive: self.query.sensitive,
            regex: self.query.pattern.is_some(),
            compiled: self.query.pattern,
            alternate: self.alternate,
            ..Search::default()
        };
        search.collect_matches(terminal, &cancelled);
        if cancelled() {
            return None;
        }
        search.selected = Some(search.matches.iter().position(|hit| *hit == self.matched)?);
        search.update_match_rows(screen.row_epoch());
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
        query: PaneSearchQuery,
        cancelled: impl Fn() -> bool,
    ) -> Self {
        let mut preview = Self::default();
        if query.text.is_empty() || query.text.len() > MAX_QUERY_BYTES || cancelled() {
            return preview;
        }
        if terminal.synchronized_output() {
            preview.updating = true;
            return preview;
        }
        let mut first = None;
        (preview.truncated, preview.incomplete) = if let Some(pattern) = &query.pattern {
            scan_regex(terminal, pattern, &cancelled, |cells| {
                add_preview_hit(&mut preview.total, &mut first, cells.iter())
            })
        } else {
            (
                literal::scan(
                    terminal,
                    &query.text,
                    query.sensitive,
                    &cancelled,
                    |cells| add_preview_hit(&mut preview.total, &mut first, cells.iter()),
                ),
                false,
            )
        };
        if let Some(matched) = first
            && !cancelled()
        {
            let mut revisions = Vec::with_capacity(matched.spans.len());
            let rows = hit_rows(terminal, &matched).unwrap().inspect(|line| {
                revisions.push(line.current_seqno());
            });
            let text = snippet(rows, &matched);
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

fn add_preview_hit<'a>(
    total: &mut usize,
    first: &mut Option<Match>,
    cells: impl Iterator<Item = &'a Cell>,
) -> bool {
    if *total == LIMIT {
        return false;
    }
    *total += 1;
    if first.is_none() {
        *first = Match::from_cells(cells);
    }
    true
}

fn snippet<'a>(rows: impl Iterator<Item = &'a Line>, hit: &Match) -> String {
    // Keep the original spelling of the match and a little context on either
    // side. A single terminal cell can contain arbitrarily many combining
    // marks, so bound bytes as well as columns before copying to the UI.
    const CONTEXT: usize = 24;
    const MAX_CONTEXT_BYTES: usize = CONTEXT * 4;
    const MAX_MATCH_BYTES: usize = MAX_QUERY_BYTES * 4;
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
                if !oversize {
                    let end = text.floor_char_boundary(MAX_MATCH_BYTES - body.len());
                    body.push_str(&text[..end]);
                    oversize = end < text.len();
                }
            } else if after.len() < MAX_CONTEXT_BYTES {
                let end = text.floor_char_boundary(MAX_CONTEXT_BYTES - after.len());
                after.push_str(&text[..end]);
            }
        }
    }
    format!(
        "{}{}{}{}",
        before.trim_start(),
        body,
        if oversize { "…" } else { "" },
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
    fn regex_preview_and_reveal_share_case_wrapping_and_hard_newline_rules() {
        let id = Arc::new(());
        let term = screen(8, 6, "prefix RX-12\r\nrx-34\r\nRX-\r\n56");
        let query = r"RX-\d+";
        let sensitive = PaneSearchQuery::new(query.into(), true, true).unwrap();
        let preview = PaneSearchPreview::scan(&term, &id, sensitive, || false);
        assert_eq!(preview.total, 1);
        let (excerpt, anchor) = preview.first.unwrap();
        assert!(excerpt.contains("RX-12"));
        assert_eq!(anchor.matched.spans.len(), 2);
        let (find, _) = anchor
            .prepare_reveal(&term, &id, query.into(), 0, || false)
            .unwrap();
        assert!(find.regex && find.sensitive);
        assert_eq!(find.matches.len(), 1);
        assert_eq!(find.selected, Some(0));

        let insensitive = PaneSearchQuery::new(query.into(), false, true).unwrap();
        assert_eq!(
            PaneSearchPreview::scan(&term, &id, insensitive, || false).total,
            2
        );
        let whole_word = PaneSearchQuery::new("rx".into(), true, false).unwrap();
        assert_eq!(
            PaneSearchPreview::scan(&term, &id, whole_word, || false).total,
            1
        );
        assert!(PaneSearchQuery::new("[".into(), false, true).is_err());
    }

    #[test]
    fn regex_preview_reports_skipped_lines_and_keeps_a_bounded_excerpt() {
        let id = Arc::new(());
        let skipped = "b".repeat(super::super::MAX_REGEX_LINE_BYTES + 1);
        let shown = "a".repeat(MAX_QUERY_BYTES * 4 + 1);
        let term = screen(512, 4, &format!("{skipped}\r\n{shown}"));
        let query = PaneSearchQuery::new("[ab]+".into(), false, true).unwrap();
        let preview = PaneSearchPreview::scan(&term, &id, query, || false);
        assert!(preview.incomplete);
        assert_eq!(preview.total, 1);
        let (excerpt, _) = preview.first.unwrap();
        assert_eq!(excerpt, format!("{}…", "a".repeat(MAX_QUERY_BYTES * 4)));
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
        let frame = terminal::Snapshot::capture(&term, None, 1, offset, String::new(), 0);
        let result = find.snapshot_for_frame(&frame).unwrap();
        assert_eq!(&*result.match_rows, &[0]);
        assert_eq!(result.selected_row, Some(0));
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
