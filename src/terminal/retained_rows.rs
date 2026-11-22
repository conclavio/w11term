use super::{Arc, Line, Snapshot};
use std::ops::Range;
use wezterm_term::Terminal;

/// Stable coordinates and revisions of rows from an immutable screen snapshot.
/// Slices share the revision array without retaining cells or image data.
#[derive(Clone, Debug)]
pub struct RetainedRows {
    epoch: u64,
    connection: u64,
    first: isize,
    revisions: Arc<[usize]>,
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
        Some(RetainedRows {
            epoch: self.row_epoch,
            connection: self.connection_generation,
            first,
            revisions: rows.iter().map(|row| row.current_seqno()).collect(),
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
        first < end
            && (first..end).all(|row| {
                self.revisions[self.indices.start + (row - self.first) as usize]
                    == frame.rows[(row - frame.first_stable_row) as usize].current_seqno()
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
        if !screen
            .physical_lines()
            .skip(first)
            .take(last - first + 1)
            .map(Line::current_seqno)
            .eq(self.revisions[self.indices.clone()].iter().copied())
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
