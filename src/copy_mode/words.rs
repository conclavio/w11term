use super::ScreenId;
use crate::interaction::{Point, WordClass, word_class};
use wezterm_term::{Line, TerminalState};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WordMotion {
    NextStart,
    PreviousStart,
    NextEnd,
}

struct Scan {
    motion: WordMotion,
    previous: WordClass,
    // When looking for the far edge of a run, keep the last grapheme until
    // its neighbor tells us whether the run continues (possibly on another row).
    edge: Option<Point>,
}

impl Scan {
    fn hard_break(&mut self) -> Option<Point> {
        self.previous = WordClass::Whitespace;
        self.edge
    }

    fn visit(&mut self, point: Point, class: WordClass) -> Option<Point> {
        if class != self.previous {
            self.previous = class;
            if let Some(edge) = self.edge {
                return Some(edge);
            }
            if self.motion == WordMotion::NextStart && class != WordClass::Whitespace {
                return Some(point);
            }
        }
        if self.motion != WordMotion::NextStart && class != WordClass::Whitespace {
            self.edge = Some(point);
        }
        None
    }
}

/// Walk retained graphemes in logical copy order, keeping at most one physical
/// row of column indices and word classes, even for an arbitrarily long wrap.
pub fn word_target(
    terminal: &TerminalState,
    generation: u64,
    id: ScreenId,
    cursor: Point,
    motion: WordMotion,
    delimiters: &str,
) -> Option<Point> {
    let screen = terminal.screen();
    if !id.same_coordinates(terminal, generation)
        || id.history_clear_count != screen.history_clear_count()
        || cursor.col >= id.cols
    {
        return None;
    }
    let start = screen.stable_row_to_phys(cursor.row)?;
    let backwards = motion == WordMotion::PreviousStart;
    let mut scan = Scan {
        motion,
        previous: WordClass::Whitespace,
        edge: None,
    };
    let mut origin = cursor;
    let mut visit_row = |(physical, line): (usize, &Line)| {
        let row = screen.phys_to_stable_row_index(physical);
        // A hard break separates runs even when both rows contain the same class.
        // Walking backward crosses this row's right edge before its cells.
        if backwards
            && physical != start
            && !line.last_cell_was_wrapped()
            && let Some(edge) = scan.hard_break()
        {
            return Some(edge);
        }
        let cells = line
            .visible_cells()
            .take_while(|c| c.cell_index() < id.cols);
        if backwards {
            // The terminal exposes a forward grapheme iterator. Only b needs
            // a row buffer. Skip everything at or to the right of the origin.
            let mut prefix = Vec::new();
            for cell in cells {
                let col = cell.cell_index();
                if physical == start && col + cell.width() > cursor.col {
                    if col <= cursor.col {
                        origin.col = col;
                    }
                    break;
                }
                prefix.push(cell);
            }
            for cell in prefix.into_iter().rev() {
                let point = Point {
                    row,
                    col: cell.cell_index(),
                };
                if let Some(target) = scan.visit(point, word_class(cell.str(), delimiters)) {
                    return Some(target);
                }
            }
            return None;
        }
        for cell in cells {
            let col = cell.cell_index();
            if physical == start && col <= cursor.col {
                if cursor.col < col + cell.width() {
                    origin.col = col;
                    scan.previous = word_class(cell.str(), delimiters);
                }
                continue;
            }
            if let Some(target) = scan.visit(Point { row, col }, word_class(cell.str(), delimiters))
            {
                return Some(target);
            }
        }
        if !line.last_cell_was_wrapped() {
            return scan.hard_break();
        }
        None
    };
    let rows = screen.physical_lines().enumerate();
    let target = if backwards {
        rows.take(start + 1).rev().find_map(&mut visit_row)
    } else {
        rows.skip(start).find_map(&mut visit_row)
    };
    Some(target.or(scan.edge).unwrap_or(origin))
}
