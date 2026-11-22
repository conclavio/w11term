use super::{Snapshot, Terminal};
use crate::session::Command;

/// The primary viewport in stable row coordinates, including the newest possible top row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollbackView {
    pub oldest: isize,
    pub top: isize,
    pub live: isize,
    pub rows: usize,
    cols: usize,
    epoch: u64,
    connection: u64,
    clear: u64,
}

impl Snapshot {
    pub fn scrollback_view(&self) -> Option<ScrollbackView> {
        let live = self
            .first_stable_row
            .saturating_add_unsigned(self.scroll_offset);
        (!self.alternate && !self.rows.is_empty() && self.oldest_stable_row < live).then_some(
            ScrollbackView {
                oldest: self.oldest_stable_row,
                top: self.first_stable_row,
                live,
                rows: self.rows.len(),
                cols: self.cols,
                epoch: self.row_epoch,
                connection: self.connection_generation,
                clear: self.history_clear_count,
            },
        )
    }
}

impl ScrollbackView {
    pub fn same_screen(&self, other: &Self) -> bool {
        self.epoch == other.epoch
            && self.connection == other.connection
            && self.clear == other.clear
            && self.rows == other.rows
            && self.cols == other.cols
    }

    pub(crate) fn max_offset(&self, term: &Terminal, connection: u64) -> Option<usize> {
        let screen = term.screen();
        if term.is_alt_screen_active()
            || term.synchronized_output()
            || self.connection != connection
            || self.clear != screen.history_clear_count()
            || self.epoch != screen.row_epoch()
            || self.rows != screen.physical_rows
            || self.cols != screen.physical_cols
        {
            return None;
        }
        let max = crate::search::max_offset(term);
        (max != 0).then_some(max)
    }

    /// None follows the newest output, even if output arrived after the gesture began.
    pub fn target(&self, top: Option<isize>) -> ScrollbackTarget {
        ScrollbackTarget { view: *self, top }
    }

    pub fn scroll_to(&self, top: Option<isize>) -> Command {
        Command::ScrollViewport(self.target(top))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ScrollbackTarget {
    view: ScrollbackView,
    top: Option<isize>,
}

impl ScrollbackTarget {
    pub(crate) fn offset(&self, term: &Terminal, connection: u64) -> Option<usize> {
        let max = self.view.max_offset(term, connection)?;
        let live = term.screen().visible_row_to_stable_row(0);
        Some(
            self.top
                .map_or(0, |top| live.saturating_sub(top).max(0) as usize)
                .min(max),
        )
    }
}

#[cfg(test)]
#[path = "scrollback_view_tests.rs"]
mod tests;
