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
fn retained_rows_validate_every_row_and_clamp_alignment_after_eviction() {
    let mut t = terminal(20, 4, 4);
    t.advance_bytes("a\r\nb\r\nc\r\nd");
    let frame = Snapshot::capture(&t, None, 1, 0, String::new(), 0);
    let all = frame.retained_rows(0, 3).unwrap();
    let middle = all.slice(1, 2).unwrap();
    assert!(all.slice(-1, 0).is_none());
    assert!(middle.slice(1, 3).is_none());
    assert!(all.slice(2, 1).is_none());
    assert!(all.matches_visible(&frame));
    // Appending enough lines to evict row 0 keeps rows 1 and 2 intact.
    t.advance_bytes("\r\ne\r\nf\r\ng\r\nh\r\ni");
    let live = Snapshot::capture(&t, None, 2, 0, String::new(), 0);
    assert_eq!(live.oldest_stable_row, 1);
    assert!(!middle.matches_visible(&live));
    assert_eq!(all.scroll_offset(&t, 0, true), None);
    assert_eq!(middle.scroll_offset(&t, 0, true), Some(4));
    assert_eq!(middle.scroll_offset(&t, 0, false), Some(4));
    let historical = Snapshot::capture(&t, None, 3, 3, String::new(), 0);
    assert!(middle.matches_visible(&historical)); // Only row 2 overlaps.
    assert!(!all.matches_visible(&historical)); // Its first row was evicted.
    assert_eq!(middle.scroll_offset(&t, 1, true), None);

    let current = live.retained_rows(5, 8).unwrap();
    t.advance_bytes("\x1b[2;1H!"); // Change an interior row, leaving both endpoints intact.
    assert_eq!(current.scroll_offset(&t, 0, true), None);
    assert!(
        current
            .slice(5, 5)
            .unwrap()
            .scroll_offset(&t, 0, true)
            .is_some()
    );
    let rewritten = Snapshot::capture(&t, None, 4, 0, String::new(), 0);
    assert!(!current.matches_visible(&rewritten));
}

#[test]
fn retained_ranges_and_snapshots_distinguish_a_wrap_change_on_identical_text() {
    let mut t = terminal(4, 2, 4);
    t.advance_bytes("abcd\r\nxy");
    let mut frame = Snapshot::capture(&t, None, 1, 0, String::new(), 0);
    assert!(!frame.rows[0].last_cell_was_wrapped());
    for (wrapped, revision_delta) in [(true, 0), (false, 1)] {
        let retained = frame.retained_rows(0, 1).unwrap();
        let other = retained.slice(1, 1).unwrap();
        let seqno = frame.rows[0].current_seqno() + revision_delta;
        t.screen_mut()
            .line_mut(0)
            .set_last_cell_was_wrapped(wrapped, seqno);
        let next = Snapshot::capture(&t, Some(&frame), 2, 0, String::new(), 0);
        assert_eq!(next.rows[0].as_str(), frame.rows[0].as_str());
        assert_eq!(next.rows[0].current_seqno(), seqno);
        assert_eq!(next.rows[0].last_cell_was_wrapped(), wrapped);
        assert_eq!(next.row_is_wrapped(0), wrapped);
        assert!(!next.same_presentation(&frame));
        assert!(!retained.matches_visible(&next));
        assert_eq!(retained.scroll_offset(&t, 0, true), None);
        assert!(other.matches_visible(&next));
        assert_eq!(other.scroll_offset(&t, 0, true), Some(0));
        frame = next;
    }
}

#[test]
fn retained_rows_keep_live_rows_across_clear_and_reject_reused_coordinates() {
    let mut t = terminal(20, 3, 4);
    let capture = |t: &Terminal| {
        let f = Snapshot::capture(t, None, 1, 0, String::new(), 0);
        f.retained_rows(f.first_stable_row, f.first_stable_row)
            .unwrap()
    };
    t.advance_bytes("1\r\n2\r\n3\r\n4");
    let oldest = Snapshot::capture(&t, None, 1, usize::MAX, String::new(), 0);
    let removed = oldest.retained_rows(0, 0).unwrap();
    let mixed = oldest.retained_rows(0, 1).unwrap();
    let surviving = capture(&t);
    t.advance_bytes("\x1b[3J");
    assert_eq!(removed.scroll_offset(&t, 0, true), None);
    assert_eq!(mixed.scroll_offset(&t, 0, true), None);
    assert_eq!(surviving.scroll_offset(&t, 0, true), Some(0));
    assert!(surviving.matches_visible(&Snapshot::capture(&t, None, 1, 0, String::new(), 0)));
    let before = capture(&t);
    resize(&mut t, 18, 3);
    resize(&mut t, 20, 3);
    assert_eq!(before.scroll_offset(&t, 0, true), None);
    let before = capture(&t);
    t.advance_bytes("\x1b[?1049h");
    assert_eq!(before.scroll_offset(&t, 0, true), None);
    let alt = capture(&t);
    assert_eq!(alt.scroll_offset(&t, 0, false), Some(0));
    t.advance_bytes("\x1b[?1049l");
    assert_eq!(before.scroll_offset(&t, 0, true), None);
    assert_eq!(alt.scroll_offset(&t, 0, true), None);
    let before = capture(&t);
    t.advance_bytes("\x1bc");
    assert_eq!(before.scroll_offset(&t, 0, true), None);
    let before = capture(&t);
    t.reset_connection();
    assert_eq!(before.scroll_offset(&t, 0, true), None);
    let before = capture(&t);
    let other = terminal(20, 3, 4);
    assert_eq!(before.scroll_offset(&other, 0, true), None);
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
