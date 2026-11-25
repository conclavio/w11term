//! Map the terminal's logical grid to the displayed row. DirectWrite shapes the
//! contents of a directional run, while selection and the VT model retain cell indices.
use std::ops::Range;
use wezterm_bidi::{
    BidiClass, BidiContext, Direction, ParagraphDirectionHint, bidi_class_for_char,
};
use wezterm_term::{Line, line_width_scale};

#[derive(Debug, Default, Clone)]
pub struct CellMap {
    cols: usize,
    width_scale: usize,
    visual: Vec<usize>,
    logical: Vec<usize>,
    rtl: Vec<bool>,
}

struct PinnedCells {
    columns: Range<usize>,
    char_boundary: usize,
    rtl: bool,
}

impl CellMap {
    pub fn new(line: &Line, grid_cols: usize) -> Self {
        let width_scale = line_width_scale(line);
        let cols = (grid_cols / width_scale).max(grid_cols.min(1));
        let (enabled, hint) = line.bidi_info();
        let identity = Self {
            cols,
            width_scale,
            ..Self::default()
        };
        if !enabled || cols == 0 {
            return identity;
        }
        let has_rectangles = line.has_multicell_text();
        if hint == ParagraphDirectionHint::LeftToRight
            && line
                .visible_cells()
                .take_while(|cell| cell.cell_index() < cols)
                .all(|cell| {
                    let text = if has_rectangles {
                        cell.attrs()
                            .multicell_row()
                            .map_or(cell.str(), |slice| slice.run().text())
                    } else {
                        cell.str()
                    };
                    !text.chars().any(needs_bidi)
                })
        {
            return identity;
        }
        // UAX #9 works on scalar values. Keep the cell owner for each one so
        // combining marks, emoji sequences and double-width cells move as units.
        let mut chars = Vec::new();
        let mut owners = Vec::new();
        let mut widths = vec![1; cols];
        let mut pinned = Vec::new();
        let mut end = 0;
        for cell in line
            .visible_cells()
            .take_while(|cell| cell.cell_index() < cols)
        {
            let col = cell.cell_index();
            let width = cell.width().max(1).min(cols - col);
            widths[col] = width;
            end = col + width;
            if has_rectangles && let Some(slice) = cell.attrs().multicell_row() {
                // The continuation cell contains a space. Shape all slices with
                // the direction of the same complete run.
                pinned.push(PinnedCells {
                    columns: col..end,
                    char_boundary: chars.len(),
                    rtl: first_strong_direction(slice.run().text().chars())
                        == Some(Direction::RightToLeft),
                });
                continue;
            }
            for ch in cell.str().chars() {
                // A remote combining sequence can be far larger than the grid.
                // Bound UAX work independently of its retained text size.
                if chars.len() >= 32_768 {
                    return identity;
                }
                chars.push(ch);
                owners.push(col);
            }
        }
        for col in end..cols {
            chars.push(' ');
            owners.push(col);
        }
        let mut map = Self {
            cols,
            width_scale,
            visual: vec![usize::MAX; cols],
            logical: (0..cols).collect(),
            rtl: vec![false; cols],
        };
        // Rectangles stay in the same columns on every row they occupy. Resolve
        // the spans between them independently, with a common paragraph base.
        let hint = match hint {
            ParagraphDirectionHint::AutoLeftToRight | ParagraphDirectionHint::AutoRightToLeft
                if !pinned.is_empty() =>
            {
                match first_strong_direction(chars.iter().copied()).unwrap_or(hint.direction()) {
                    Direction::LeftToRight => ParagraphDirectionHint::LeftToRight,
                    Direction::RightToLeft => ParagraphDirectionHint::RightToLeft,
                }
            }
            _ => hint,
        };
        let mut bidi = BidiContext::new();
        let mut col_start = 0;
        let mut char_start = 0;
        for fixed in pinned {
            let span = char_start..fixed.char_boundary;
            bidi.resolve_paragraph(&chars[span.clone()], hint);
            map.reorder(
                &bidi,
                &owners[span],
                &widths,
                col_start..fixed.columns.start,
            );
            col_start = fixed.columns.end;
            char_start = fixed.char_boundary;
            for col in fixed.columns {
                map.visual[col] = col;
                map.rtl[col] = fixed.rtl;
            }
        }
        bidi.resolve_paragraph(&chars[char_start..], hint);
        map.reorder(&bidi, &owners[char_start..], &widths, col_start..cols);
        map
    }

    fn reorder(
        &mut self,
        bidi: &BidiContext,
        owners: &[usize],
        widths: &[usize],
        columns: Range<usize>,
    ) {
        let mut visual = columns.start;
        for run in bidi.reordered_runs(0..owners.len()) {
            for index in run.indices {
                let col = owners[index];
                if self.visual[col] != usize::MAX {
                    continue;
                }
                for logical in col..col + widths[col] {
                    self.visual[logical] = visual;
                    self.logical[visual] = logical;
                    self.rtl[logical] = run.direction == Direction::RightToLeft;
                    visual += 1;
                }
            }
        }
        // The VT engine normally discards stand-alone zero-width controls. If a
        // producer supplies one in its own grid cell, keep the span hit-testable.
        if visual != columns.end {
            for col in columns {
                self.visual[col] = col;
                self.logical[col] = col;
                self.rtl[col] = false;
            }
        }
    }

    pub fn visual(&self, logical: usize) -> usize {
        self.visual.get(logical).copied().unwrap_or(logical)
    }

    fn logical(&self, visual: usize) -> usize {
        self.logical.get(visual).copied().unwrap_or(visual)
    }

    pub fn width_scale(&self) -> usize {
        self.width_scale.max(1)
    }

    pub fn logical_cols(&self) -> usize {
        self.cols
    }

    pub fn visual_grid(&self, col: usize) -> usize {
        self.visual(col) * self.width_scale()
    }

    pub fn logical_at_grid(&self, col: usize) -> usize {
        let scale = self.width_scale();
        let visual = if scale > 1 {
            (col / scale).min(self.cols.saturating_sub(1))
        } else {
            col
        };
        self.logical(visual)
    }

    pub fn rtl(&self, logical: usize) -> bool {
        self.rtl.get(logical).copied().unwrap_or(false)
    }

    pub fn visual_ranges(&self, logical: Range<usize>) -> Vec<Range<usize>> {
        let logical = logical.start..logical.end.min(self.cols);
        if logical.is_empty() {
            return Vec::new();
        }
        if self.visual.is_empty() {
            return std::iter::once(
                logical.start * self.width_scale()..logical.end * self.width_scale(),
            )
            .collect();
        }
        let mut spans: Vec<Range<usize>> = Vec::new();
        for (visual, col) in self.logical.iter().enumerate() {
            if logical.contains(col) {
                if let Some(previous) = spans.last_mut()
                    && previous.end == visual
                {
                    previous.end += 1;
                } else {
                    spans.push(visual..visual + 1);
                }
            }
        }
        for span in &mut spans {
            span.start *= self.width_scale();
            span.end *= self.width_scale();
        }
        spans
    }
}

fn first_strong_direction(chars: impl Iterator<Item = char>) -> Option<Direction> {
    // UAX #9 paragraph direction ignores the contents of directional isolates.
    let mut isolates = 0_usize;
    for ch in chars {
        match bidi_class_for_char(ch) {
            BidiClass::LeftToRightIsolate
            | BidiClass::RightToLeftIsolate
            | BidiClass::FirstStrongIsolate => isolates += 1,
            BidiClass::PopDirectionalIsolate => isolates = isolates.saturating_sub(1),
            BidiClass::LeftToRight if isolates == 0 => return Some(Direction::LeftToRight),
            BidiClass::RightToLeft | BidiClass::ArabicLetter if isolates == 0 => {
                return Some(Direction::RightToLeft);
            }
            _ => {}
        }
    }
    None
}

fn needs_bidi(ch: char) -> bool {
    !ch.is_ascii()
        && matches!(
            bidi_class_for_char(ch),
            BidiClass::ArabicLetter
                | BidiClass::RightToLeft
                | BidiClass::ArabicNumber
                | BidiClass::RightToLeftEmbedding
                | BidiClass::RightToLeftOverride
                | BidiClass::LeftToRightEmbedding
                | BidiClass::LeftToRightOverride
                | BidiClass::RightToLeftIsolate
                | BidiClass::LeftToRightIsolate
                | BidiClass::FirstStrongIsolate
                | BidiClass::PopDirectionalIsolate
                | BidiClass::PopDirectionalFormat
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wezterm_term::{CellAttributes, MulticellText};

    fn line(text: &str, hint: ParagraphDirectionHint) -> Line {
        let mut line = Line::from_text(text, &CellAttributes::default(), 0, None);
        line.set_bidi_info(true, hint, 1);
        line
    }

    #[test]
    fn mixed_directions_keep_numbers_and_graphemes_in_their_cells() {
        // Logical columns: A0, אְ1, ב2, 1=3, 2=4, ג5, 界6..8
        let map = CellMap::new(&line("Aאְב12ג界", ParagraphDirectionHint::LeftToRight), 10);
        assert_eq!(
            (0..10).map(|v| map.logical(v)).collect::<Vec<_>>(),
            [0, 5, 3, 4, 2, 1, 6, 7, 8, 9]
        );
        for col in 0..10 {
            assert_eq!(map.logical(map.visual(col)), col);
        }
        assert_eq!(map.visual_ranges(0..2), [0..1, 5..6]);
        assert!(map.rtl(1));
        assert!(!map.rtl(4));
    }

    #[test]
    fn explicit_rtl_moves_blank_columns_and_disabled_bidi_keeps_visual_order() {
        let mut line = line("אב👩‍💻", ParagraphDirectionHint::RightToLeft);
        let map = CellMap::new(&line, 8);
        assert_eq!(map.visual(0), 7);
        assert_eq!(map.visual(1), 6);
        assert_eq!(map.visual(3), map.visual(2) + 1);
        line.set_bidi_info(false, ParagraphDirectionHint::RightToLeft, 2);
        let map = CellMap::new(&line, 8);
        assert_eq!(
            (0..8).map(|c| map.visual(c)).collect::<Vec<_>>(),
            (0..8).collect::<Vec<_>>()
        );
    }

    fn insert_slice(line: &mut Line, col: usize, block: &std::sync::Arc<MulticellText>, row: u8) {
        let mut attrs = CellAttributes::default();
        attrs
            .set_explicit_width(true)
            .set_multicell_row(Some(block.row(row)));
        let text = if row == 0 { block.text() } else { " " };
        line.set_cell_grapheme(col, text, block.columns(), attrs, 2);
    }

    #[test]
    fn rectangle_columns_stay_fixed_between_directional_spans_on_each_row() {
        let block = MulticellText::new("(ب)", 1, 2).unwrap();
        let mut top = line("אב  גדהZ", ParagraphDirectionHint::LeftToRight);
        let mut bottom = line("AB  אבC ", ParagraphDirectionHint::LeftToRight);
        insert_slice(&mut top, 2, &block, 0);
        insert_slice(&mut bottom, 2, &block, 1);

        let top = CellMap::new(&top, 8);
        let bottom = CellMap::new(&bottom, 8);
        for (map, visual_order) in [
            (&top, [1, 0, 2, 3, 6, 5, 4, 7]),
            (&bottom, [0, 1, 2, 3, 5, 4, 6, 7]),
        ] {
            for (visual, logical) in visual_order.into_iter().enumerate() {
                assert_eq!(map.logical_at_grid(visual), logical);
                assert_eq!(map.visual_grid(logical), visual);
            }
            assert!(map.rtl(2));
            assert!(map.rtl(3));
        }
        assert_eq!(top.visual_ranges(1..5), [0..1, 2..4, 6..7]);
        assert!(top.rtl(0) && !bottom.rtl(0));
    }

    #[test]
    fn all_spans_share_the_rows_auto_direction() {
        let block = MulticellText::new("X", 1, 2).unwrap();
        let mut row = line("א  12Z", ParagraphDirectionHint::AutoLeftToRight);
        insert_slice(&mut row, 1, &block, 0);
        let map = CellMap::new(&row, 8);
        let rtl_tail = CellMap::new(&line("12Z", ParagraphDirectionHint::RightToLeft), 5);
        for col in 0..3 {
            assert_eq!(map.visual(col), col);
        }
        for col in 3..8 {
            assert_eq!(map.visual(col), 3 + rtl_tail.visual(col - 3));
        }
        assert!(!map.rtl(1));
    }
}
