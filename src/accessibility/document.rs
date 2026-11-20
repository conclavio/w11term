use std::ops::Range;
use w11term::{interaction::Point, terminal::Snapshot};
use windows::Win32::UI::Accessibility::*;

// The terminal caps launch geometry to 1000×500. Limit an accessible snapshot
// independently, including pathological combining-character sequences in a cell.
const MAX_CELLS: usize = 500_000;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Selection {
    pub anchor: Point,
    pub end: Point,
    pub block: bool,
}

pub(super) struct Document {
    text: String,
    // Grapheme/cell boundaries, including the exclusive end. Soft-wrapped lines
    // still retain individual screen rows for geometric and TextUnit_Line queries.
    offsets: Vec<usize>,
    cols: Vec<usize>,
    final_col: usize,
    pub(super) starts: Vec<usize>,
    pub(super) first: isize,
    pub(super) generation: u64,
    pub(super) caret: usize,
}

impl Document {
    pub fn from_frame(frame: &Snapshot) -> Self {
        let mut d = Self {
            text: String::new(),
            offsets: vec![0],
            cols: vec![],
            final_col: 0,
            starts: vec![],
            first: frame.first_stable_row,
            generation: frame.generation,
            caret: 0,
        };
        for (row, line) in frame.rows.iter().enumerate() {
            d.starts.push(d.len());
            d.final_col = 0;
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
            if row + 1 < frame.rows.len() && !d.push(col, 0, "\r\n") {
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
        self.final_col = col.saturating_add(width);
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
    pub fn end(&self, row: usize) -> usize {
        self.starts.get(row + 1).copied().unwrap_or(self.len())
    }
    pub fn col(&self, i: usize) -> usize {
        self.cols.get(i).copied().unwrap_or(self.final_col)
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
            return std::iter::once(self.stable_point(a, false)..self.stable_point(b, true))
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

    pub fn unit(&self, i: usize, unit: TextUnit) -> Range<usize> {
        if self.len() == 0 {
            return 0..0;
        }
        let i = i.min(self.len() - 1);
        if unit == TextUnit_Character {
            return i..i + 1;
        }
        if unit == TextUnit_Word {
            let space = |j| self.text(j..j + 1).chars().all(char::is_whitespace);
            let flag = space(i);
            let mut a = i;
            let mut b = i + 1;
            while a > 0 && space(a - 1) == flag {
                a -= 1;
            }
            while b < self.len() && space(b) == flag {
                b += 1;
            }
            return a..b;
        }
        if matches!(unit, TextUnit_Format | TextUnit_Line | TextUnit_Paragraph) {
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
