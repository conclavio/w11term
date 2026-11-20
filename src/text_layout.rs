//! Map the terminal's logical grid to the displayed row. DirectWrite shapes the
//! contents of a directional run, while selection and the VT model retain cell indices.
use std::ops::Range;
use wezterm_bidi::{
    BidiClass, BidiContext, Direction, ParagraphDirectionHint, bidi_class_for_char,
};
use wezterm_term::Line;

#[derive(Debug, Default, Clone)]
pub struct CellMap {
    cols: usize,
    visual: Vec<usize>,
    logical: Vec<usize>,
    rtl: Vec<bool>,
}

impl CellMap {
    pub fn new(line: &Line, cols: usize) -> Self {
        let (enabled, hint) = line.bidi_info();
        let identity = Self {
            cols,
            ..Self::default()
        };
        if !enabled || cols == 0 {
            return identity;
        }
        if hint == ParagraphDirectionHint::LeftToRight
            && line
                .visible_cells()
                .take_while(|cell| cell.cell_index() < cols)
                .all(|cell| !cell.str().chars().any(needs_bidi))
        {
            return identity;
        }
        // UAX #9 works on scalar values. Keep the cell owner for each one so
        // combining marks, emoji sequences and double-width cells move as units.
        let mut chars = Vec::new();
        let mut owners = Vec::new();
        let mut widths = vec![1; cols];
        let mut end = 0;
        for cell in line
            .visible_cells()
            .take_while(|cell| cell.cell_index() < cols)
        {
            let col = cell.cell_index();
            let width = cell.width().max(1).min(cols - col);
            widths[col] = width;
            for ch in cell.str().chars() {
                // A remote combining sequence can be far larger than the grid.
                // Bound UAX work independently of its retained text size.
                if chars.len() >= 32_768 {
                    return identity;
                }
                chars.push(ch);
                owners.push(col);
            }
            end = col + width;
        }
        for col in end..cols {
            chars.push(' ');
            owners.push(col);
        }
        if chars.is_empty() {
            return identity;
        }
        let mut bidi = BidiContext::new();
        bidi.resolve_paragraph(&chars, hint);
        let mut map = Self {
            cols,
            visual: vec![usize::MAX; cols],
            logical: Vec::with_capacity(cols),
            rtl: vec![false; cols],
        };
        for run in bidi.reordered_runs(0..chars.len()) {
            for index in run.indices {
                let col = owners[index];
                if map.visual[col] != usize::MAX {
                    continue;
                }
                for logical in col..col + widths[col] {
                    map.visual[logical] = map.logical.len();
                    map.logical.push(logical);
                    map.rtl[logical] = run.direction == Direction::RightToLeft;
                }
            }
        }
        // The VT engine normally discards stand-alone zero-width controls. If a
        // producer supplies one in its own grid cell, keep the row hit-testable.
        if map.logical.len() != cols {
            return identity;
        }
        map
    }

    pub fn visual(&self, logical: usize) -> usize {
        self.visual.get(logical).copied().unwrap_or(logical)
    }

    pub fn logical(&self, visual: usize) -> usize {
        self.logical.get(visual).copied().unwrap_or(visual)
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
            return std::iter::once(logical).collect();
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
        spans
    }
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
    use wezterm_term::CellAttributes;

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
}
