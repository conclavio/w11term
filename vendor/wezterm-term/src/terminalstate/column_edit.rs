use super::*;
use wezterm_surface::line::Line;

#[derive(Clone, Copy)]
pub(super) enum Shift {
    Left,
    Right,
}

impl TerminalState {
    pub(super) fn edit_columns(&mut self, count: u32, shift: Shift) {
        let rows = self.top_and_bottom_margins.clone();
        let x = self.cursor.x;
        if !rows.contains(&self.cursor.y) || !self.left_and_right_margins.contains(&x) {
            return;
        }
        let right = self.left_and_right_margins.end;
        // A cut through a wide glyph can change the adjacent boundary cell.
        let graphics_cols = x.saturating_sub(1)..(right + 1).min(self.screen().physical_cols);
        let graphics = self.prepare_kitty_text_edit(&rows, &graphics_cols, false);
        let seqno = self.seqno;
        let screen = self.screen_mut();
        for row in rows {
            let end = right.min(screen.logical_cols(row));
            if x < end {
                let physical = screen.phys_row(row);
                shift_line(
                    screen.line_mut(physical),
                    x..end,
                    count as usize,
                    shift,
                    seqno,
                );
            }
        }
        self.finish_kitty_tile_update(graphics);
    }
}

fn shift_line(
    line: &mut Line,
    columns: Range<usize>,
    count: usize,
    shift: Shift,
    seqno: SequenceNo,
) {
    let start = columns.start;
    let end = columns.end;
    if count == 0 || start >= line.len() {
        return;
    }
    let count = count.min(columns.len());
    let discarded = match shift {
        Shift::Left => start + count,
        Shift::Right => end - count,
    };
    let cuts = [start, discarded, end];
    let split_glyphs: Vec<_> = line
        .visible_cells()
        .take_while(|cell| cell.cell_index() < end)
        .filter_map(|cell| {
            let glyph = cell.cell_index()..cell.cell_index() + cell.width();
            cuts.iter()
                .any(|&cut| glyph.start < cut && cut < glyph.end)
                .then_some(glyph)
        })
        .collect();
    let wrapped = line.last_cell_was_wrapped();
    if wrapped {
        line.set_last_cell_was_wrapped(false, seqno);
    }
    line.invalidate_implicit_hyperlinks(seqno);
    // Expand implicit blanks and invalidate cached semantic zones before the
    // positional edit. Preserve all cells beyond the edited margin.
    line.resize(line.len().max(end), seqno);
    let cells = line.cells_mut();
    for glyph in split_glyphs {
        let last = glyph.end.min(cells.len());
        for cell in &mut cells[glyph.start..last] {
            let mut attrs = cell.attrs().clone();
            attrs.set_hyperlink(None);
            *cell = Cell::blank_with_attrs(attrs);
        }
    }
    let cells = &mut cells[columns];
    match shift {
        Shift::Left => {
            cells.rotate_left(count);
            cells[end - start - count..].fill_with(Cell::blank);
        }
        Shift::Right => {
            cells.rotate_right(count);
            cells[..count].fill_with(Cell::blank);
        }
    }
    if wrapped {
        line.set_last_cell_was_wrapped(true, seqno);
    }
    line.prune_trailing_blanks(seqno);
}
