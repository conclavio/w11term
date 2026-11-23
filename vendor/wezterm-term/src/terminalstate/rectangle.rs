use super::*;
use wezterm_cell::SemanticType;

fn rectangle_axis(start: OneBased, end: OneBased, limits: Range<usize>) -> Range<usize> {
    if limits.is_empty() || start.as_one_based() > end.as_one_based() {
        return limits.start..limits.start;
    }
    let first = (start.as_zero_based() as usize).min(limits.len() - 1);
    let last = (end.as_one_based() as usize).min(limits.len());
    limits.start + first..limits.start + last
}

impl TerminalState {
    pub(super) fn screen_rectangle(
        &self,
        top: OneBased,
        left: OneBased,
        bottom: OneBased,
        right: OneBased,
    ) -> (Range<usize>, Range<usize>) {
        let screen = self.screen();
        let (row_limits, col_limits) = if self.dec_origin_mode {
            (
                self.top_and_bottom_margins.start as usize
                    ..(self.top_and_bottom_margins.end as usize).min(screen.physical_rows),
                self.left_and_right_margins.start
                    ..self.left_and_right_margins.end.min(screen.physical_cols),
            )
        } else {
            (0..screen.physical_rows, 0..screen.physical_cols)
        };
        (
            rectangle_axis(top, bottom, row_limits),
            rectangle_axis(left, right, col_limits),
        )
    }

    pub(super) fn rectangle_fill_cell(&self, character: u8) -> Cell {
        let mut utf8 = [0; 4];
        let glyph = char::from(character).encode_utf8(&mut utf8);
        let mut attrs = self.pen.clone();
        attrs.set_hyperlink(None);
        attrs.set_semantic_type(SemanticType::default());
        attrs.clear_images();
        // DEC fill writes one cell per column, even for an ambiguous-width glyph.
        Cell::new_grapheme_with_width(self.remap_grapheme(glyph), 1, attrs)
    }

    pub(super) fn paint_rectangle(
        &mut self,
        (rows, columns): (Range<usize>, Range<usize>),
        cell: &Cell,
    ) {
        let seqno = self.seqno;
        let screen = self.screen_mut();
        let is_default_blank = *cell == Cell::blank();
        for row in rows {
            let row = row as VisibleRowIndex;
            let width = screen.logical_cols(row);
            let physical = screen.phys_row(row);
            let line = screen.line_mut(physical);
            let mut end = columns.end.min(width);
            if is_default_blank {
                // The unwritten part of the line already consists of default blanks.
                end = end.min(line.len());
            }
            if columns.start >= end {
                continue;
            }
            let wrapped = line.last_cell_was_wrapped();
            for column in columns.start..end {
                // Keep independent Kitty placements as in other local text overwrites.
                line.set_cell(column, cell.clone(), seqno);
            }
            if wrapped {
                line.set_last_cell_was_wrapped(true, seqno);
            }
            if is_default_blank {
                line.prune_trailing_blanks(seqno);
            }
        }
    }
}
