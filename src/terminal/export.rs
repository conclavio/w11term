use crate::interaction::{TextShape, plain_text};
use wezterm_term::TerminalState;

const MAX_TEXT: usize = 32 * 1024 * 1024;
const TOO_LARGE: &str = "Scrollback exceeds the 32 MiB text export limit";

fn capture(terminal: &TerminalState, limit: usize) -> Result<String, &'static str> {
    if terminal.synchronized_output() {
        return Err("Terminal output is updating; try again after the frame finishes");
    }
    let screen = terminal.screen();
    let first = if terminal.is_alt_screen_active() {
        screen.phys_row(0)
    } else {
        0
    };
    let cursor = screen
        .phys_row(terminal.cursor_pos().y)
        .min(screen.scrollback_rows() - 1);
    // The cursor row preserves real final line breaks. Text below the cursor
    // can survive cursor addressing and must still be included.
    let last = screen
        .physical_lines()
        .enumerate()
        .skip(cursor + 1)
        .rfind(|(_, line)| {
            line.visible_cells()
                .any(|cell| !cell.str().trim_end().is_empty())
        })
        .map_or(cursor, |(row, _)| row);
    let cols = screen.physical_cols;
    let rows = screen.physical_lines().skip(first).take(last + 1 - first);
    plain_text(
        rows.map(|line| (line, 0..cols)),
        cols,
        TextShape::Linear,
        limit,
        TOO_LARGE,
    )
}

pub(crate) fn export_text(terminal: &TerminalState) -> Result<String, &'static str> {
    capture(terminal, MAX_TEXT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal;
    use wezterm_term::{CellAttributes, Line, Terminal, TerminalSize};

    fn engine(cols: usize, rows: usize) -> Terminal {
        let (writer, _rx) = tokio::sync::mpsc::unbounded_channel();
        terminal::create(
            TerminalSize {
                cols,
                rows,
                ..TerminalSize::default()
            },
            writer,
        )
    }

    #[test]
    fn retained_text_joins_wraps_and_preserves_hard_blank_lines_and_unicode() {
        let mut t = engine(6, 3);
        t.advance_bytes("  界e\u{301}  wrapped  \r\n\r\n\x1b[31mred\x1b[0m\r\nlast\r\n");
        assert!(t.screen().scrollback_rows() > 3);
        let cursor = t.cursor_pos();
        let export = export_text(&t).unwrap();
        assert_eq!(export, "  界e\u{301}  wrapped\r\n\r\nred\r\nlast\r\n");
        assert_eq!(t.cursor_pos(), cursor);
        t.advance_bytes("later");
        assert!(export.ends_with("last\r\n"));
    }

    #[test]
    fn excludes_unused_rows_but_keeps_text_below_the_cursor_and_alternate_isolated() {
        let mut t = engine(16, 6);
        assert_eq!(export_text(&t), Ok(String::new()));
        t.advance_bytes("one\r\n\r\nthree\x1b[H");
        assert_eq!(export_text(&t).unwrap(), "one\r\n\r\nthree");
        t.advance_bytes("\x1b[?1049h\x1b[Halternate\x1b[3;4Hz\x1b[H");
        assert_eq!(export_text(&t).unwrap(), "alternate\r\n\r\n   z");
        t.advance_bytes("\x1b[?1049l");
        assert_eq!(export_text(&t).unwrap(), "one\r\n\r\nthree");
    }

    #[test]
    fn synchronized_edits_cannot_escape_before_commit_or_watchdog_expiry() {
        let mut t = engine(20, 5);
        t.advance_bytes("before\x1b[?2026h middle");
        assert!(export_text(&t).unwrap_err().contains("updating"));
        t.advance_bytes("\x1b[?2026l");
        assert_eq!(export_text(&t).unwrap(), "before middle");
        t.advance_bytes("\x1b[?2026h after");
        assert!(export_text(&t).is_err());
        t.end_synchronized_output();
        assert_eq!(export_text(&t).unwrap(), "before middle after");
    }

    #[test]
    fn cap_counts_utf8_hard_newlines_and_column_gaps_without_counting_trimmed_padding() {
        let mut t = engine(64, 6);
        t.advance_bytes("界 \t \r\n\x1b[4Ge\u{301}    ");
        let expected = "界\r\n   e\u{301}";
        assert_eq!(capture(&t, expected.len()).unwrap(), expected);
        assert_eq!(capture(&t, expected.len() - 1), Err(TOO_LARGE));
        assert_eq!(capture(&engine(64, 6), 0).unwrap(), "");
    }

    #[test]
    fn exports_more_than_the_clipboard_limit() {
        let mut t = engine(500, 3_000);
        let row = "x".repeat(500);
        let line = Line::from_text(&row, &CellAttributes::default(), 1, None);
        for i in 0..3_000 {
            *t.screen_mut().line_mut(i) = line.clone();
        }
        let text = export_text(&t).unwrap();
        assert!(text.len() > crate::clipboard::MAX_TEXT);
        assert_eq!(text.len(), row.len() * 3_000 + 2 * 2_999);
        assert!(text.split("\r\n").all(|line| line == row));
    }

    #[tokio::test]
    async fn session_exports_through_a_read_only_scrolled_pane_without_moving_its_view() {
        use crate::{
            options::Source,
            session::{Command, Session},
        };
        use tokio::sync::oneshot;

        let fixture = tempfile::NamedTempFile::new().unwrap();
        let text = "first\r\nsecond\r\nthird\r\nfourth\r\nlast";
        std::fs::write(fixture.path(), text).unwrap();
        let pane = Session::start_sized(
            Source::Replay(fixture.path().into()),
            TerminalSize {
                cols: 12,
                rows: 2,
                ..TerminalSize::default()
            },
        );
        pane.input.send(Command::SetReadOnly(true)).unwrap();
        pane.input.send(Command::ScrollEdge(true)).unwrap();
        let (tx, reply) = oneshot::channel();
        pane.input.send(Command::ExportText(tx)).unwrap();
        assert_eq!(reply.await.unwrap().unwrap(), text);
        let before = pane.frames.load();
        assert!(before.scroll_offset > 0);
        let (tx, reply) = oneshot::channel();
        pane.input.send(Command::ExportText(tx)).unwrap();
        assert_eq!(reply.await.unwrap().unwrap(), text);
        let after = pane.frames.load();
        assert_eq!(after.generation, before.generation);
        assert_eq!(after.first_stable_row, before.first_stable_row);
    }
}
