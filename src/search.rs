//! Literal and regular expression scrollback search with stable cell coordinates, including soft wraps.
use crate::interaction::Point;
use regex::{Regex, RegexBuilder};
use std::sync::Arc;
use wezterm_term::{SemanticType, Terminal};

mod literal;
mod panes;
pub use panes::{PaneSearchAnchor, PaneSearchPreview};

const LIMIT: usize = 10_000;
pub const MAX_QUERY_BYTES: usize = 4096;
// Buffer one logical line and its cell offsets. Each stored cell contributes
// at least one byte, so this bound also limits the coordinate map.
const MAX_REGEX_LINE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub spans: Vec<(Point, usize)>,
}
impl Match {
    fn from_cells<'a>(cells: impl Iterator<Item = &'a Cell>) -> Option<Self> {
        let mut spans: Vec<(Point, usize)> = Vec::new();
        for cell in cells {
            if let Some((point, right)) = spans.last_mut().filter(|(p, _)| p.row == cell.point.row)
            {
                *right = (*right).max(cell.point.col + cell.width);
                point.col = point.col.min(cell.point.col);
            } else {
                spans.push((cell.point, cell.point.col + cell.width));
            }
        }
        (!spans.is_empty()).then_some(Self { spans })
    }

    pub fn start(&self) -> Point {
        self.spans[0].0
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchSnapshot {
    pub query: String,
    pub sensitive: bool,
    pub regex: bool,
    pub error: Option<String>,
    pub incomplete: bool,
    pub total: usize,
    pub selected: Option<usize>,
    /// Sorted, distinct stable physical rows on which a retained match starts.
    pub match_rows: Arc<[isize]>,
    pub selected_row: Option<isize>,
    pub truncated: bool,
    pub visible: Vec<(bool, Match)>,
}
#[derive(Default)]
pub struct Search {
    pub open: bool,
    pub query: String,
    sensitive: bool,
    regex: bool,
    compiled: Option<Regex>,
    error: Option<String>,
    incomplete: bool,
    matches: Vec<Match>,
    match_rows: Arc<[isize]>,
    match_row_epoch: u64,
    selected: Option<usize>,
    truncated: bool,
    alternate: bool,
}
struct Cell {
    start: usize,
    end: usize,
    point: Point,
    width: usize,
}

impl Search {
    pub fn configure(
        &mut self,
        query: String,
        sensitive: bool,
        terminal: &Terminal,
        offset: usize,
    ) -> usize {
        self.configure_with_mode(query, sensitive, false, terminal, offset)
    }

    pub fn configure_with_mode(
        &mut self,
        query: String,
        sensitive: bool,
        regex: bool,
        terminal: &Terminal,
        offset: usize,
    ) -> usize {
        let changed = self.query != query || self.sensitive != sensitive || self.regex != regex;
        if changed || !self.open {
            self.selected = None;
        }
        self.open = true;
        self.query = query;
        self.sensitive = sensitive;
        self.regex = regex;
        if changed {
            self.compiled = None;
            self.error = None;
            if self.query.len() > MAX_QUERY_BYTES {
                self.error = Some(format!("Search text exceeds {MAX_QUERY_BYTES} UTF-8 bytes"));
            } else if regex && !self.query.is_empty() {
                match compile_regex(&self.query, sensitive) {
                    Ok(pattern) => self.compiled = Some(pattern),
                    Err(error) => self.error = Some(error),
                }
            }
        }
        self.refresh(terminal, offset);
        self.reveal(terminal, offset)
    }
    pub fn refresh(&mut self, terminal: &Terminal, offset: usize) {
        let previous = self
            .selected
            .and_then(|i| self.matches.get(i))
            .map(Match::start);
        self.matches.clear();
        self.truncated = false;
        self.incomplete = false;
        self.alternate = terminal.is_alt_screen_active();
        if !self.open || self.query.is_empty() || self.error.is_some() {
            self.update_match_rows(terminal.screen().row_epoch());
            self.selected = None;
            return;
        }
        if let Some(pattern) = &self.compiled {
            (self.truncated, self.incomplete) = search_regex(terminal, pattern, &mut self.matches);
        } else {
            self.search_literal(terminal);
        }
        let screen = terminal.screen();
        self.update_match_rows(screen.row_epoch());
        let anchor = previous.unwrap_or(Point {
            row: screen.visible_row_to_stable_row(0) - offset as isize,
            col: 0,
        });
        self.selected = self
            .matches
            .iter()
            .position(|m| m.start() >= anchor)
            .or_else(|| self.matches.len().checked_sub(1));
    }
    fn search_literal(&mut self, terminal: &Terminal) {
        self.collect_literal(terminal, || false);
    }

    fn update_match_rows(&mut self, row_epoch: u64) {
        self.match_row_epoch = row_epoch;
        // Both scanners return matches in screen order. An output refresh can
        // change the number of hits on a row without moving any scrollbar marks.
        let rows = || {
            self.matches
                .chunk_by(|a, b| a.start().row == b.start().row)
                .map(|group| group[0].start().row)
        };
        if !rows().eq(self.match_rows.iter().copied()) {
            self.match_rows = rows().collect();
        }
    }

    fn collect_literal(&mut self, terminal: &Terminal, cancelled: impl Fn() -> bool) {
        self.truncated = literal::scan(terminal, &self.query, self.sensitive, cancelled, |cells| {
            if self.matches.len() == LIMIT {
                return false;
            }
            if let Some(hit) = Match::from_cells(cells.iter()) {
                self.matches.push(hit);
            }
            true
        });
    }

    pub fn navigate(&mut self, direction: isize, terminal: &Terminal, offset: usize) -> usize {
        if !self.matches.is_empty() {
            let n = self.matches.len() as isize;
            self.selected =
                Some((self.selected.unwrap_or(0) as isize + direction).rem_euclid(n) as usize);
        }
        self.reveal(terminal, offset)
    }
    fn reveal(&self, terminal: &Terminal, offset: usize) -> usize {
        if terminal.is_alt_screen_active() {
            return offset;
        }
        let Some(matched) = self.selected.and_then(|i| self.matches.get(i)) else {
            return offset;
        };
        let screen = terminal.screen();
        let live = screen.visible_row_to_stable_row(0);
        let top = live - offset as isize;
        let last = matched.spans.last().unwrap().0.row;
        if matched.start().row >= top && last < top + screen.physical_rows as isize {
            return offset;
        }
        let desired = matched.start().row - screen.physical_rows.saturating_sub(1) as isize / 2;
        (live - desired).max(0).min(screen.phys_row(0) as isize) as usize
    }
    pub(crate) fn snapshot_for_frame(
        &self,
        frame: &crate::terminal::Snapshot,
    ) -> Option<SearchSnapshot> {
        let mut result =
            self.snapshot(frame.first_stable_row, frame.rows.len(), frame.alternate)?;
        // Between a reflow/reset and the next debounced scan the query and
        // count remain useful, but its stable rows belong to a different grid.
        if frame.row_epoch != self.match_row_epoch {
            result.match_rows = Arc::default();
            result.selected_row = None;
        }
        Some(result)
    }

    pub fn snapshot(&self, first: isize, rows: usize, alternate: bool) -> Option<SearchSnapshot> {
        if !self.open || self.alternate != alternate {
            return None;
        }
        // Match spans are nonempty and ordered by both start and end, including
        // hits that start above this viewport and wrap onto its first row.
        let end = first.saturating_add_unsigned(rows);
        let start_index = self
            .matches
            .partition_point(|m| m.spans.last().unwrap().0.row < first);
        let visible = self.matches[start_index..]
            .iter()
            .take_while(|m| rows != 0 && m.start().row < end);
        Some(SearchSnapshot {
            query: self.query.clone(),
            sensitive: self.sensitive,
            regex: self.regex,
            error: self.error.clone(),
            incomplete: self.incomplete,
            total: self.matches.len(),
            selected: self.selected,
            match_rows: self.match_rows.clone(),
            selected_row: self.selected.map(|i| self.matches[i].start().row),
            truncated: self.truncated,
            visible: visible
                .enumerate()
                .map(|(i, m)| (Some(start_index + i) == self.selected, m.clone()))
                .collect(),
        })
    }
}

fn compile_regex(query: &str, sensitive: bool) -> Result<Regex, String> {
    RegexBuilder::new(query)
        .case_insensitive(!sensitive)
        .size_limit(1024 * 1024)
        .dfa_size_limit(256 * 1024)
        .build()
        .map_err(|error| match error {
            // The full diagnostic includes the pasted pattern and a multiline caret.
            regex::Error::Syntax(message) => message
                .lines()
                .rev()
                .find_map(|line| line.strip_prefix("error: "))
                .unwrap_or("Invalid regular expression")
                .chars()
                .take(160)
                .collect(),
            regex::Error::CompiledTooBig(_) => "Compiled pattern exceeds the 1 MiB limit".into(),
            _ => "Invalid regular expression".into(),
        })
}

/// Search complete hard lines. Running a regex on a clipped prefix can
/// fabricate an end anchor, a word boundary, or a shorter greedy match.
fn search_regex(terminal: &Terminal, pattern: &Regex, matches: &mut Vec<Match>) -> (bool, bool) {
    let screen = terminal.screen();
    let mut text = String::new();
    let mut cells = Vec::new();
    let mut skip_line = false;
    let mut incomplete = false;
    let mut truncated = false;
    screen.for_each_phys_line(|row, line| {
        if truncated {
            return;
        }
        if !skip_line {
            let stable = screen.phys_to_stable_row_index(row);
            for cell in line.visible_cells() {
                let value = cell.str();
                if value.is_empty() {
                    continue;
                }
                if value.len() > MAX_REGEX_LINE_BYTES - text.len() {
                    skip_line = true;
                    incomplete = true;
                    break;
                }
                let start = text.len();
                text.push_str(value);
                cells.push(Cell {
                    start,
                    end: text.len(),
                    point: Point {
                        row: stable,
                        col: cell.cell_index(),
                    },
                    width: cell.width(),
                });
            }
        }
        if line.last_cell_was_wrapped() && row + 1 < screen.scrollback_rows() {
            return;
        }
        if !skip_line {
            for found in pattern.find_iter(&text).filter(|m| !m.is_empty()) {
                if matches.len() == LIMIT {
                    truncated = true;
                    break;
                }
                // The regexp can start or end inside a grapheme. Include the
                // full terminal cell on either side, including its wide columns.
                let first = cells.partition_point(|c| c.end <= found.start());
                let end = cells.partition_point(|c| c.start < found.end());
                if let Some(hit) = Match::from_cells(cells[first..end].iter()) {
                    matches.push(hit);
                }
            }
        }
        skip_line = false;
        text.clear();
        cells.clear();
    });
    (truncated, incomplete)
}

pub fn max_offset(terminal: &Terminal) -> usize {
    terminal.screen().phys_row(0)
}
pub fn prompt_offset(terminal: &mut Terminal, offset: usize, direction: isize) -> usize {
    if terminal.is_alt_screen_active() {
        return offset;
    }
    let screen = terminal.screen();
    let live = screen.visible_row_to_stable_row(0);
    let anchor = live - offset as isize;
    let Ok(zones) = terminal.get_semantic_zones() else {
        return offset;
    };
    let rows = zones
        .iter()
        .filter(|z| z.semantic_type == SemanticType::Prompt)
        .map(|z| z.start_y);
    let target = if direction < 0 {
        rows.filter(|row| *row < anchor).max()
    } else {
        rows.filter(|row| *row > anchor).min()
    };
    target.map_or(offset, |row| {
        (live - row).max(0).min(max_offset(terminal) as isize) as usize
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal;
    use tokio::sync::mpsc;
    use wezterm_term::TerminalSize;
    #[test]
    fn find_marks_share_sorted_start_rows_across_navigation_and_paints() {
        let term = screen(8, 3, "aa aa\r\n.......aa\r\nnone\r\naa aa");
        for regex in [false, true] {
            let mut search = Search::default();
            let query = if regex { "a{2}" } else { "aa" };
            let offset =
                search.configure_with_mode(query.into(), true, regex, &term, max_offset(&term));
            let first = search.snapshot(0, 3, false).unwrap();
            assert_eq!(first.total, 5);
            assert_eq!(&*first.match_rows, &[0, 1, 4]);
            assert_eq!(first.selected_row, Some(0));
            let offset = search.navigate(1, &term, offset);
            assert_eq!(search.snapshot(0, 3, false).unwrap().selected_row, Some(0));
            search.navigate(1, &term, offset);
            // Only the second half of the selected hit is in this viewport.
            let tail = search.snapshot(2, 1, false).unwrap();
            assert!(Arc::ptr_eq(&first.match_rows, &tail.match_rows));
            assert_eq!(tail.selected_row, Some(1));
            assert_eq!(tail.visible.len(), 1);
            assert!(tail.visible[0].0);
            assert_eq!(tail.visible[0].1.spans.len(), 2);
            assert!(search.snapshot(2, 0, false).unwrap().visible.is_empty());
            search.refresh(&term, 0);
            assert!(Arc::ptr_eq(&first.match_rows, &search.match_rows));
            search.open = false;
            assert!(search.snapshot(0, 3, false).is_none());
            search.configure_with_mode("[".into(), false, true, &term, 0);
            let invalid = search.snapshot(0, 3, false).unwrap();
            assert!(invalid.match_rows.is_empty());
            assert_eq!(invalid.selected_row, None);
        }
    }

    #[test]
    fn find_marks_follow_eviction_clear_rewrites_and_screen_switches() {
        let (tx, _) = mpsc::unbounded_channel();
        let mut term = terminal::create_with_profile(
            TerminalSize {
                cols: 20,
                rows: 2,
                ..Default::default()
            },
            tx,
            2u32.try_into().unwrap(),
            crate::options::ColorScheme::Default,
        );
        let mut search = Search::default();
        term.advance_bytes("hit\r\nhit\r\nhit\r\nhit");
        search.configure("hit".into(), true, &term, 0);
        let initial = search.match_rows.clone();
        assert_eq!(&*initial, &[0, 1, 2, 3]);
        term.advance_bytes("\r\nhit");
        search.refresh(&term, 0);
        assert_eq!(&*search.match_rows, &[1, 2, 3, 4]);
        assert_eq!(&*initial, &[0, 1, 2, 3]);
        term.erase_scrollback();
        search.refresh(&term, 0);
        assert_eq!(&*search.match_rows, &[3, 4]);
        term.advance_bytes("\rnew");
        search.refresh(&term, 0);
        assert_eq!(&*search.match_rows, &[3]);
        term.advance_bytes("\x1b[?1049h\x1b[Halt hit");
        search.refresh(&term, 0);
        assert!(search.snapshot(0, 2, false).is_none());
        assert_eq!(&*search.snapshot(0, 2, true).unwrap().match_rows, &[0]);
        term.advance_bytes("\x1b[?1049l");
        search.refresh(&term, 0);
        assert_eq!(&*search.match_rows, &[3]);
    }

    #[test]
    fn wrapped_unicode_search_navigation_and_alternate_isolation() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut term = terminal::create(
            TerminalSize {
                cols: 8,
                rows: 3,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes("prefix界e\u{301}abc\r\nsecond ABC\r\nthird\r\nlast\r\n");
        let mut search = Search::default();
        let offset = search.configure("界e\u{301}abc".into(), false, &term, 0);
        assert_eq!(search.matches.len(), 1);
        assert_eq!(search.matches[0].spans.len(), 2);
        assert_eq!(search.matches[0].spans[0].0.col, 6);
        assert!(offset > 0);
        let offset = search.configure("abc".into(), false, &term, offset);
        assert_eq!(search.matches.len(), 2);
        let first = search.selected;
        search.navigate(1, &term, offset);
        assert_ne!(search.selected, first);
        search.navigate(1, &term, offset);
        assert_eq!(search.selected, first);
        search.configure("abc".into(), true, &term, offset);
        assert_eq!(search.matches.len(), 1);
        term.advance_bytes(b"\x1b[?1049halt");
        search.refresh(&term, offset);
        assert!(search.matches.is_empty());
        term.advance_bytes(b"\x1b[?1049l");
        search.refresh(&term, offset);
        assert_eq!(search.matches.len(), 1);
    }
    #[test]
    fn hard_newlines_case_mapping_reflow_and_match_cap() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut term = terminal::create(
            TerminalSize {
                cols: 20,
                rows: 3,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes("abc\r\ndef ΟΣ\r\n");
        let mut search = Search::default();
        search.configure("cdef".into(), true, &term, 0);
        assert!(search.matches.is_empty());
        search.configure("ΟΣ".into(), false, &term, 0);
        assert_eq!(search.matches.len(), 1);
        term.resize(TerminalSize {
            cols: 5,
            rows: 3,
            ..Default::default()
        });
        search.refresh(&term, 0);
        assert_eq!(search.matches.len(), 1);
        term.advance_bytes("z".repeat(20_001));
        search.configure("z".into(), true, &term, 0);
        assert_eq!(search.matches.len(), LIMIT);
        assert!(search.truncated);
    }
    fn screen(cols: usize, rows: usize, text: &str) -> Terminal {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut term = terminal::create(
            TerminalSize {
                cols,
                rows,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes(text);
        term
    }

    #[test]
    fn regex_maps_wrapped_wide_and_combining_cells_and_respects_hard_breaks() {
        let mut term = screen(4, 6, "ab界e\u{301}42\r\nAB界E\u{301}09\r\nlast");
        let mut search = Search::default();
        let pattern = r"^ab界e\p{M}\d{2}$";
        search.configure_with_mode(pattern.into(), false, true, &term, 0);
        let snapshot = search.snapshot(0, 6, false).unwrap();
        assert!(snapshot.regex && snapshot.error.is_none());
        assert_eq!(snapshot.total, 2);
        assert_eq!(
            snapshot.visible[0].1.spans,
            [(Point { row: 0, col: 0 }, 4), (Point { row: 1, col: 0 }, 3),]
        );
        search.navigate(1, &term, 0);
        let selected = search.selected;
        // Unchanged input and screen refresh keep both the compiled pattern
        // and the navigation anchor. The clone keeps the original allocation live.
        let compiled = search.compiled.clone().unwrap();
        search.configure_with_mode(pattern.into(), false, true, &term, 0);
        assert_eq!(search.selected, selected);
        assert_eq!(
            compiled.as_str().as_ptr(),
            search.compiled.as_ref().unwrap().as_str().as_ptr()
        );
        search.configure_with_mode(pattern.into(), true, true, &term, 0);
        assert_eq!(search.matches.len(), 1);
        search.configure_with_mode(r"界|\p{M}".into(), true, true, &term, 0);
        assert_eq!(search.matches[0].spans, [(Point { row: 0, col: 2 }, 4)]);
        assert_eq!(search.matches[1].spans, [(Point { row: 1, col: 0 }, 1)]);
        search.configure_with_mode("(?s)42.*AB".into(), false, true, &term, 0);
        assert!(search.matches.is_empty());
        term.advance_bytes(b"\x1b[?1049h42AB");
        search.refresh(&term, 0);
        assert!(search.snapshot(0, 6, false).is_none());
        assert_eq!(search.snapshot(0, 6, true).unwrap().total, 1);
        term.advance_bytes(b"\x1b[?1049l");
        search.refresh(&term, 0);
        assert!(search.matches.is_empty());
    }

    #[test]
    fn invalid_regex_replaces_old_results_and_recovers_on_edit_or_mode_change() {
        let term = screen(32, 3, "[error] and [error]");
        let mut search = Search::default();
        search.configure_with_mode(r"\[error\]".into(), true, true, &term, 0);
        assert_eq!(search.matches.len(), 2);
        for (pattern, message) in [
            ("[".into(), "unclosed character class"),
            ("界".repeat(MAX_QUERY_BYTES / 3 + 1), "4096 UTF-8 bytes"),
            (r"\w{10000}".into(), "1 MiB limit"),
        ] {
            search.configure_with_mode(pattern.clone(), false, true, &term, 0);
            search.refresh(&term, 0);
            let snapshot = search.snapshot(0, 3, false).unwrap();
            assert_eq!(snapshot.query, pattern);
            let error = snapshot.error.unwrap();
            assert!(error.contains(message), "{error}");
            assert_eq!(error.lines().count(), 1);
            assert_eq!((snapshot.total, snapshot.selected), (0, None));
            assert!(!snapshot.truncated && !snapshot.incomplete);
            assert_eq!(search.navigate(1, &term, 0), 0);
        }
        search.configure("x".repeat(MAX_QUERY_BYTES + 1), true, &term, 0);
        assert!(
            search
                .error
                .as_deref()
                .unwrap()
                .contains("4096 UTF-8 bytes")
        );
        assert!(search.matches.is_empty());
        search.configure_with_mode("[".into(), true, true, &term, 0);
        search.configure("[".into(), true, &term, 0);
        let literal = search.snapshot(0, 3, false).unwrap();
        assert!(!literal.regex && literal.error.is_none());
        assert_eq!(literal.total, 2);
        search.configure_with_mode("error".into(), true, true, &term, 0);
        assert!(search.error.is_none() && search.matches.len() == 2);
        search.configure_with_mode(String::new(), true, true, &term, 0);
        assert!(search.error.is_none() && search.matches.is_empty());
    }

    #[test]
    fn regex_skips_whole_oversized_lines_with_explicit_partial_status() {
        let mut text = "x".repeat(MAX_REGEX_LINE_BYTES - 3);
        text.push_str("CUT");
        let exact = text.clone();
        text.push_str("tail\r\n");
        text.push_str(&exact);
        text.push_str("\r\nkept");
        let term = screen(128, 4, &text);
        let mut search = Search::default();
        search.configure_with_mode(r"CUT$|^tail|^kept$".into(), true, true, &term, 0);
        let snapshot = search.snapshot(0, usize::from(u16::MAX), false).unwrap();
        assert_eq!(
            snapshot.total, 2,
            "only the exact-size line and the final hard line may match"
        );
        assert!(snapshot.incomplete && !snapshot.truncated);
        assert!(snapshot.visible[0].1.start().row > (MAX_REGEX_LINE_BYTES / 128) as isize);
        search.configure("CUT".into(), true, &term, 0);
        assert_eq!(
            search.matches.len(),
            2,
            "literal KMP still scans the long line"
        );
        assert!(!search.incomplete);
    }

    #[test]
    fn regex_skips_zero_width_matches_and_caps_only_real_results() {
        let mut term = screen(120, 4, &"z".repeat(LIMIT));
        let mut search = Search::default();
        search.configure_with_mode(r"^|\b|$".into(), true, true, &term, 0);
        assert_eq!(search.matches.len(), 0);
        search.configure_with_mode(r"z|\b".into(), true, true, &term, 0);
        assert_eq!(search.matches.len(), LIMIT);
        assert!(!search.truncated);
        let marks = search.match_rows.clone();
        term.advance_bytes("\r\nz");
        search.refresh(&term, 0);
        assert_eq!(search.matches.len(), LIMIT);
        assert!(search.truncated && !search.incomplete);
        assert!(
            Arc::ptr_eq(&search.match_rows, &marks),
            "the capped-out row must have no mark"
        );
    }

    #[test]
    fn osc133_regions_and_prompt_navigation_survive_scrollback() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut term = terminal::create(
            TerminalSize {
                cols: 40,
                rows: 3,
                ..Default::default()
            },
            tx,
        );
        for i in 0..6 {
            term.advance_bytes(format!("\x1b]133;A\x07$ \x1b]133;B\x07echo {i}\r\n\x1b]133;C\x07output {i}\r\n\x1b]133;D;0\x07"));
        }
        let zones = term.get_semantic_zones().unwrap();
        assert!(
            zones
                .iter()
                .any(|z| z.semantic_type == SemanticType::Prompt)
        );
        assert!(zones.iter().any(|z| z.semantic_type == SemanticType::Input));
        assert!(
            zones
                .iter()
                .any(|z| z.semantic_type == SemanticType::Output)
        );
        let older = prompt_offset(&mut term, 0, -1);
        assert!(older > 0);
        assert!(prompt_offset(&mut term, older, -1) > older);
        assert!(prompt_offset(&mut term, older, 1) < older);
    }
}
