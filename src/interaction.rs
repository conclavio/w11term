//! Window interaction preferences and cell-based selections, independent of native UI.
use crate::terminal::Snapshot;
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::{collections::BTreeMap, sync::Arc};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TabSwitcherMode {
    Disabled,
    #[default]
    InOrder,
    Mru,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Interaction {
    pub copy_on_select: bool,
    pub trim_block_selection: bool,
    pub trim_paste: bool,
    pub snap_to_grid_on_resize: bool,
    pub tab_switcher_mode: TabSwitcherMode,
    pub scroll_to_zoom: bool,
    pub detect_urls: bool,
    pub osc52: bool,
    pub word_delimiters: String,
}
impl Default for Interaction {
    fn default() -> Self {
        Self {
            copy_on_select: true,
            trim_block_selection: true,
            trim_paste: true,
            snap_to_grid_on_resize: true,
            tab_switcher_mode: TabSwitcherMode::InOrder,
            scroll_to_zoom: true,
            detect_urls: true,
            osc52: true,
            word_delimiters: " /\\()\"'-:,.;<>~!@#$%^&*|+=[]{}?│".into(),
        }
    }
}
pub fn paste_text(text: &str, trim: bool) -> &str {
    if trim { text.trim_end() } else { text }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub row: isize,
    pub col: usize,
}
#[derive(Clone, Debug)]
pub struct Selection {
    pub anchor: Point,
    pub end: Point,
    pub block: bool,
    pub alternate: bool,
    pub active: bool,
    pub dragging: bool,
    pub cols: usize,
    lines: BTreeMap<isize, Arc<wezterm_term::Line>>,
}
impl Selection {
    pub fn new(point: Point, block: bool, alternate: bool) -> Self {
        Self {
            anchor: point,
            end: point,
            block,
            alternate,
            active: false,
            dragging: true,
            cols: 0,
            lines: BTreeMap::new(),
        }
    }
    pub fn observe(&mut self, frame: &Snapshot) {
        if self.cols != 0 && self.cols != frame.cols {
            self.active = false;
            self.dragging = false;
            self.lines.clear();
        }
        self.cols = frame.cols;
        for (i, line) in frame.rows.iter().enumerate() {
            self.lines
                .insert(frame.first_stable_row + i as isize, line.clone());
        }
        while self.lines.len() > 10000 {
            self.lines.pop_first();
        }
    }
    pub fn update(&mut self, point: Point) {
        if point != self.anchor {
            self.active = true;
        }
        self.end = point;
    }
    pub fn span(&self, row: isize, cols: usize) -> Option<Range<usize>> {
        if !self.active {
            return None;
        }
        let (first, last) = if self.anchor <= self.end {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        };
        if row < first.row || row > last.row {
            return None;
        }
        let (start, end) = if self.block {
            (
                self.anchor.col.min(self.end.col),
                self.anchor.col.max(self.end.col).saturating_add(1),
            )
        } else {
            (
                if row == first.row { first.col } else { 0 },
                if row == last.row {
                    last.col.saturating_add(1)
                } else {
                    cols
                },
            )
        };
        Some(start.min(cols)..end.min(cols))
    }
    pub fn text(&self, frame: &Snapshot, trim_block: bool) -> String {
        if self.alternate != frame.alternate || self.cols != frame.cols || !self.active {
            return String::new();
        }
        let mut output = String::new();
        let mut previous_wrapped = false;
        let mut first = true;
        let mut lines = self.lines.clone();
        for (i, line) in frame.rows.iter().enumerate() {
            lines.insert(frame.first_stable_row + i as isize, line.clone());
        }
        for (row, line) in &lines {
            let Some(range) = self.span(*row, frame.cols) else {
                continue;
            };
            if !first && (self.block || !previous_wrapped) {
                output.push_str("\r\n");
            }
            first = false;
            let mut text = String::new();
            let mut column = range.start;
            for cell in line.visible_cells() {
                if cell.cell_index() >= range.end {
                    break;
                }
                if cell.cell_index() + cell.width() <= range.start {
                    continue;
                }
                while column < cell.cell_index() {
                    text.push(' ');
                    column += 1;
                }
                text.push_str(cell.str());
                column = cell.cell_index() + cell.width();
            }
            if self.block && !trim_block {
                while column < range.end {
                    text.push(' ');
                    column += 1;
                }
            }
            previous_wrapped = line.last_cell_was_wrapped() && range.end == frame.cols;
            if (self.block && trim_block) || (!self.block && !previous_wrapped) {
                text.truncate(text.trim_end().len());
            }
            output.push_str(&text);
        }
        output
    }
}

pub fn word_range(frame: &Snapshot, point: Point, delimiters: &str) -> Range<usize> {
    let Some(line) = usize::try_from(point.row - frame.first_stable_row)
        .ok()
        .and_then(|i| frame.rows.get(i))
    else {
        return point.col..point.col + 1;
    };
    let cells: Vec<_> = line.visible_cells().collect();
    let Some(index) = cells
        .iter()
        .position(|c| c.cell_index() <= point.col && c.cell_index() + c.width() > point.col)
    else {
        return point.col..point.col + 1;
    };
    let category = |s: &str| {
        if s.chars().all(char::is_whitespace) {
            0
        } else if s.chars().any(|c| delimiters.contains(c)) {
            1
        } else {
            2
        }
    };
    let class = category(cells[index].str());
    let mut first = index;
    let mut last = index;
    while first > 0 && category(cells[first - 1].str()) == class {
        first -= 1;
    }
    while last + 1 < cells.len() && category(cells[last + 1].str()) == class {
        last += 1;
    }
    cells[first].cell_index()..cells[last].cell_index() + cells[last].width()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    pub spans: Vec<(isize, Range<usize>)>,
}
pub fn safe_url(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    !uri.chars().any(char::is_control)
        && (lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("mailto:"))
}
pub fn link_at(frame: &Snapshot, point: Point, detect: bool) -> Option<Link> {
    let index = usize::try_from(point.row - frame.first_stable_row).ok()?;
    let line = frame.rows.get(index)?;
    if let Some(cell) = line
        .visible_cells()
        .find(|c| c.cell_index() <= point.col && c.cell_index() + c.width() > point.col)
        && let Some(link) = cell.attrs().hyperlink()
        && safe_url(link.uri())
    {
        let uri = link.uri().to_owned();
        let spans = frame
            .rows
            .iter()
            .enumerate()
            .flat_map(|(y, line)| {
                line.visible_cells()
                    .filter(|c| c.attrs().hyperlink().is_some_and(|link| link.uri() == uri))
                    .map(|c| {
                        (
                            frame.first_stable_row + y as isize,
                            c.cell_index()..c.cell_index() + c.width(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        return Some(Link { uri, spans });
    }
    if !detect {
        return None;
    }
    let mut first = index;
    let mut last = index;
    while first > 0 && frame.rows[first - 1].last_cell_was_wrapped() {
        first -= 1;
    }
    while last + 1 < frame.rows.len() && frame.rows[last].last_cell_was_wrapped() {
        last += 1;
    }
    let mut text = String::new();
    let mut map = Vec::new();
    for y in first..=last {
        for cell in frame.rows[y]
            .visible_cells()
            .take_while(|c| c.cell_index() < frame.cols)
        {
            let start = text.len();
            text.push_str(cell.str());
            map.push((
                start..text.len(),
                Point {
                    row: frame.first_stable_row + y as isize,
                    col: cell.cell_index(),
                },
                cell.width(),
            ));
        }
    }
    for (start, c) in text.char_indices() {
        if !matches!(c, 'h' | 'H' | 'm' | 'M' | 'w' | 'W') {
            continue;
        }
        let tail = &text[start..];
        let lower = tail
            .chars()
            .take(8)
            .collect::<String>()
            .to_ascii_lowercase();
        if !(lower.starts_with("https://")
            || lower.starts_with("http://")
            || lower.starts_with("mailto:")
            || lower.starts_with("www."))
        {
            continue;
        }
        let end = tail
            .find(|c: char| {
                c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | '"' | '\'' | '`')
            })
            .unwrap_or(tail.len());
        let mut url = &tail[..end];
        while let Some(c) = url.chars().last() {
            let unbalanced = match c {
                ')' => url.matches(')').count() > url.matches('(').count(),
                ']' => url.matches(']').count() > url.matches('[').count(),
                '}' => url.matches('}').count() > url.matches('{').count(),
                _ => false,
            };
            if unbalanced || matches!(c, '.' | ',' | ';' | '!' | '?') {
                url = &url[..url.len() - c.len_utf8()];
            } else {
                break;
            }
        }
        let range = start..start + url.len();
        let spans: Vec<_> = map
            .iter()
            .filter(|(r, _, _)| r.start < range.end && r.end > range.start)
            .map(|(_, p, width)| (p.row, p.col..p.col + width))
            .collect();
        if spans
            .iter()
            .any(|(row, cols)| *row == point.row && cols.contains(&point.col))
        {
            let uri = if url.to_ascii_lowercase().starts_with("www.") {
                format!("https://{url}")
            } else {
                url.to_owned()
            };
            if safe_url(&uri) {
                return Some(Link { uri, spans });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn screen(bytes: &[u8], cols: usize) -> Snapshot {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = crate::terminal::create(
            wezterm_term::TerminalSize {
                cols,
                rows: 8,
                ..Default::default()
            },
            tx,
        );
        terminal.advance_bytes(bytes);
        Snapshot::capture(&terminal, None, 1, 0, "test".into(), 0)
    }
    fn selection(
        frame: &Snapshot,
        start: (usize, usize),
        end: (usize, usize),
        block: bool,
    ) -> Selection {
        let mut selection = Selection::new(
            Point {
                row: frame.first_stable_row + start.0 as isize,
                col: start.1,
            },
            block,
            frame.alternate,
        );
        selection.update(Point {
            row: frame.first_stable_row + end.0 as isize,
            col: end.1,
        });
        selection.observe(frame);
        selection
    }
    #[test]
    fn copying_joins_soft_wraps_trims_hard_lines_and_uses_plain_graphemes() {
        let frame = screen(b"\x1b[31m1234567890abc\x1b[0m\r\nnext   ", 10);
        let selected = selection(&frame, (0, 0), (2, 8), false);
        assert_eq!(selected.text(&frame, true), "1234567890abc\r\nnext");
        let frame = screen("a界e\u{301}z".as_bytes(), 20);
        assert_eq!(
            selection(&frame, (0, 2), (0, 3), false).text(&frame, true),
            "界e\u{301}"
        );
    }
    #[test]
    fn rectangular_copy_padding_and_reverse_selection() {
        let frame = screen(b"alpha\r\nb\r\ncat", 20);
        let selected = selection(&frame, (2, 4), (0, 0), true);
        assert_eq!(selected.text(&frame, true), "alpha\r\nb\r\ncat");
        assert_eq!(selected.text(&frame, false), "alpha\r\nb    \r\ncat  ");
        let mut other = frame.clone();
        other.alternate = !other.alternate;
        assert!(selected.text(&other, true).is_empty());
    }
    #[test]
    fn word_selection_and_paste_trim_preserve_internal_whitespace() {
        let frame = screen(b"hello/world  words", 30);
        let range = word_range(
            &frame,
            Point {
                row: frame.first_stable_row,
                col: 8,
            },
            &Interaction::default().word_delimiters,
        );
        assert_eq!(range, 6..11);
        assert_eq!(paste_text("one  \r\n  two \r\n\t ", true), "one  \r\n  two");
        assert_eq!(paste_text("x \r\n", false), "x \r\n");
    }
    #[test]
    fn urls_cover_wrapped_lines_explicit_links_and_safe_schemes() {
        let frame = screen(b"https://example.com/a(b).", 12);
        let point = Point {
            row: frame.first_stable_row + 1,
            col: 2,
        };
        let link = link_at(&frame, point, true).unwrap();
        assert_eq!(link.uri, "https://example.com/a(b)");
        assert!(
            link.spans
                .iter()
                .any(|(row, _)| *row == frame.first_stable_row)
        );
        assert!(link_at(&frame, point, false).is_none());
        let frame = screen(b"\x1b]8;;https://example.com\x1b\\label\x1b]8;;\x1b\\", 20);
        assert_eq!(
            link_at(
                &frame,
                Point {
                    row: frame.first_stable_row,
                    col: 1
                },
                false
            )
            .unwrap()
            .uri,
            "https://example.com"
        );
        assert!(!safe_url("javascript:alert(1)"));
        assert!(!safe_url("file:///C:/Windows/notepad.exe"));
        assert!(!safe_url("https://example.com\nattack"));
    }
    #[test]
    fn selection_remembers_rows_scrolled_out_of_the_viewport() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut terminal = crate::terminal::create(
            wezterm_term::TerminalSize {
                cols: 10,
                rows: 2,
                ..Default::default()
            },
            tx,
        );
        terminal.advance_bytes(b"first\r\nsecond");
        let first = Snapshot::capture(&terminal, None, 1, 0, "test".into(), 0);
        let mut selected = selection(&first, (0, 0), (1, 5), false);
        terminal.advance_bytes(b"\r\nthird\r\nfourth");
        let last = Snapshot::capture(&terminal, None, 2, 0, "test".into(), 0);
        selected.observe(&last);
        selected.update(Point {
            row: last.first_stable_row + 1,
            col: 5,
        });
        assert_eq!(
            selected.text(&last, true),
            "first\r\nsecond\r\nthird\r\nfourth"
        );
    }
}
