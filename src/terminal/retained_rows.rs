use super::{Arc, Line, Snapshot};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use wezterm_term::Terminal;

// Match the native accessible document’s text budget. Oversized rows still
// support exact revision checks without hashing arbitrarily large cell text.
const TEXT_BUDGET: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
struct RowRevision {
    seqno: usize,
    wrapped: bool,
    text_geometry: Option<u64>,
}

fn text_geometry(line: &Line, cols: usize, budget: &mut usize) -> Option<u64> {
    let mut hash = DefaultHasher::new();
    let (enabled, hint) = line.bidi_info();
    enabled.hash(&mut hash);
    std::mem::discriminant(&hint).hash(&mut hash);
    for cell in line.visible_cells().take_while(|c| c.cell_index() < cols) {
        *budget = budget.checked_sub(cell.str().len())?;
        cell.cell_index().hash(&mut hash);
        cell.width().hash(&mut hash);
        cell.str().hash(&mut hash);
    }
    Some(hash.finish())
}

impl RowRevision {
    fn matches(&self, row: &Line, wrapped: bool, cols: usize, budget: &mut usize) -> bool {
        // Cursor motion and palette changes can dirty a row without changing
        // the text or its screen geometry.
        self.wrapped == wrapped
            && (self.seqno == row.current_seqno()
                || self
                    .text_geometry
                    .is_some_and(|old| text_geometry(row, cols, budget) == Some(old)))
    }
}

/// Stable coordinates and revisions of rows from an immutable screen snapshot.
/// Slices share the revision array without retaining cells or image data.
#[derive(Clone, Debug)]
pub struct RetainedRows {
    epoch: u64,
    connection: u64,
    first: isize,
    revisions: Arc<[RowRevision]>,
    indices: Range<usize>,
}

impl Snapshot {
    pub fn retained_rows(&self, first: isize, last: isize) -> Option<RetainedRows> {
        let start = usize::try_from(first.checked_sub(self.first_stable_row)?).ok()?;
        let end = usize::try_from(last.checked_sub(self.first_stable_row)?).ok()?;
        let rows = self.rows.get(start..=end)?;
        if rows.is_empty() {
            return None;
        }
        let mut budget = TEXT_BUDGET;
        Some(RetainedRows {
            epoch: self.row_epoch,
            connection: self.connection_generation,
            first,
            revisions: rows
                .iter()
                .zip(&self.row_wraps[start..=end])
                .map(|(row, &wrapped)| RowRevision {
                    seqno: row.current_seqno(),
                    wrapped,
                    text_geometry: text_geometry(row, self.cols, &mut budget),
                })
                .collect(),
            indices: 0..rows.len(),
        })
    }
}

impl RetainedRows {
    pub fn slice(&self, first: isize, last: isize) -> Option<Self> {
        let skip = usize::try_from(first.checked_sub(self.first)?).ok()?;
        let len = usize::try_from(last.checked_sub(first)?)
            .ok()?
            .checked_add(1)?;
        let start = self.indices.start.checked_add(skip)?;
        let end = start.checked_add(len)?;
        (end <= self.indices.end).then(|| Self {
            first,
            indices: start..end,
            ..self.clone()
        })
    }

    fn last(&self) -> isize {
        self.first + (self.indices.len() - 1) as isize
    }

    /// Bounding rectangles can report only the part of the range in the view.
    pub fn matches_visible(&self, frame: &Snapshot) -> bool {
        if self.epoch != frame.row_epoch
            || self.connection != frame.connection_generation
            || self.first < frame.oldest_stable_row
        {
            return false;
        }
        let first = self.first.max(frame.first_stable_row);
        let end = (self.last() + 1).min(frame.first_stable_row + frame.rows.len() as isize);
        let mut budget = TEXT_BUDGET;
        first < end
            && (first..end).all(|row| {
                let index = (row - frame.first_stable_row) as usize;
                self.revisions[self.indices.start + (row - self.first) as usize].matches(
                    &frame.rows[index],
                    frame.row_is_wrapped(index),
                    frame.cols,
                    &mut budget,
                )
            })
    }

    pub(crate) fn scroll_offset(
        &self,
        term: &Terminal,
        connection: u64,
        align_top: bool,
    ) -> Option<usize> {
        let screen = term.screen();
        if term.synchronized_output()
            || self.connection != connection
            || self.epoch != screen.row_epoch()
        {
            return None;
        }
        let first = screen.stable_row_to_phys(self.first)?;
        let last = screen.stable_row_to_phys(self.last())?;
        let mut budget = TEXT_BUDGET;
        if !screen
            .physical_lines()
            .skip(first)
            .take(last - first + 1)
            .zip(&self.revisions[self.indices.clone()])
            .all(|(row, old)| {
                old.matches(
                    row,
                    row.last_cell_was_wrapped(),
                    screen.physical_cols,
                    &mut budget,
                )
            })
        {
            return None;
        }
        let top = if align_top {
            first
        } else {
            (last + 1).saturating_sub(screen.physical_rows)
        };
        Some(screen.phys_row(0).saturating_sub(top))
    }
}
