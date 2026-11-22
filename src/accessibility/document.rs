use std::ops::{Range, RangeInclusive};
use std::sync::{Arc, OnceLock};
use w11term::{
    interaction::{Point, WordClass, word_class},
    terminal::{RetainedRows, Snapshot},
    text_layout::CellMap,
};
use windows::Win32::UI::Accessibility::*;

// The terminal caps launch geometry to 1000×500. Limit an accessible snapshot
// independently, including pathological combining-character sequences in a cell.
const MAX_CELLS: usize = 500_000;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Selection {
    pub anchor: Point,
    pub end: Point,
    pub end_exclusive: bool,
    pub block: bool,
}

pub(super) struct Document {
    text: String,
    pub(super) word_delimiters: String,
    word_starts: OnceLock<Vec<usize>>,
    // Grapheme/cell boundaries, including the exclusive end. Soft-wrapped lines
    // still retain individual screen rows for geometric and TextUnit_Line queries.
    offsets: Vec<usize>,
    cols: Vec<usize>,
    pub(super) cell_maps: Arc<[CellMap]>,
    // A wrap shares one text offset between the end of this row and column zero
    // of the next. Keep each exclusive right edge for hit-testing and rectangles.
    row_end_cols: Vec<usize>,
    paragraphs: Vec<usize>,
    pub(super) starts: Vec<usize>,
    pub(super) first: isize,
    retained: Option<RetainedRows>,
    pub(super) generation: u64,
    pub(super) caret: usize,
}

impl Document {
    #[cfg(test)]
    pub fn from_frame(frame: &Snapshot) -> Self {
        Self::with_word_delimiters(
            frame,
            w11term::interaction::Interaction::default().word_delimiters,
        )
    }

    pub fn with_word_delimiters(frame: &Snapshot, word_delimiters: String) -> Self {
        let mut d = Self {
            text: String::new(),
            word_delimiters,
            word_starts: OnceLock::new(),
            offsets: vec![0],
            cols: vec![],
            cell_maps: Arc::clone(frame.cell_maps()),
            row_end_cols: Vec::new(),
            paragraphs: Vec::new(),
            starts: vec![],
            first: frame.first_stable_row,
            retained: frame.rows.len().checked_sub(1).and_then(|last| {
                frame.retained_rows(
                    frame.first_stable_row,
                    frame.first_stable_row.checked_add(last as isize)?,
                )
            }),
            generation: frame.generation,
            caret: 0,
        };
        for (row, line) in frame.rows.iter().enumerate() {
            d.starts.push(d.len());
            d.row_end_cols.push(0);
            if row == 0 || !frame.row_is_wrapped(row - 1) {
                d.paragraphs.push(d.len());
            }
            let mut col = 0;
            for cell in line.visible_cells() {
                if cell.cell_index() >= frame.cols {
                    break;
                }
                while col < cell.cell_index() {
                    if !d.push(col, 1, " ") {
                        return d.finish(frame);
                    }
                    col += 1;
                }
                if !d.push(cell.cell_index(), cell.width(), cell.str()) {
                    return d.finish(frame);
                }
                col = cell.cell_index().saturating_add(cell.width());
            }
            // A cursor in an empty region is still a distinct insertion point.
            let cursor_row = frame.cursor.y.saturating_add(frame.scroll_offset as i64);
            if usize::try_from(cursor_row).ok() == Some(row) {
                while col < frame.cursor.x.min(frame.cols) {
                    if !d.push(col, 1, " ") {
                        return d.finish(frame);
                    }
                    col += 1;
                }
            }
            if row + 1 < frame.rows.len() && !frame.row_is_wrapped(row) && !d.push(col, 0, "\r\n") {
                break;
            }
        }
        d.finish(frame)
    }

    fn push(&mut self, col: usize, width: usize, text: &str) -> bool {
        if self.len() >= MAX_CELLS || self.text.len().saturating_add(text.len()) > MAX_BYTES {
            return false;
        }
        self.text.push_str(text);
        self.cols.push(col);
        *self
            .row_end_cols
            .last_mut()
            .expect("a cell belongs to a row") = col.saturating_add(width);
        self.offsets.push(self.text.len());
        true
    }

    fn finish(mut self, frame: &Snapshot) -> Self {
        self.caret = self.position(
            frame
                .cursor
                .y
                .saturating_add(frame.scroll_offset as i64)
                .max(0) as usize,
            frame.cursor.x,
        );
        self
    }

    pub fn len(&self) -> usize {
        self.cols.len()
    }
    pub fn text(&self, span: Range<usize>) -> &str {
        &self.text[self.offsets[span.start]..self.offsets[span.end]]
    }

    pub fn point(&self, offset: usize) -> Point {
        Point {
            row: self.first.saturating_add(self.row(offset) as isize),
            col: self.col(offset),
        }
    }
    pub fn row(&self, offset: usize) -> usize {
        self.starts
            .partition_point(|&start| start <= offset)
            .saturating_sub(1)
    }
    pub fn covered_rows(&self, span: &Range<usize>) -> RangeInclusive<usize> {
        // A nonempty range ends immediately before its exclusive endpoint.
        // A caret at that same endpoint belongs to the next row, if present.
        self.row(span.start)..=self.row(span.end.saturating_sub(1).max(span.start))
    }

    pub fn retained_rows(&self, span: &Range<usize>) -> Option<RetainedRows> {
        let rows = self.covered_rows(span);
        self.retained.as_ref()?.slice(
            self.first.checked_add(*rows.start() as isize)?,
            self.first.checked_add(*rows.end() as isize)?,
        )
    }

    pub fn visible_row(&self, row: usize, frame: &Snapshot) -> Option<usize> {
        let stable = self.first.checked_add(row as isize)?;
        let visible = usize::try_from(stable.checked_sub(frame.first_stable_row)?).ok()?;
        (visible < frame.rows.len()).then_some(visible)
    }
    pub fn end(&self, row: usize) -> usize {
        self.starts.get(row + 1).copied().unwrap_or(self.len())
    }
    pub fn col(&self, i: usize) -> usize {
        self.cols
            .get(i)
            .or_else(|| self.row_end_cols.last())
            .copied()
            .unwrap_or(0)
    }
    pub fn end_col(&self, row: usize, end: usize) -> usize {
        if end == self.end(row) {
            self.row_end_cols[row]
        } else {
            self.col(end)
        }
    }
    pub fn position(&self, row: usize, col: usize) -> usize {
        let Some(&start) = self.starts.get(row) else {
            return self.len();
        };
        let end = self.end(row);
        // Map a wide-cell continuation to its containing text character.
        let at = self.cols[start..end].partition_point(|&x| x <= col);
        if at == 0 {
            start
        } else if at == end - start && end == self.len() && self.col(end) <= col {
            end
        } else {
            start + at - 1
        }
    }

    fn stable_point(&self, point: Point, exclusive: bool) -> usize {
        if point.row < self.first {
            return 0;
        }
        let row = point.row.saturating_sub(self.first) as usize;
        let p = self.position(row, point.col);
        if exclusive && p < self.len() && self.row(p) == row && self.text(p..p + 1) != "\r\n" {
            p + 1
        } else {
            p
        }
    }

    pub fn selected(&self, selected: Option<Selection>) -> Vec<Range<usize>> {
        let Some(s) = selected else {
            return std::iter::once(self.caret..self.caret).collect();
        };
        let (a, b) = if s.anchor <= s.end {
            (s.anchor, s.end)
        } else {
            (s.end, s.anchor)
        };
        if !s.block {
            return std::iter::once(
                self.stable_point(a, false)..self.stable_point(b, !s.end_exclusive),
            )
            .collect();
        }
        (0..self.starts.len())
            .filter_map(|r| {
                let row = self.first.saturating_add(r as isize);
                (row >= a.row && row <= b.row).then(|| {
                    self.stable_point(
                        Point {
                            row,
                            col: a.col.min(b.col),
                        },
                        false,
                    )
                        ..self.stable_point(
                            Point {
                                row,
                                col: a.col.max(b.col),
                            },
                            true,
                        )
                })
            })
            .collect()
    }

    fn word(&self, i: usize) -> Range<usize> {
        let starts = self.word_starts.get_or_init(|| {
            let mut starts = Vec::new();
            let mut previous = None;
            for index in 0..self.len() {
                let cell = self.text(index..index + 1);
                if cell == "\r\n" {
                    starts.push(index);
                    previous = None;
                } else {
                    let class = word_class(cell, &self.word_delimiters);
                    // UIA includes trailing spaces in the preceding word or
                    // punctuation unit. Indentation after a hard break stands alone.
                    if previous.is_none()
                        || (class != WordClass::Whitespace && previous != Some(class))
                    {
                        starts.push(index);
                    }
                    previous = Some(class);
                }
            }
            starts.push(self.len());
            starts
        });
        let next = starts.partition_point(|&start| start <= i);
        starts[next - 1]..starts[next]
    }

    pub fn unit(&self, i: usize, unit: TextUnit) -> Range<usize> {
        if self.len() == 0 {
            return 0..0;
        }
        let i = i.min(self.len() - 1);
        if unit == TextUnit_Character {
            return i..i + 1;
        }
        if unit == TextUnit_Word {
            return self.word(i);
        }
        if unit == TextUnit_Paragraph {
            let p = self.paragraphs.partition_point(|&start| start <= i) - 1;
            return self.paragraphs[p]..self.paragraphs.get(p + 1).copied().unwrap_or(self.len());
        }
        if matches!(unit, TextUnit_Format | TextUnit_Line) {
            let r = self.row(i);
            return self.starts.get(r).copied().unwrap_or(0)..self.end(r);
        }
        0..self.len()
    }

    pub fn advance(&self, mut pos: usize, unit: TextUnit, count: i32) -> (usize, i32) {
        if unit == TextUnit_Character {
            let next = (pos as i64 + i64::from(count)).clamp(0, self.len() as i64) as usize;
            return (next, next as i32 - pos as i32);
        }
        let mut moved = 0;
        while moved != count {
            let next = if count > 0 {
                if pos == self.len() {
                    break;
                }
                self.unit(pos, unit).end
            } else {
                if pos == 0 {
                    break;
                }
                self.unit(pos - 1, unit).start
            };
            if next == pos {
                break;
            }
            moved += count.signum();
            pos = next;
        }
        (pos, moved)
    }

    pub fn find(
        &self,
        range: Range<usize>,
        query: &str,
        ignorecase: bool,
        backward: bool,
    ) -> Option<Range<usize>> {
        if query.is_empty() {
            return None;
        }
        let re = regex::RegexBuilder::new(&regex::escape(query))
            .case_insensitive(ignorecase)
            .build()
            .ok()?;
        let mut matches = re.find_iter(self.text(range.clone()));
        let found = if backward {
            matches.last()?
        } else {
            matches.next()?
        };
        let base = self.offsets[range.start];
        let start = self
            .offsets
            .partition_point(|&off| off <= base + found.start())
            .saturating_sub(1);
        let end = self
            .offsets
            .partition_point(|&off| off < base + found.end());
        Some(start..end.min(range.end))
    }
}

#[cfg(test)]
mod wrapped_text_tests {
    use super::*;

    #[test]
    fn a_soft_wrap_shares_a_text_offset_but_keeps_both_screen_rows() {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut term = w11term::terminal::create(
            wezterm_term::TerminalSize {
                rows: 4,
                cols: 8,
                ..Default::default()
            },
            tx,
        );
        term.advance_bytes("12界34XYcd\r\nnext".as_bytes());
        let doc = Document::from_frame(&Snapshot::capture(&term, None, 1, 0, String::new(), 0));
        let seam = doc.find(0..doc.len(), "xycd", true, false).unwrap();
        assert_eq!(doc.text(seam.clone()), "XYcd");
        let row = doc.unit(seam.start, TextUnit_Line);
        assert_eq!(doc.text(row.clone()), "12界34XY");
        assert_eq!(doc.end_col(0, row.end), 8);
        assert_eq!(doc.col(row.end), 0);
        assert_eq!(doc.point(row.end).row, doc.first + 1);
        assert_eq!(doc.text(doc.unit(row.end, TextUnit_Line)), "cd\r\n");
        assert_eq!(
            doc.text(doc.unit(row.end, TextUnit_Paragraph)),
            "12界34XYcd\r\n"
        );
        assert!(doc.find(0..doc.len(), "cdnext", false, false).is_none());
        let (next, moved) = doc.advance(row.start, TextUnit_Paragraph, 1);
        assert_eq!(moved, 1);
        assert!(
            doc.text(doc.unit(next, TextUnit_Paragraph))
                .starts_with("next")
        );
    }
}
