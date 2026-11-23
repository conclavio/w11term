use super::{Snapshot, Terminal};
use crate::session::Command;
use std::sync::Arc;

pub(super) fn failed_command_rows(
    terminal: &wezterm_term::TerminalState,
    previous: Option<&Snapshot>,
) -> Arc<[isize]> {
    let rows = || {
        let history = terminal.command_history();
        terminal
            .screen()
            .command_output_rows()
            .filter_map(move |(id, row)| {
                let entry = &history[history.binary_search_by_key(&id, |entry| entry.id).ok()?];
                entry.exit_code.is_some_and(|code| code != 0).then_some(row)
            })
    };
    if let Some(old) = previous.filter(|old| rows().eq(old.failed_command_rows.iter().copied())) {
        old.failed_command_rows.clone()
    } else {
        rows().collect()
    }
}

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
    pub fn adjacent_failed_command(&self, older: bool) -> Option<(ScrollbackView, isize)> {
        let view = self.scrollback_view()?;
        let rows = self.failed_command_rows.iter().copied();
        let target = if older {
            rows.filter(|&row| row < view.top).max()?
        } else {
            // Rows within the live viewport share the same scroll position.
            (view.top < view.live).then_some(())?;
            rows.filter(|&row| row > view.top).min()?
        };
        Some((view, target))
    }

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
