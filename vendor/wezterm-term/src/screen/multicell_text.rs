use super::*;
use std::collections::BTreeMap;
use std::convert::TryFrom;
use wezterm_cell::MulticellText;

type Placements = BTreeMap<(isize, usize), Arc<MulticellText>>;

impl Screen {
    /// Install one logical text run as a series of wide row cells. Only the top
    /// cell contains searchable/copyable text. Every row can render its slice.
    pub(crate) fn set_multicell_text(
        &mut self,
        x: usize,
        y: VisibleRowIndex,
        text: Arc<MulticellText>,
        attr: &CellAttributes,
        seqno: SequenceNo,
    ) -> bool {
        let rows = y..y + VisibleRowIndex::from(text.scale());
        let cols = x..x + text.columns();
        if rows.start < 0
            || rows.end > self.physical_rows as VisibleRowIndex
            || rows
                .clone()
                .any(|row| self.line_width_scale(row) != 1 || cols.end > self.logical_cols(row))
        {
            return false;
        }
        self.erase_multicell_text(rows.clone(), cols, seqno);
        for (offset, row) in self.phys_range(&rows).enumerate() {
            let mut attrs = attr.clone();
            attrs
                .set_explicit_width(true)
                .set_multicell_row(Some(text.row(offset as u8)));
            let content = if offset == 0 { text.text() } else { " " };
            self.line_mut(row)
                .set_cell_grapheme(x, content, text.columns(), attrs, seqno);
        }
        true
    }

    pub(crate) fn append_multicell_text(
        &mut self,
        y: VisibleRowIndex,
        col: usize,
        suffix: &str,
        seqno: SequenceNo,
    ) -> bool {
        let row = self.phys_row(y);
        let blocks = self.multicell_text_in(row..row + 1, &(col..col + 1));
        let Some(((top, left), block)) = blocks.into_iter().next() else {
            return false;
        };
        if block.text().len() + suffix.len() > 8192 {
            return true;
        }
        let text = format!("{}{suffix}", block.text());
        let Some(updated) = MulticellText::new(
            &text,
            block.columns() / usize::from(block.scale()),
            block.scale(),
        ) else {
            return true;
        };
        for offset in 0..block.scale() {
            let Ok(physical) = usize::try_from(top + isize::from(offset)) else {
                continue;
            };
            let Some(line) = self.lines.get_mut(physical) else {
                break;
            };
            let Some(cell) = line.get_cell(left) else {
                break;
            };
            if !cell
                .attrs()
                .multicell_row()
                .is_some_and(|slice| slice.offset() == offset && slice.run() == &block)
            {
                break;
            }
            let mut attrs = cell.attrs().clone();
            attrs.set_multicell_row(Some(updated.row(offset)));
            let content = if offset == 0 { updated.text() } else { " " };
            line.set_cell_grapheme(left, content, updated.columns(), attrs, seqno);
        }
        true
    }

    pub(crate) fn prepare_multicell_column_shift(
        &mut self,
        rows: Range<VisibleRowIndex>,
        edited: Range<usize>,
        moving: Range<usize>,
        seqno: SequenceNo,
    ) {
        let physical = self.phys_range(&rows);
        let mut blocks = self.multicell_text_in(physical.clone(), &edited);
        blocks.retain(|(top, col), block| {
            *top < physical.start as isize
                || *top + isize::from(block.scale()) > physical.end as isize
                || *col < moving.start
                || *col + block.columns() > moving.end
        });
        self.clear_multicell_text(blocks, seqno);
    }

    pub(crate) fn selectively_erase_multicell_text(
        &mut self,
        y: VisibleRowIndex,
        cols: &Range<usize>,
        seqno: SequenceNo,
    ) {
        let row = self.phys_row(y);
        let mut blocks = self.multicell_text_in(row..row + 1, cols);
        blocks.retain(|(_, col), _| {
            self.lines[row]
                .get_cell(*col)
                .is_some_and(|cell| !cell.attrs().protected())
        });
        self.clear_multicell_text(blocks, seqno);
    }

    pub(crate) fn erase_multicell_text(
        &mut self,
        rows: Range<VisibleRowIndex>,
        cols: Range<usize>,
        seqno: SequenceNo,
    ) {
        if rows.is_empty() || cols.is_empty() {
            return;
        }
        let physical = self.phys_range(&rows);
        let blocks = self.multicell_text_in(physical, &cols);
        self.clear_multicell_text(blocks, seqno);
    }

    pub(super) fn erase_multicell_text_in_row(
        &mut self,
        physical: usize,
        cols: Range<usize>,
        seqno: SequenceNo,
    ) {
        if self.lines[physical].has_multicell_text() {
            let blocks = self.multicell_text_in(physical..physical + 1, &cols);
            self.clear_multicell_text(blocks, seqno);
        }
    }

    /// Find the first lower row intersecting a prospective text write. Moving
    /// past its end leaves the text rectangle intact.
    pub(crate) fn multicell_continuation_end(
        &self,
        row: VisibleRowIndex,
        cols: Range<usize>,
    ) -> Option<usize> {
        let line = self.lines.get(self.phys_row(row))?;
        if !line.has_multicell_text() {
            return None;
        }
        line.visible_cells()
            .take_while(|cell| cell.cell_index() < cols.end)
            .find_map(|cell| {
                let end = cell.cell_index() + cell.width();
                (end > cols.start
                    && cell
                        .attrs()
                        .multicell_row()
                        .is_some_and(|row| row.offset() > 0))
                .then_some(end)
            })
    }

    pub(super) fn split_multicell_text_at(&mut self, cut: usize, seqno: SequenceNo) {
        if cut == 0 || cut >= self.lines.len() || !self.lines[cut - 1].has_multicell_text() {
            return;
        }
        let mut blocks = self.multicell_text_in(cut - 1..cut, &(0..usize::MAX));
        blocks.retain(|(top, _), block| *top + isize::from(block.scale()) > cut as isize);
        self.clear_multicell_text(blocks, seqno);
    }

    // A partial-width scroll copies cell attributes between lines. Keep a
    // block only when the entire source rectangle moves with the same delta.
    pub(super) fn prepare_multicell_partial_scroll(
        &mut self,
        rows: Range<usize>,
        cols: &Range<usize>,
        moving: Range<usize>,
        seqno: SequenceNo,
    ) {
        let mut blocks = self.multicell_text_in(rows, cols);
        blocks.retain(|(top, col), block| {
            *top < moving.start as isize
                || *top + isize::from(block.scale()) > moving.end as isize
                || *col < cols.start
                || *col + block.columns() > cols.end
        });
        self.clear_multicell_text(blocks, seqno);
    }

    pub(super) fn clip_multicell_text(&mut self, cols: usize, rows: usize, seqno: SequenceNo) {
        let mut blocks = self.multicell_text_in(0..self.lines.len(), &(0..usize::MAX));
        blocks.retain(|(_, col), block| {
            *col + block.columns() > cols || usize::from(block.scale()) > rows
        });
        self.clear_multicell_text(blocks, seqno);
    }

    fn multicell_text_in(&self, rows: Range<usize>, cols: &Range<usize>) -> Placements {
        let mut blocks = BTreeMap::new();
        for physical in rows {
            let Some(line) = self
                .lines
                .get(physical)
                .filter(|line| line.has_multicell_text())
            else {
                continue;
            };
            for cell in line
                .visible_cells()
                .take_while(|cell| cell.cell_index() < cols.end)
            {
                let col = cell.cell_index();
                if col + cell.width() <= cols.start {
                    continue;
                }
                if let Some(row) = cell.attrs().multicell_row() {
                    let top = physical as isize - isize::from(row.offset());
                    blocks
                        .entry((top, col))
                        .or_insert_with(|| row.run().clone());
                }
            }
        }
        blocks
    }

    fn clear_multicell_text(&mut self, blocks: Placements, seqno: SequenceNo) {
        for ((top, col), block) in blocks {
            for offset in 0..block.scale() {
                let Ok(physical) = usize::try_from(top + isize::from(offset)) else {
                    continue;
                };
                let Some(line) = self.lines.get_mut(physical) else {
                    continue;
                };
                let Some(cell) = line.get_cell(col) else {
                    continue;
                };
                if !cell
                    .attrs()
                    .multicell_row()
                    .is_some_and(|row| row.offset() == offset && row.run() == &block)
                {
                    continue;
                }
                let mut attrs = cell.attrs().clone();
                attrs.clear_text_sizing().set_wrapped(false).clear_images();
                line.fill_range(
                    col..col + block.columns(),
                    &Cell::blank_with_attrs(attrs),
                    seqno,
                );
            }
        }
    }
}
