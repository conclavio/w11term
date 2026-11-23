use super::*;
use wezterm_cell::SemanticType;
use wezterm_surface::line::Line;

fn rectangle_axis(start: OneBased, end: OneBased, limits: Range<usize>) -> Range<usize> {
    if limits.is_empty() || start.as_one_based() > end.as_one_based() {
        return limits.start..limits.start;
    }
    let first = (start.as_zero_based() as usize).min(limits.len() - 1);
    let last = (end.as_one_based() as usize).min(limits.len());
    limits.start + first..limits.start + last
}

impl TerminalState {
    fn rectangle_limits(&self) -> (Range<usize>, Range<usize>) {
        let screen = self.screen();
        if self.dec_origin_mode {
            (
                self.top_and_bottom_margins.start as usize
                    ..(self.top_and_bottom_margins.end as usize).min(screen.physical_rows),
                self.left_and_right_margins.start
                    ..self.left_and_right_margins.end.min(screen.physical_cols),
            )
        } else {
            (0..screen.physical_rows, 0..screen.physical_cols)
        }
    }

    pub(super) fn screen_rectangle(
        &self,
        top: OneBased,
        left: OneBased,
        bottom: OneBased,
        right: OneBased,
    ) -> (Range<usize>, Range<usize>) {
        let (row_limits, col_limits) = self.rectangle_limits();
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

    pub(super) fn copy_rectangle(
        &mut self,
        [top, left, bottom, right]: [OneBased; 4],
        [destination_top, destination_left]: [OneBased; 2],
    ) {
        let (row_limits, col_limits) = self.rectangle_limits();
        let row = destination_top.as_zero_based() as usize;
        let col = destination_left.as_zero_based() as usize;
        if row >= row_limits.len() || col >= col_limits.len() {
            return;
        }
        let target_row = row_limits.start + row;
        let target_col = col_limits.start + col;
        let rows = rectangle_axis(top, bottom, row_limits.clone());
        let columns = rectangle_axis(left, right, col_limits.clone());
        let height = rows.len().min(row_limits.end - target_row);
        let width = columns.len().min(col_limits.end - target_col);
        if height == 0 || width == 0 || (rows.start, columns.start) == (target_row, target_col) {
            return;
        }

        let dest_rows = target_row as VisibleRowIndex..(target_row + height) as VisibleRowIndex;
        // Replacing half of a wide glyph can touch the adjoining column.
        let dest_cols =
            target_col.saturating_sub(1)..(target_col + width + 1).min(self.screen().physical_cols);
        let graphics = self.prepare_kitty_text_edit(&dest_rows, &dest_cols, false);
        let seqno = self.seqno;
        let screen = self.screen_mut();
        let mut buffer = Vec::with_capacity(width);
        // Copy each source row before writing. Bottom-up traversal prevents a
        // downward overlap from overwriting source rows we have yet to read.
        for index in 0..height {
            let offset = if target_row > rows.start {
                height - 1 - index
            } else {
                index
            };
            let from = (rows.start + offset) as VisibleRowIndex;
            let to = (target_row + offset) as VisibleRowIndex;
            let count = width.min(screen.logical_cols(to).saturating_sub(target_col));
            if count == 0 {
                continue;
            }
            let from_width = screen.logical_cols(from);
            let source = screen.phys_row(from);
            read_rectangle_row(
                screen.line_mut(source),
                columns.start..columns.start + count,
                from_width,
                &mut buffer,
            );
            let dest = screen.phys_row(to);
            let line = screen.line_mut(dest);
            let wrapped = line.last_cell_was_wrapped();
            let mut column = target_col;
            for cell in buffer.drain(..) {
                let width = cell.width().max(1);
                line.set_cell(column, cell, seqno);
                column += width;
            }
            if wrapped {
                line.set_last_cell_was_wrapped(true, seqno);
            }
            line.prune_trailing_blanks(seqno);
        }
        self.finish_kitty_tile_update(graphics);
    }
}

fn read_rectangle_row(
    line: &Line,
    columns: Range<usize>,
    row_width: usize,
    buffer: &mut Vec<Cell>,
) {
    let end = columns.end.min(row_width);
    if end <= columns.start {
        buffer.resize(columns.len(), Cell::blank());
        return;
    }
    let mut column = columns.start;
    for cell in line
        .visible_cells()
        .take_while(|cell| cell.cell_index() < end)
    {
        let start = cell.cell_index();
        let next = start + cell.width().max(1);
        if next <= columns.start {
            continue;
        }
        let first = start.max(columns.start);
        buffer.extend((column..first).map(|_| Cell::blank()));
        let mut attrs = cell.attrs().clone();
        attrs.clear_images().set_wrapped(false);
        if attrs.hyperlink().is_some_and(|link| link.is_implicit()) {
            attrs.set_hyperlink(None);
        }
        if start >= columns.start && next <= end {
            buffer.push(Cell::new_grapheme_with_width(
                cell.str(),
                cell.width().max(1),
                attrs,
            ));
        } else {
            // A left or right cut through a grapheme copies styled blanks.
            attrs.set_hyperlink(None);
            buffer.extend((first..next.min(end)).map(|_| Cell::blank_with_attrs(attrs.clone())));
        }
        column = next.min(end);
    }
    buffer.extend((column..columns.end).map(|_| Cell::blank()));
}
