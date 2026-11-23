use super::*;

impl TerminalState {
    pub(super) fn selectively_erase_display(&mut self, erase: EraseInDisplay) {
        let current = self.cursor.y;
        let rows = self.screen().physical_rows as VisibleRowIndex;
        let width = self.screen().physical_cols;
        let others = match erase {
            EraseInDisplay::EraseToEndOfDisplay => {
                self.selectively_erase_line(
                    current,
                    self.erase_line_columns(EraseInLine::EraseToEndOfLine),
                );
                current + 1..rows
            }
            EraseInDisplay::EraseToStartOfDisplay => {
                self.selectively_erase_line(
                    current,
                    self.erase_line_columns(EraseInLine::EraseToStartOfLine),
                );
                0..current
            }
            EraseInDisplay::EraseDisplay => 0..rows,
            EraseInDisplay::EraseScrollback => {
                self.screen_mut().erase_scrollback();
                return;
            }
        };
        for row in others {
            self.selectively_erase_line(row, 0..width);
        }
    }

    pub(super) fn selectively_erase_line(&mut self, row: VisibleRowIndex, columns: Range<usize>) {
        let seqno = self.seqno;
        let screen = self.screen_mut();
        let end = columns.end.min(screen.logical_cols(row));
        if columns.start >= end {
            return;
        }
        let physical = screen.phys_row(row);
        let line = screen.line_mut(physical);

        // In a modern terminal the DEC character can occupy several columns.
        // Replace an intersecting grapheme in full or keep it in full.
        let changes: Vec<_> = line
            .visible_cells()
            .take_while(|cell| cell.cell_index() < end)
            .filter_map(|cell| {
                let start = cell.cell_index();
                let end = (start + cell.width()).min(line.len());
                (end > columns.start && !cell.attrs().protected() && cell.str() != " ")
                    .then_some(start..end)
            })
            .collect();
        for index in changes.into_iter().flatten() {
            let mut attrs = line.cells_mut_for_attr_changes_only()[index]
                .attrs()
                .clone();
            attrs.set_hyperlink(None);
            line.set_cell_clearing_image_placements(index, Cell::blank_with_attrs(attrs), seqno);
        }
    }
}
