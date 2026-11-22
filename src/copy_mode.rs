//! Keyboard selection in retained terminal rows. Coordinates stay logical for copying
//! even when the renderer reorders a row for bidirectional display.
use crate::{
    interaction::{Point, Selection},
    terminal::Snapshot,
};
use wezterm_term::{KeyModifiers as Mods, TerminalState};

mod words;
pub use words::{WordMotion, word_target};

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
    Word(WordMotion),
    Character,
    Line,
    Block,
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
                0x25 if !mods.contains(Mods::SHIFT) => Some(Word(WordMotion::PreviousStart)),
                0x27 if !mods.contains(Mods::SHIFT) => Some(Word(WordMotion::NextStart)),
                0x24 => Some(First),
                0x23 => Some(Last),
                0x43 => Some(Copy),
                0x2d if !mods.contains(Mods::SHIFT) => Some(Copy),
                0x56 if !mods.contains(Mods::SHIFT) => Some(Block),
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
            0x57 if !shift => Word(WordMotion::NextStart),
            0x42 if !shift => Word(WordMotion::PreviousStart),
            0x45 if !shift => Word(WordMotion::NextEnd),
            0x56 if shift => Line,
            0x56 => Character,
            0x52 if !shift => Block,
            0x0d | 0x59 => Copy,
            0x1b => Cancel,
            _ => return None,
        })
    }
    pub fn repeats(self) -> bool {
        !matches!(
            self,
            Self::Character | Self::Line | Self::Block | Self::Copy | Self::Cancel
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenId {
    cols: usize,
    height: usize,
    alternate: bool,
    generation: u64,
    row_epoch: u64,
    history_clear_count: u64,
}

impl From<&Snapshot> for ScreenId {
    fn from(frame: &Snapshot) -> Self {
        Self {
            cols: frame.cols,
            height: frame.rows.len(),
            alternate: frame.alternate,
            generation: frame.connection_generation,
            row_epoch: frame.row_epoch,
            history_clear_count: frame.history_clear_count,
        }
    }
}

impl ScreenId {
    pub(crate) fn matches(self, terminal: &TerminalState, generation: u64) -> bool {
        self.same_coordinates(terminal, generation)
            && self.history_clear_count == terminal.screen().history_clear_count()
    }

    fn same_coordinates(self, terminal: &TerminalState, generation: u64) -> bool {
        let screen = terminal.screen();
        self.generation == generation
            && self.cols == screen.physical_cols
            && self.height == screen.physical_rows
            && self.alternate == terminal.is_alt_screen_active()
            && self.row_epoch == screen.row_epoch()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Shape {
    All { empty_last_row: bool },
    Character,
    Line,
    Block,
}

#[derive(Debug)]
pub struct CopyMode {
    pub screen: ScreenId,
    pub cursor: Point,
    /// Keep the viewport pinned as shell output arrives, including when starting at offset zero.
    pub top: isize,
    pub original_top: Option<isize>,
    anchor: Option<(Point, Shape)>,
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

    /// Retain only stable endpoints. A large history costs no memory until Copy,
    /// when the same bounded plain-text formatter as any keyboard selection runs.
    pub(crate) fn select_all(
        terminal: &TerminalState,
        generation: u64,
        id: ScreenId,
        frame: &Snapshot,
    ) -> Result<Self, &'static str> {
        let screen = terminal.screen();
        if id != frame.into()
            || !id.same_coordinates(terminal, generation)
            || id.history_clear_count != screen.history_clear_count()
        {
            return Err("The terminal changed while selecting");
        }
        let rows = crate::terminal::retained_text_rows(terminal)?;
        let last = rows.end - 1;
        let line = screen
            .physical_lines()
            .nth(last)
            .expect("a screen has at least one row");
        let last_col = line
            .visible_cells()
            .take_while(|cell| cell.cell_index() < screen.physical_cols)
            .filter(|cell| !cell.str().trim_end().is_empty())
            .map(|cell| cell.cell_index() + cell.width().saturating_sub(1))
            .last();
        let mut mode = Self::new(frame);
        mode.anchor = Some((
            Point {
                row: screen.phys_to_stable_row_index(rows.start),
                col: 0,
            },
            Shape::All {
                empty_last_row: last_col.is_none(),
            },
        ));
        mode.cursor = Point {
            row: screen.phys_to_stable_row_index(last),
            col: last_col
                .unwrap_or(0)
                .min(screen.physical_cols.saturating_sub(1)),
        };
        Ok(mode)
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
            Some((_, Shape::All { .. })) if self.screen.alternate => {
                "Copy mode · Alternate screen text selected · y or Enter copy · Esc cancel"
            }
            Some((_, Shape::All { .. })) => {
                "Copy mode · All retained text selected · y or Enter copy · Esc cancel"
            }
            None => {
                "Copy mode · Arrows or hjkl move · w/b/e words · v character · Shift+V line · Ctrl+V or r block · Esc exit"
            }
            Some((_, Shape::Character)) => {
                "Copy mode · Character selection · y or Enter copy · Esc cancel"
            }
            Some((_, Shape::Line)) => "Copy mode · Line selection · y or Enter copy · Esc cancel",
            Some((_, Shape::Block)) => {
                "Copy mode · Rectangular selection · y or Enter copy · Esc cancel"
            }
        }
    }

    fn toggle_shape(&mut self, shape: Shape) {
        self.anchor = match self.anchor {
            Some((_, active)) if active == shape => None,
            Some((anchor, _)) => Some((anchor, shape)),
            None => Some((self.cursor, shape)),
        };
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
            Character => self.toggle_shape(Shape::Character),
            Line => self.toggle_shape(Shape::Line),
            Block => self.toggle_shape(Shape::Block),
            Word(_) | Copy | Cancel => return,
        }
        self.move_to(self.cursor, frame);
    }

    pub fn move_to(&mut self, point: Point, frame: &Snapshot) {
        if let Some((_, shape @ Shape::All { .. })) = &mut self.anchor {
            *shape = Shape::Character;
        }
        let height = frame.rows.len().max(1) as isize;
        let last_top = frame.first_stable_row + frame.scroll_offset as isize;
        self.cursor = Point {
            row: point
                .row
                .clamp(frame.oldest_stable_row, last_top + height - 1),
            col: point.col.min(frame.cols.saturating_sub(1)),
        };
        self.top = self
            .top
            .clamp(self.cursor.row - height + 1, self.cursor.row)
            .clamp(frame.oldest_stable_row, last_top);
    }

    pub fn selection(&self) -> Option<Selection> {
        let (mut anchor, shape) = self.anchor?;
        let mut end = self.cursor;
        if shape == Shape::Line {
            if anchor.row > end.row {
                std::mem::swap(&mut anchor, &mut end);
            }
            anchor.col = 0;
            end.col = self.screen.cols.saturating_sub(1);
        }
        let mut selection = Selection::new(anchor, shape == Shape::Block, self.screen.alternate);
        selection.end = end;
        selection.end_exclusive = matches!(
            shape,
            Shape::All {
                empty_last_row: true
            }
        );
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
    trim_block: bool,
) -> Result<String, &'static str> {
    let screen = terminal.screen();
    if terminal.synchronized_output() {
        return Err("Terminal output is updating; try again after the frame finishes");
    }
    if !id.same_coordinates(terminal, generation) {
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
    if id.history_clear_count != screen.history_clear_count() {
        return Err("The terminal changed while selecting");
    }
    let lines = screen
        .physical_lines()
        .skip(start)
        .take(end - start + 1)
        .enumerate()
        .map(|(offset, line)| (first + offset as isize, line));
    selection.text_from_rows(lines, trim_block, crate::clipboard::MAX_TEXT)
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
    fn select_all_keeps_the_viewport_and_selects_wrapped_unicode_history_through_the_last_cell() {
        let mut t = terminal("123456789\r\n\r\n\x1b[35me\u{301}\x1b[0m\r\nlast界");
        let frame = view(&t, 1);
        let mode = CopyMode::select_all(&t, 0, (&frame).into(), &frame).unwrap();
        let selection = mode.selection().unwrap();
        assert!(mode.valid(&frame));
        assert!(frame.first_stable_row > frame.oldest_stable_row);
        assert_eq!(mode.top, frame.first_stable_row);
        assert_eq!(mode.original_top, Some(mode.top));
        assert_eq!(
            selection.anchor,
            Point {
                row: frame.oldest_stable_row,
                col: 0
            }
        );
        assert_eq!(selection.end.col, 5); // Includes the trailing column of 界.
        let expected = "123456789\r\n\r\ne\u{301}\r\nlast界";
        assert_eq!(crate::terminal::export_text(&t).as_deref(), Ok(expected));
        t.advance_bytes("xy\r\nlater");
        assert!(mode.valid(&view(&t, 0)));
        assert_eq!(
            selected_text(&t, 0, mode.screen, selection, true).as_deref(),
            Ok(expected)
        );
    }

    #[test]
    fn select_all_preserves_intentional_blank_lines_and_only_visits_the_active_screen() {
        let mut t = terminal("");
        let copy = |t: &TerminalState| {
            let frame = view(t, 0);
            let mode = CopyMode::select_all(t, 0, (&frame).into(), &frame).unwrap();
            selected_text(t, 0, mode.screen, mode.selection().unwrap(), true).unwrap()
        };
        assert_eq!(copy(&t), "");
        t.advance_bytes("primary\r\n\r\n");
        assert_eq!(copy(&t), "primary\r\n\r\n");
        let primary = view(&t, 0);
        t.advance_bytes("\x1b[?1049h\x1b[Halt\x1b[3;2H界\x1b[H");
        assert!(CopyMode::select_all(&t, 0, (&primary).into(), &primary).is_err());
        let frame = view(&t, 0);
        let mut mode = CopyMode::select_all(&t, 0, (&frame).into(), &frame).unwrap();
        assert!(mode.instructions().contains("Alternate screen"));
        assert_eq!(copy(&t), "alt\r\n\r\n 界");
        mode.navigate(Left, &frame);
        assert!(mode.instructions().contains("Character selection"));
        t.advance_bytes("\x1b[?1049l");
        assert!(!mode.valid(&view(&t, 0)));
        assert_eq!(copy(&t), "primary\r\n\r\n");
    }

    #[test]
    fn select_all_ending_at_an_empty_row_does_not_grow_when_that_row_receives_output() {
        for initial in ["", "previous\r\n"] {
            let mut t = terminal(initial);
            let frame = view(&t, 0);
            let mode = CopyMode::select_all(&t, 0, (&frame).into(), &frame).unwrap();
            let selection = mode.selection().unwrap();
            assert_eq!(selection.span(selection.end.row, frame.cols), Some(0..0));
            t.advance_bytes("new");
            assert_eq!(
                selected_text(&t, 0, mode.screen, selection, true).as_deref(),
                Ok(initial)
            );
        }
    }

    #[test]
    fn select_all_rejects_reconnect_clear_and_natural_eviction_even_at_the_same_dimensions() {
        let mut t = crate::terminal::create_with_profile(
            wezterm_term::TerminalSize {
                cols: 8,
                rows: 2,
                ..Default::default()
            },
            tokio::sync::mpsc::unbounded_channel().0,
            1u32.try_into().unwrap(),
            Default::default(),
        );
        t.advance_bytes("one\r\ntwo\r\nthree");
        let frame = view(&t, 0);
        let mode = CopyMode::select_all(&t, 0, (&frame).into(), &frame).unwrap();
        assert!(CopyMode::select_all(&t, 1, mode.screen, &frame).is_err());
        t.advance_bytes("\r\nfour");
        assert!(!mode.valid(&view(&t, 0)));
        assert_eq!(
            selected_text(&t, 0, mode.screen, mode.selection().unwrap(), true),
            Err("Selected text left scrollback")
        );
        let frame = view(&t, 0);
        let mode = CopyMode::select_all(&t, 0, (&frame).into(), &frame).unwrap();
        t.advance_bytes("\x1b[3J");
        assert!(!mode.valid(&view(&t, 0)));
        assert!(CopyMode::select_all(&t, 0, mode.screen, &frame).is_err());
    }

    #[test]
    fn select_all_accepts_the_standard_copy_gestures_in_keyboard_selection() {
        for (vk, mods) in [
            (0x43, Mods::CTRL),
            (0x43, Mods::CTRL | Mods::SHIFT),
            (0x2d, Mods::CTRL),
        ] {
            assert_eq!(CopyKey::from_vk(vk, mods), Some(Copy));
        }
        assert!(!Copy.repeats());
        for (vk, mods) in [
            (0x43, Mods::NONE),
            (0x2d, Mods::NONE),
            (0x2d, Mods::CTRL | Mods::SHIFT),
            (0x43, Mods::CTRL | Mods::ALT),
            (0x43, Mods::CTRL | Mods::SUPER),
        ] {
            assert_eq!(CopyKey::from_vk(vk, mods), None);
        }
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
            selected_text(&t, 0, m.screen, selection, true),
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
            selected_text(&t, 0, m.screen, m.selection().unwrap(), true),
            Ok("界e\u{301}".into())
        );
        m.navigate(Left, &frame);
        assert_eq!(m.cursor.col, 1);
        let mut evicted = frame.clone();
        evicted.oldest_stable_row = m.cursor.row + 1;
        assert!(!m.valid(&evicted));
        assert!(selected_text(&t, 1, m.screen, m.selection().unwrap(), true).is_err());
    }

    #[test]
    fn block_copies_each_physical_row_with_the_configured_padding() {
        let t = terminal("ab界e\u{301}XYZ123 L \r\nQ\r\n---Z");
        let frame = view(&t, 0);
        let first = Point { row: 0, col: 3 };
        let last = Point { row: 3, col: 5 };
        assert!(first.row < frame.first_stable_row);
        for (anchor, end) in [(first, last), (last, first)] {
            let mut mode = CopyMode::new(&frame);
            mode.move_to(anchor, &frame);
            mode.navigate(Block, &frame);
            mode.move_to(end, &frame);
            let block = mode.selection().unwrap();
            assert!(block.block);
            assert_eq!(block.anchor, anchor);
            for row in first.row..=last.row {
                assert_eq!(block.span(row, frame.cols), Some(3..6));
            }
            // The left edge intersects the second column of 界. The following
            // row is soft-wrapped, and another row has no text in these columns.
            assert_eq!(
                selected_text(&t, 0, mode.screen, block.clone(), true).as_deref(),
                Ok("界e\u{301}X\r\n L\r\n\r\nZ")
            );
            assert_eq!(
                selected_text(&t, 0, mode.screen, block, false).as_deref(),
                Ok("界e\u{301}X\r\n L \r\n   \r\nZ  ")
            );
        }
    }

    #[test]
    fn changing_shape_keeps_the_original_anchor_and_toggling_it_off_releases_it() {
        let t = terminal("123456\r\nABCDEF");
        let frame = view(&t, 0);
        let mut mode = CopyMode::new(&frame);
        let anchor = Point { row: 1, col: 2 };
        let end = Point { row: 0, col: 4 };
        mode.move_to(anchor, &frame);
        mode.navigate(Character, &frame);
        mode.move_to(end, &frame);
        mode.navigate(Block, &frame);
        assert!(mode.instructions().contains("Rectangular selection"));
        assert_eq!(mode.selection().unwrap().anchor, anchor);
        mode.navigate(Line, &frame);
        let lines = mode.selection().unwrap();
        assert!(!lines.block);
        assert_eq!(lines.span(0, frame.cols), Some(0..frame.cols));
        mode.navigate(Block, &frame);
        assert_eq!(mode.selection().unwrap().span(0, frame.cols), Some(2..5));
        mode.navigate(Character, &frame);
        let linear = mode.selection().unwrap();
        assert!(!linear.block);
        assert_eq!(linear.anchor, anchor);
        assert_eq!(linear.end, end);
        mode.navigate(Block, &frame);
        mode.navigate(Block, &frame);
        assert!(mode.selection().is_none());
        mode.navigate(Block, &frame);
        assert_eq!(mode.selection().unwrap().anchor, end);

        assert_eq!(CopyKey::from_vk(0x52, Mods::NONE), Some(Block));
        assert_eq!(CopyKey::from_vk(0x56, Mods::CTRL), Some(Block));
        assert_eq!(CopyKey::from_vk(0x56, Mods::CTRL | Mods::SHIFT), None);
        assert_eq!(CopyKey::from_vk(0x52, Mods::SHIFT), None);
        assert!(!Block.repeats());
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
        assert_eq!(CopyKey::from_vk(0x56, Mods::CTRL), Some(Block));
    }

    #[test]
    fn word_stops_use_delimiter_runs_and_leading_grapheme_columns() {
        use WordMotion::*;
        let t = terminal("a//界e\u{301} z");
        let id = ScreenId::from(&view(&t, 0));
        let target = |col, motion, delimiters| {
            word_target(&t, 0, id, Point { row: 0, col }, motion, delimiters).map(|p| p.col)
        };
        assert_eq!(target(0, NextStart, "/"), Some(1));
        assert_eq!(target(1, NextEnd, "/"), Some(2));
        assert_eq!(target(1, NextStart, "/"), Some(3));
        assert_eq!(target(5, PreviousStart, "/"), Some(3));
        assert_eq!(target(4, PreviousStart, "/"), Some(1)); // second column of 界
        assert_eq!(target(4, NextEnd, "/"), Some(5)); // e + combining accent
        assert_eq!(target(0, NextEnd, ""), Some(5));
        assert_eq!(target(0, NextStart, ""), Some(7));
        assert_eq!(target(7, NextStart, "/"), Some(7));
        assert_eq!(target(7, NextEnd, "/"), Some(7));
        assert_eq!(target(0, PreviousStart, "/"), Some(0));

        let wrapped = terminal("abcdef界g x");
        let id = ScreenId::from(&view(&wrapped, 0));
        let p = |row, col| Point { row, col };
        let target = |point, motion| word_target(&wrapped, 0, id, point, motion, "");
        assert_eq!(target(p(0, 7), NextEnd), Some(p(1, 0)));
        assert_eq!(target(p(1, 0), PreviousStart), Some(p(0, 0)));
        assert_eq!(target(p(0, 6), NextStart), Some(p(1, 2)));
    }

    #[test]
    fn word_stops_join_soft_wraps_and_cross_empty_hard_rows() {
        use WordMotion::*;
        let t = terminal("one twoABCDEFGHI\r\nJK\r\n\r\nnext");
        let frame = view(&t, 0);
        let p = |row, col| Point { row, col };
        let target =
            |origin, motion| word_target(&t, 0, (&frame).into(), origin, motion, "").unwrap();
        assert_eq!(target(p(0, 4), NextEnd), p(1, 7));
        assert_eq!(target(p(1, 3), PreviousStart), p(0, 4));
        assert_eq!(target(p(0, 4), NextStart), p(2, 0));
        assert_eq!(target(p(1, 7), NextEnd), p(2, 1));
        assert_eq!(target(p(2, 0), PreviousStart), p(0, 4));
        assert_eq!(target(p(2, 7), PreviousStart), p(2, 0)); // blank row padding
        assert_eq!(target(p(2, 7), NextStart), p(4, 0));
        assert_eq!(target(p(3, 0), NextEnd), p(4, 3));
        assert_eq!(target(p(4, 0), PreviousStart), p(2, 0));

        let mut mode = CopyMode::new(&frame);
        mode.move_to(p(0, 4), &frame);
        mode.navigate(Character, &frame);
        mode.move_to(target(mode.cursor, NextEnd), &frame);
        assert_eq!(mode.top, 0);
        assert_eq!(
            selected_text(&t, 0, mode.screen, mode.selection().unwrap(), true).as_deref(),
            Ok("twoABCDEFGHI")
        );
    }

    #[test]
    fn word_queries_require_the_original_screen_and_a_retained_origin() {
        let mut t = terminal("abc");
        let id = ScreenId::from(&view(&t, 0));
        let origin = Point { row: 0, col: 0 };
        let target = |t: &TerminalState, generation, point| {
            word_target(t, generation, id, point, WordMotion::NextEnd, "")
        };
        assert_eq!(target(&t, 0, origin), Some(Point { row: 0, col: 2 }));
        assert_eq!(target(&t, 1, origin), None);
        assert_eq!(target(&t, 0, Point { row: -1, col: 0 }), None);
        assert_eq!(target(&t, 0, Point { row: 0, col: 8 }), None);
        t.advance_bytes(b"\x1b[?1049h");
        assert_eq!(target(&t, 0, origin), None);
        t.advance_bytes(b"\x1b[?1049l");
        assert_eq!(target(&t, 0, origin), None);

        t.advance_bytes(b"\r\n1\r\n2\r\n3\r\n4");
        let id = ScreenId::from(&view(&t, 0));
        let live = Point {
            row: t.screen().visible_row_to_stable_row(0),
            col: 0,
        };
        t.advance_bytes(b"\x1b[3J");
        assert!(t.screen().stable_row_to_phys(origin.row).is_none());
        assert!(t.screen().stable_row_to_phys(live.row).is_some());
        assert_eq!(
            word_target(&t, 0, id, live, WordMotion::NextStart, ""),
            None
        );
        let after_clear = ScreenId::from(&view(&t, 0));
        assert_eq!(
            word_target(&t, 0, after_clear, origin, WordMotion::NextStart, ""),
            None
        );
        assert!(word_target(&t, 0, after_clear, live, WordMotion::NextStart, "").is_some());
    }

    #[test]
    fn word_key_bindings_preserve_modified_chords() {
        use WordMotion::*;
        for (vk, mods, expected) in [
            (0x57, Mods::NONE, NextStart),
            (0x42, Mods::NONE, PreviousStart),
            (0x45, Mods::NONE, NextEnd),
            (0x27, Mods::CTRL, NextStart),
            (0x25, Mods::CTRL, PreviousStart),
        ] {
            assert_eq!(CopyKey::from_vk(vk, mods), Some(Word(expected)));
            assert!(Word(expected).repeats());
        }
        assert_eq!(CopyKey::from_vk(0x57, Mods::SHIFT), None);
        assert_eq!(CopyKey::from_vk(0x27, Mods::CTRL | Mods::SHIFT), None);
        assert_eq!(CopyKey::from_vk(0x57, Mods::ALT), None);
    }
}
