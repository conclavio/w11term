use super::{Snapshot, Terminal};
use crate::session::Command;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandContext {
    pub start: isize,
    pub label: Arc<str>,
}

pub(super) fn command_context(
    terminal: &wezterm_term::TerminalState,
    top: isize,
    offset: usize,
    previous: Option<&Snapshot>,
) -> Option<CommandContext> {
    if offset == 0 {
        return None;
    }
    let (start, command) = terminal.command_context(top)?;
    let label = crate::events::display_field(command);
    if label.is_empty() {
        return None;
    }
    if let Some(old) = previous.and_then(|frame| frame.command_context.as_ref())
        && old.start == start
        && old.label.as_ref() == label
    {
        return Some(old.clone());
    }
    Some(CommandContext {
        start,
        label: label.into(),
    })
}

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

pub(super) fn bookmark_rows(
    screen: &wezterm_term::Screen,
    previous: Option<&Snapshot>,
) -> Arc<[isize]> {
    let rows = || screen.scrollback_bookmark_rows();
    if let Some(old) = previous.filter(|old| rows().eq(old.scrollback_bookmarks.iter().copied())) {
        old.scrollback_bookmarks.clone()
    } else {
        rows().collect()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BookmarkTarget {
    row: isize,
    col: usize,
    epoch: u64,
    clear: u64,
    connection: u64,
}

impl BookmarkTarget {
    pub(crate) fn toggle(self, term: &mut Terminal, connection: u64) -> bool {
        if !term.is_alt_screen_active()
            && !term.synchronized_output()
            && self.connection == connection
            && self.epoch == term.screen().row_epoch()
            && self.clear == term.screen().history_clear_count()
        {
            return term
                .screen_mut()
                .toggle_scrollback_bookmark(self.row, self.col);
        }
        false
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
    pub fn bookmark_target(&self) -> Option<BookmarkTarget> {
        (!self.alternate && !self.rows.is_empty()).then(|| {
            let (row, col) = if self.scroll_offset == 0 {
                (
                    self.first_stable_row.saturating_add(self.cursor.y as isize),
                    self.cursor.x,
                )
            } else {
                (self.first_stable_row, 0)
            };
            BookmarkTarget {
                row,
                col,
                epoch: self.row_epoch,
                clear: self.history_clear_count,
                connection: self.connection_generation,
            }
        })
    }

    pub fn adjacent_bookmark(&self, older: bool) -> Option<ScrollbackTarget> {
        let view = self.scrollback_view()?;
        let rows = &self.scrollback_bookmarks;
        let row = if older {
            let end = rows.partition_point(|&row| row < view.top);
            *rows.get(end.checked_sub(1)?)?
        } else {
            (view.top < view.live).then_some(())?;
            *rows.get(rows.partition_point(|&row| row <= view.top))?
        };
        Some(ScrollbackTarget {
            view,
            top: Some(row),
            require_retained: true,
        })
    }

    pub fn command_context_target(&self) -> Option<ScrollbackTarget> {
        let start = self.command_context.as_ref()?.start;
        let view = self.scrollback_view()?;
        Some(ScrollbackTarget {
            view,
            top: Some(start),
            require_retained: true,
        })
    }

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
        ScrollbackTarget {
            view: *self,
            top,
            require_retained: false,
        }
    }

    pub fn scroll_to(&self, top: Option<isize>) -> Command {
        Command::ScrollViewport(self.target(top))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ScrollbackTarget {
    view: ScrollbackView,
    top: Option<isize>,
    require_retained: bool,
}

impl ScrollbackTarget {
    pub(crate) fn offset(&self, term: &Terminal, connection: u64) -> Option<usize> {
        let max = self.view.max_offset(term, connection)?;
        if self.require_retained
            && self
                .top
                .is_some_and(|top| term.screen().stable_row_to_phys(top).is_none())
        {
            return None;
        }
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
