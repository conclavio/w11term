use super::{CommandPosition, Screen, VisibleRowIndex};

const BATCH_ROWS: usize = 128;
const LINE_BYTES: usize = 1024;

/// A borrowed range on the primary screen, offered only during command output.
pub struct LiveCommandOutput<'a> {
    screen: &'a Screen,
    start: CommandPosition,
    end: (usize, usize),
    finishing: bool,
}

#[derive(Debug, Default)]
pub struct LiveOutputCursor(Option<(u64, CommandPosition)>);

#[derive(Default)]
pub struct LiveOutputBatch {
    pub gap: bool,
    pub lines: Vec<LiveOutputLine>,
}

pub struct LiveOutputLine {
    pub text: String,
    pub shortened: bool,
}

impl Screen {
    pub(crate) fn live_command_output(
        &self,
        row: VisibleRowIndex,
        col: usize,
        finishing: bool,
    ) -> Option<LiveCommandOutput<'_>> {
        Some(LiveCommandOutput {
            screen: self,
            start: self.command_start?,
            end: (self.phys_row(row), col),
            finishing,
        })
    }
}

impl LiveCommandOutput<'_> {
    pub fn is_finishing(&self) -> bool {
        self.finishing
    }

    /// Offer completed logical lines once. The current line can still be
    /// rewritten by the shell, so offer it only when the command finishes.
    pub fn read(&self, cursor: &mut LiveOutputCursor) -> LiveOutputBatch {
        self.read_window(cursor, BATCH_ROWS)
    }

    /// Inspect recent screen rows, including the current unfinished line.
    pub(crate) fn recent(mut self) -> LiveOutputBatch {
        self.finishing = true;
        self.read_window(&mut LiveOutputCursor::default(), 6)
    }

    fn read_window(&self, cursor: &mut LiveOutputCursor, row_limit: usize) -> LiveOutputBatch {
        let mut batch = LiveOutputBatch::default();
        let screen = self.screen;
        let (end_row, end_col) = self.end;
        let revision = screen.command_output_revision;
        if end_row >= screen.lines.len() {
            return batch;
        }
        let (row, col) = match cursor.0 {
            Some((old_revision, position)) if old_revision == revision => position,
            Some(_) => {
                // Reflow changes the meaning of stable row numbers. Resume at
                // the current row and flag the boundary in the combined output.
                cursor.0 = Some((
                    revision,
                    (screen.phys_to_stable_row_index(end_row), end_col),
                ));
                batch.gap = true;
                return batch;
            }
            None => self.start,
        };
        if (row, col) > (screen.phys_to_stable_row_index(end_row), end_col) {
            return batch;
        }
        let floor = end_row.saturating_sub(row_limit - 1);
        let (first, col) = match screen.stable_row_to_phys(row) {
            Some(first) if first >= floor => (first, col),
            _ => {
                batch.gap = true;
                cursor.0 = Some((revision, (screen.phys_to_stable_row_index(floor), 0)));
                (floor, 0)
            }
        };
        let mut text = String::new();
        let mut shortened = false;
        for index in first..=end_row {
            if index == end_row && !self.finishing {
                break;
            }
            let line = &screen.lines[index];
            let from = if index == first { col } else { 0 };
            let to = if index == end_row {
                end_col
            } else {
                screen.physical_cols
            };
            if !shortened {
                for cell in line
                    .visible_cells()
                    .skip_while(|cell| cell.cell_index() < from)
                    .take_while(|cell| cell.cell_index() < to)
                {
                    if text.len() + cell.str().len() > LINE_BYTES {
                        shortened = true;
                        break;
                    }
                    text.push_str(cell.str());
                }
            }
            let hard_break = index < end_row && !line.last_cell_was_wrapped();
            if hard_break || (index == end_row && (!text.is_empty() || shortened)) {
                text.truncate(text.trim_end_matches(' ').len());
                batch.lines.push(LiveOutputLine {
                    text: std::mem::take(&mut text),
                    shortened: std::mem::take(&mut shortened),
                });
                cursor.0 = Some((revision, (screen.phys_to_stable_row_index(index + 1), 0)));
            }
        }
        batch
    }
}
