use super::Cell;
use crate::interaction::Point;
use std::collections::VecDeque;
use wezterm_term::Terminal;

/// Stream KMP across soft wraps. The coordinate window covers at most the
/// needle's bytes, even for a logical line spanning the entire scrollback.
/// Return true if the caller stopped the scan (hit limit or cancellation).
pub(super) fn scan(
    terminal: &Terminal,
    query: &str,
    sensitive: bool,
    cancelled: impl Fn() -> bool,
    mut on_match: impl FnMut(&VecDeque<Cell>) -> bool,
) -> bool {
    let folded;
    let query = if sensitive {
        query.as_bytes()
    } else {
        folded = query
            .chars()
            .flat_map(char::to_lowercase)
            .collect::<String>();
        folded.as_bytes()
    };
    debug_assert!(!query.is_empty());
    let mut prefix = vec![0; query.len()];
    for i in 1..query.len() {
        let mut j = prefix[i - 1];
        while j > 0 && query[i] != query[j] {
            j = prefix[j - 1];
        }
        prefix[i] = j + usize::from(query[i] == query[j]);
    }

    let screen = terminal.screen();
    let mut cells = VecDeque::new();
    let mut position = 0usize;
    let mut matched = 0;
    for (row, line) in screen.physical_lines().enumerate() {
        if cancelled() {
            return true;
        }
        let row = screen.phys_to_stable_row_index(row);
        for (i, cell) in line.visible_cells().enumerate() {
            if i % 256 == 255 && cancelled() {
                return true;
            }
            let text = cell.str();
            if text.is_empty() {
                continue;
            }
            cells.push_back(Cell {
                start: position,
                // A case-folded grapheme can expand. Retain it until we've
                // processed all its bytes, then record its actual end.
                end: usize::MAX,
                point: Point {
                    row,
                    col: cell.cell_index(),
                },
                width: cell.width(),
            });
            let mut consume = |byte| {
                position += 1;
                while matched > 0 && byte != query[matched] {
                    matched = prefix[matched - 1];
                }
                matched += usize::from(byte == query[matched]);
                let start = position.saturating_sub(query.len());
                while cells.front().is_some_and(|c| c.end <= start) {
                    cells.pop_front();
                }
                if matched == query.len() {
                    matched = 0;
                    return on_match(&cells);
                }
                true
            };
            if sensitive || text.is_ascii() {
                for chunk in text.as_bytes().chunks(1024) {
                    if text.len() > 1024 && cancelled() {
                        return true;
                    }
                    if !chunk
                        .iter()
                        .all(|&b| consume(if sensitive { b } else { b.to_ascii_lowercase() }))
                    {
                        return true;
                    }
                }
            } else {
                for (i, c) in text.chars().flat_map(char::to_lowercase).enumerate() {
                    if i % 256 == 255 && cancelled() {
                        return true;
                    }
                    if !c.encode_utf8(&mut [0; 4]).bytes().all(&mut consume) {
                        return true;
                    }
                }
            }
            cells.back_mut().unwrap().end = position;
        }
        if !line.last_cell_was_wrapped() {
            matched = 0;
            position = 0;
            cells.clear();
        }
    }
    false
}
