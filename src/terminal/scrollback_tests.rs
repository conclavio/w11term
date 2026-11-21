use super::*;

fn terminal(cols: usize, rows: usize, limit: u32) -> Terminal {
    let (tx, _) = mpsc::unbounded_channel();
    create_with_scrollback(
        TerminalSize {
            cols,
            rows,
            ..Default::default()
        },
        tx,
        limit.try_into().unwrap(),
    )
}

fn rows(t: &Terminal) -> Vec<String> {
    t.screen()
        .physical_lines()
        .map(|row| row.as_str().trim_end().to_owned())
        .collect()
}

fn resize(t: &mut Terminal, cols: usize, rows: usize) {
    let size = t.get_size();
    t.resize(TerminalSize { cols, rows, ..size });
}

#[test]
fn scrollback_limit_evicts_physical_rows_and_preserves_their_stable_indices() {
    for limit in [0, 1, 4, u32::from(ScrollbackLines::MAX)] {
        let mut term = terminal(12, 2, limit);
        let count = limit as usize + 6;
        for i in 0..count {
            term.advance_bytes(format!("row{i}\r\n"));
        }
        let screen = term.screen();
        let oldest = (count + 1 - (2 + limit as usize)) as isize;
        assert_eq!(screen.scrollback_rows(), 2 + limit as usize);
        assert_eq!(screen.phys_to_stable_row_index(0), oldest);
        assert_eq!(screen.stable_row_to_phys(oldest - 1), None);
        assert_eq!(screen.stable_row_to_phys(oldest), Some(0));
        assert_eq!(
            screen.physical_lines().next().unwrap().as_str().trim_end(),
            format!("row{oldest}")
        );
        let oldest_view = Snapshot::capture(&term, None, 1, usize::MAX, String::new(), 0);
        assert_eq!(oldest_view.scroll_offset, limit as usize);
        assert_eq!(oldest_view.first_stable_row, oldest);
    }
}

#[test]
fn scrollback_limit_applies_immediately_to_width_reflow_and_keeps_the_cursor_row() {
    for (limit, expected) in [
        (0, vec!["qrst", "uvwx", ""]),
        (2, vec!["ijkl", "mnop", "qrst", "uvwx", ""]),
    ] {
        let mut term = terminal(12, 3, limit);
        term.advance_bytes("abcdefghijkl\r\nmnopqrstuvwx\r\n");
        resize(&mut term, 4, 3);
        assert_eq!(rows(&term), expected);
        assert_eq!(term.screen().phys_row(0), limit as usize);
        assert_eq!(
            term.screen().phys_to_stable_row_index(0),
            4 - limit as isize
        );
        assert_eq!(term.cursor_pos().y, 2);
        term.advance_bytes("now");
        assert_eq!(rows(&term).last().unwrap(), "now");
        resize(&mut term, 12, 3);
        assert!(term.screen().phys_row(0) <= limit as usize);
        assert!(rows(&term).iter().any(|line| line == "now"));
    }
    // Clearing the continuation and moving back above it leaves a wrap marker
    // on the last retained row after resize prunes the unused blank tail.
    let mut term = terminal(4, 2, 0);
    term.advance_bytes("abcde\x1b[2;1H\x1b[2K\x1b[H");
    resize(&mut term, 2, 2);
    assert_eq!(rows(&term), ["ab", "cd"]);
}

#[test]
fn scrollback_limit_survives_height_changes_alt_screen_and_transport_reset() {
    for conpty in [false, true] {
        let mut term = terminal(8, 4, 1);
        if conpty {
            term.enable_conpty_quirks();
        }
        term.advance_bytes("A\r\nB\r\nC\r\nD");
        resize(&mut term, 8, 2);
        assert_eq!(rows(&term), ["B", "C", "D"]);
        assert_eq!(term.screen().phys_to_stable_row_index(0), 1);
        term.advance_bytes("\x1b[?1049h\x1b[H1\r\n2\r\n3\r\n4");
        assert_eq!(term.screen().phys_row(0), 0);
        let before = Snapshot::capture(&term, None, 1, 0, String::new(), 0);
        resize(&mut term, 8, 1);
        assert_eq!(rows(&term), ["4"]);
        let after = Snapshot::capture(&term, Some(&before), 2, 0, String::new(), 0);
        assert_eq!(after.rows[0].as_str().trim_end(), "4");
        assert_eq!(after.first_stable_row, before.first_stable_row + 1);
        resize(&mut term, 8, 2);
        assert_eq!(term.screen().phys_row(0), 0);
        term.advance_bytes("\x1b[?1049l");
        assert!(term.screen().phys_row(0) <= 1);
        assert!(rows(&term).iter().all(|line| !line.contains('4')));
    }
    for (limit, expected) in [(0, vec!["", ""]), (1, vec!["last", "", ""])] {
        let mut term = terminal(8, 2, limit);
        term.advance_bytes("first\r\nlast");
        term.reset_connection();
        assert_eq!(rows(&term), expected);
        term.advance_bytes("new");
        assert_eq!(
            term.screen()
                .physical_lines()
                .nth(term.screen().phys_row(0))
                .unwrap()
                .as_str()
                .trim_end(),
            "new"
        );
    }
}

#[test]
fn scrollback_reflow_keeps_only_command_markers_on_retained_rows() {
    let mut term = terminal(12, 3, 1);
    term.advance_bytes("old1\r\nold2\r\nold3\r\nabcdefghijkl\r\nmnopqrstuvwx\r\n\x1b]133;C\x07ok");
    assert_eq!(term.screen().phys_to_stable_row_index(0), 2);
    resize(&mut term, 4, 3);
    assert_eq!(rows(&term), ["mnop", "qrst", "uvwx", "ok"]);
    assert_eq!(term.screen().phys_to_stable_row_index(0), 6);
    term.advance_bytes("\x1b]133;D;0\x07");
    assert_eq!(term.last_command().unwrap().output.as_deref(), Ok("ok"));

    let mut term = terminal(12, 3, 1);
    term.advance_bytes("\x1b]133;C\x07abcdefghijkl\r\nmnopqrstuvwx\r\nok");
    resize(&mut term, 4, 3);
    term.advance_bytes("\x1b]133;D;0\x07");
    assert_eq!(
        term.last_command().unwrap().output.as_deref(),
        Err(&"Command output left scrollback")
    );
}
