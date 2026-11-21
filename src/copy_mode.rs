//! Keyboard selection in retained terminal rows. Coordinates stay logical for copying
//! even when the renderer reorders a row for bidirectional display.
use crate::{
    interaction::{Point, Selection},
    terminal::Snapshot,
};
use wezterm_term::{KeyModifiers as Mods, TerminalState};

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum CopyKey {
    Left,
    Down,
    Up,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    First,
    Last,
    Character,
    Line,
    Copy,
    Cancel,
}

impl CopyKey {
    pub fn from_vk(vk: u32, mods: Mods) -> Option<Self> {
        use CopyKey::*;
        if mods.intersects(Mods::ALT | Mods::SUPER) {
            return None;
        }
        if mods.contains(Mods::CTRL) {
            return match vk {
                0x24 => Some(First),
                0x23 => Some(Last),
                0x43 if mods.contains(Mods::SHIFT) => Some(Copy),
                _ => None,
            };
        }
        let shift = mods.contains(Mods::SHIFT);
        Some(match vk {
            0x25 | 0x48 => Left,
            0x28 | 0x4a => Down,
            0x26 | 0x4b => Up,
            0x27 | 0x4c => Right,
            0x24 | 0x30 => Home,
            0x34 if shift => End,
            0x23 => End,
            0x21 => PageUp,
            0x22 => PageDown,
            0x47 if shift => Last,
            0x47 => First,
            0x56 if shift => Line,
            0x56 => Character,
            0x0d | 0x59 => Copy,
            0x1b => Cancel,
            _ => return None,
        })
    }
    pub fn repeats(self) -> bool {
        !matches!(
            self,
            Self::Character | Self::Line | Self::Copy | Self::Cancel
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenId {
    cols: usize,
    height: usize,
    alternate: bool,
    generation: u64,
}

impl From<&Snapshot> for ScreenId {
    fn from(frame: &Snapshot) -> Self {
        Self {
            cols: frame.cols,
            height: frame.rows.len(),
            alternate: frame.alternate,
            generation: frame.connection_generation,
        }
    }
}

pub struct CopyMode {
    pub screen: ScreenId,
    pub cursor: Point,
    /// Keep the viewport pinned as shell output arrives, including when starting at offset zero.
    pub top: isize,
    pub original_top: Option<isize>,
    anchor: Option<(Point, bool)>,
}

impl CopyMode {
    pub fn new(frame: &Snapshot) -> Self {
        let top = frame.first_stable_row;
        let cursor = Point {
            row: if frame.scroll_offset == 0 {
                top + (frame.cursor.y as isize)
                    .clamp(0, frame.rows.len().saturating_sub(1) as isize)
            } else {
                top
            },
            col: if frame.scroll_offset == 0 {
                frame.cursor.x.min(frame.cols.saturating_sub(1))
            } else {
                0
            },
        };
        Self {
            screen: frame.into(),
            cursor,
            top,
            original_top: (frame.scroll_offset != 0).then_some(top),
            anchor: None,
        }
    }

    pub fn valid(&self, frame: &Snapshot) -> bool {
        let live_top = frame.first_stable_row + frame.scroll_offset as isize;
        let retained = frame.oldest_stable_row..live_top + frame.rows.len() as isize;
        self.screen == frame.into()
            && retained.contains(&self.cursor.row)
            && self.anchor.is_none_or(|(p, _)| retained.contains(&p.row))
    }

    pub fn instructions(&self) -> &'static str {
        match self.anchor {
            None => "Copy mode · Arrows or hjkl move · v select · Shift+V select lines · Esc exit",
            Some((_, false)) => "Copy mode · Character selection · y or Enter copy · Esc cancel",
            Some((_, true)) => "Copy mode · Line selection · y or Enter copy · Esc cancel",
        }
    }

    pub fn navigate(&mut self, key: CopyKey, frame: &Snapshot) {
        use CopyKey::*;
        let height = frame.rows.len().max(1) as isize;
        let last_top = frame.first_stable_row + frame.scroll_offset as isize;
        let last_row = last_top + height - 1;
        let last_col = frame.cols.saturating_sub(1);
        let line = usize::try_from(self.cursor.row - frame.first_stable_row)
            .ok()
            .and_then(|i| frame.rows.get(i));
        match key {
            Left | Right => {
                let mut next = if key == Left {
                    self.cursor.col.saturating_sub(1)
                } else {
                    self.cursor.col.saturating_add(1).min(last_col)
                };
                if let Some(cell) = line.and_then(|line| {
                    line.visible_cells()
                        .find(|c| c.cell_index() < next && next < c.cell_index() + c.width())
                }) {
                    next = if key == Right && cell.cell_index() + cell.width() <= last_col {
                        cell.cell_index() + cell.width()
                    } else {
                        cell.cell_index()
                    };
                }
                self.cursor.col = next;
            }
            Up => self.cursor.row -= 1,
            Down => self.cursor.row += 1,
            PageUp => self.cursor.row -= height,
            PageDown => self.cursor.row += height,
            First => self.cursor.row = frame.oldest_stable_row,
            Last => self.cursor.row = last_row,
            Home => self.cursor.col = 0,
            End => self.cursor.col = last_col,
            Character | Line => {
                let lines = key == Line;
                self.anchor = match self.anchor {
                    Some((_, old)) if old == lines => None,
                    Some((p, _)) => Some((p, lines)),
                    None => Some((self.cursor, lines)),
                }
            }
            Copy | Cancel => return,
        }
        self.cursor.row = self.cursor.row.clamp(frame.oldest_stable_row, last_row);
        self.top = self
            .top
            .clamp(self.cursor.row - height + 1, self.cursor.row)
            .clamp(frame.oldest_stable_row, last_top);
    }

    pub fn selection(&self) -> Option<Selection> {
        let (mut anchor, lines) = self.anchor?;
        let mut end = self.cursor;
        if lines {
            if anchor.row > end.row {
                std::mem::swap(&mut anchor, &mut end);
            }
            anchor.col = 0;
            end.col = self.screen.cols.saturating_sub(1);
        }
        let mut selection = Selection::new(anchor, false, self.screen.alternate);
        selection.end = end;
        selection.active = true;
        selection.dragging = false;
        selection.cols = self.screen.cols;
        Some(selection)
    }
}

/// Read the entire selected range from the worker, even if a page jump skipped rows.
pub fn selected_text(
    terminal: &TerminalState,
    generation: u64,
    id: ScreenId,
    selection: Selection,
) -> Result<String, &'static str> {
    let screen = terminal.screen();
    if (
        screen.physical_cols,
        screen.physical_rows,
        terminal.is_alt_screen_active(),
        generation,
    ) != (id.cols, id.height, id.alternate, id.generation)
    {
        return Err("The terminal changed while selecting");
    }
    let first = selection.anchor.row.min(selection.end.row);
    let last = selection.anchor.row.max(selection.end.row);
    let (Some(start), Some(end)) = (
        screen.stable_row_to_phys(first),
        screen.stable_row_to_phys(last),
    ) else {
        return Err("Selected text left scrollback");
    };
    let mut result = Ok(String::new());
    screen.with_phys_lines(start..end + 1, |lines| {
        result = selection.text_from_rows(
            lines
                .iter()
                .enumerate()
                .map(|(i, line)| (first + i as isize, *line)),
            true,
            crate::clipboard::MAX_TEXT,
        );
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use CopyKey::*;

    fn terminal(input: &str) -> wezterm_term::Terminal {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let mut t = crate::terminal::create(
            wezterm_term::TerminalSize {
                cols: 8,
                rows: 3,
                ..Default::default()
            },
            tx,
        );
        t.advance_bytes(input.as_bytes());
        t
    }
    fn view(t: &TerminalState, offset: usize) -> Snapshot {
        Snapshot::capture(t, None, 1, offset, String::new(), 0)
    }
    #[test]
    fn reverse_line_selection_copies_pages_between_endpoints_and_joins_wrapped_lines() {
        let t =
            terminal("123456789\r\n\x1b[34mblue\x1b[0m\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven");
        let frame = view(&t, 0);
        let mut m = CopyMode::new(&frame);
        m.navigate(Line, &frame);
        m.navigate(PageUp, &frame);
        assert!(m.top < frame.first_stable_row);
        m.navigate(First, &frame);
        let selection = m.selection().unwrap();
        assert_eq!(selection.anchor, Point { row: m.top, col: 0 });
        assert_eq!(
            selected_text(&t, 0, m.screen, selection),
            Ok("123456789\r\nblue\r\nthree\r\nfour\r\nfive\r\nsix\r\nseven".into())
        );
        assert_eq!(m.top, frame.oldest_stable_row);
        m.navigate(Line, &frame);
        assert!(m.selection().is_none());
    }
    #[test]
    fn character_selection_keeps_graphemes_and_rejects_an_expired_screen() {
        let t = terminal("a界e\u{301}z");
        let frame = view(&t, 0);
        let mut m = CopyMode::new(&frame);
        for key in [Home, Right, Character, Right] {
            m.navigate(key, &frame);
        }
        assert_eq!(m.cursor.col, 3);
        assert_eq!(
            selected_text(&t, 0, m.screen, m.selection().unwrap()),
            Ok("界e\u{301}".into())
        );
        m.navigate(Left, &frame);
        assert_eq!(m.cursor.col, 1);
        let mut evicted = frame.clone();
        evicted.oldest_stable_row = m.cursor.row + 1;
        assert!(!m.valid(&evicted));
        assert!(selected_text(&t, 1, m.screen, m.selection().unwrap()).is_err());
    }
    #[test]
    fn alternate_screen_has_no_history_and_historical_entry_keeps_original_viewport() {
        let mut t = terminal("1\r\n2\r\n3\r\n4\r\n5");
        let f = view(&t, 1);
        let m = CopyMode::new(&f);
        assert_eq!(m.original_top, Some(f.first_stable_row));
        assert_eq!(
            m.cursor,
            Point {
                row: f.first_stable_row,
                col: 0
            }
        );
        t.advance_bytes(b"\x1b[?1049h");
        let alt = view(&t, 0);
        assert!(!m.valid(&alt));
        let mut m = CopyMode::new(&alt);
        for key in [Last, PageDown, First, PageUp] {
            m.navigate(key, &alt);
        }
        assert_eq!(m.top, alt.first_stable_row);
        assert_eq!(m.cursor.row, alt.first_stable_row);
        assert_eq!(CopyKey::from_vk(0x56, Mods::SHIFT), Some(Line));
        assert_eq!(CopyKey::from_vk(0x56, Mods::CTRL), None);
    }
}
