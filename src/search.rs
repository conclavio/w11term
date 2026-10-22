//! Literal scrollback search with stable cell coordinates, including soft wraps.
use crate::interaction::Point;
use std::collections::VecDeque;
use wezterm_term::{SemanticType, Terminal};

const LIMIT: usize = 10_000;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub spans: Vec<(Point, usize)>,
}
impl Match {
    pub fn start(&self) -> Point {
        self.spans[0].0
    }
}
#[derive(Clone, Debug, Default)]
pub struct SearchSnapshot {
    pub query: String,
    pub sensitive: bool,
    pub total: usize,
    pub selected: Option<usize>,
    pub truncated: bool,
    pub visible: Vec<(bool, Match)>,
}
#[derive(Default)]
pub struct Search {
    pub open: bool,
    pub query: String,
    sensitive: bool,
    matches: Vec<Match>,
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
        let changed = self.query != query || self.sensitive != sensitive || !self.open;
        self.open = true;
        self.query = query;
        self.sensitive = sensitive;
        if changed {
            self.selected = None;
            self.matches.clear();
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
        self.alternate = terminal.is_alt_screen_active();
        if !self.open || self.query.is_empty() {
            self.selected = None;
            return;
        }
        let query = if self.sensitive {
            self.query.clone()
        } else {
            self.query.chars().flat_map(char::to_lowercase).collect()
        };
        let screen = terminal.screen();
        // Stream a KMP matcher across cells. Keep only the query-sized coordinate
        // window, so even one logical line spanning all history stays bounded.
        let query = query.as_bytes();
        let mut prefix = vec![0; query.len()];
        for i in 1..query.len() {
            let mut j = prefix[i - 1];
            while j > 0 && query[i] != query[j] {
                j = prefix[j - 1];
            }
            if query[i] == query[j] {
                j += 1;
            }
            prefix[i] = j;
        }
        let mut cells: VecDeque<Cell> = VecDeque::new();
        let mut position = 0usize;
        let mut matched = 0usize;
        screen.for_each_phys_line(|row, line| {
            if self.truncated {
                return;
            }
            for cell in line.visible_cells() {
                let lower;
                let text = if self.sensitive {
                    cell.str()
                } else {
                    lower = cell
                        .str()
                        .chars()
                        .flat_map(char::to_lowercase)
                        .collect::<String>();
                    &lower
                };
                cells.push_back(Cell {
                    start: position,
                    end: position + text.len(),
                    point: Point {
                        row: screen.phys_to_stable_row_index(row),
                        col: cell.cell_index(),
                    },
                    width: cell.width(),
                });
                for byte in text.bytes() {
                    position += 1;
                    while matched > 0 && byte != query[matched] {
                        matched = prefix[matched - 1];
                    }
                    if byte == query[matched] {
                        matched += 1;
                    }
                    let start = position.saturating_sub(query.len());
                    while cells.front().is_some_and(|c| c.end <= start) {
                        cells.pop_front();
                    }
                    if matched == query.len() {
                        if self.matches.len() == LIMIT {
                            self.truncated = true;
                            return;
                        }
                        let mut spans: Vec<(Point, usize)> = Vec::new();
                        for cell in cells.iter().filter(|c| c.start < position && c.end > start) {
                            if let Some((point, right)) =
                                spans.last_mut().filter(|(p, _)| p.row == cell.point.row)
                            {
                                *right = (*right).max(cell.point.col + cell.width);
                                point.col = point.col.min(cell.point.col);
                            } else {
                                spans.push((cell.point, cell.point.col + cell.width));
                            }
                        }
                        if !spans.is_empty() {
                            self.matches.push(Match { spans });
                        }
                        matched = 0;
                    }
                }
            }
            if !line.last_cell_was_wrapped() {
                matched = 0;
                position = 0;
                cells.clear();
            }
        });
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
    pub fn snapshot(&self, first: isize, rows: usize, alternate: bool) -> Option<SearchSnapshot> {
        if !self.open || self.alternate != alternate {
            return None;
        }
        Some(SearchSnapshot {
            query: self.query.clone(),
            sensitive: self.sensitive,
            total: self.matches.len(),
            selected: self.selected,
            truncated: self.truncated,
            visible: self
                .matches
                .iter()
                .enumerate()
                .filter(|(_, m)| {
                    m.spans
                        .iter()
                        .any(|(p, _)| p.row >= first && p.row < first + rows as isize)
                })
                .map(|(i, m)| (Some(i) == self.selected, m.clone()))
                .collect(),
        })
    }
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
