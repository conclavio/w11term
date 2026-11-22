use super::document::Document;
use w11term::terminal::{Snapshot, create};
use wezterm_term::{Terminal, TerminalSize};

fn terminal(input: &str) -> Terminal {
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    let mut t = create(
        TerminalSize {
            rows: 3,
            cols: 18,
            ..Default::default()
        },
        tx,
    );
    t.advance_bytes(input.as_bytes());
    t
}

fn frame(t: &Terminal, offset: usize) -> Snapshot {
    Snapshot::capture(t, None, 1, offset, String::new(), 0)
}

#[test]
fn scroll_uses_covered_rows_for_exclusive_end_and_degenerate_ranges() {
    let doc = Document::from_frame(&frame(&terminal("界e\u{301}\r\nאב\r\n"), 0));
    let next = doc.starts[1];
    assert_eq!(doc.covered_rows(&(0..next)), 0..=0);
    assert_eq!(doc.text(next - 1..next), "\r\n");
    assert_eq!(doc.covered_rows(&(next - 1..next)), 0..=0);
    assert_eq!(doc.covered_rows(&(next..next)), 1..=1);
    assert_eq!(doc.covered_rows(&(1..next + 1)), 0..=1);
    let eof = doc.len();
    assert_eq!(doc.covered_rows(&(eof..eof)), 2..=2);
    assert!(doc.retained_rows(&(eof..eof)).is_some());
}

#[test]
fn retained_rectangles_map_bidi_cells_against_the_current_viewport() {
    let mut t = terminal("one\r\nAאב12ג界\r\nthree");
    let doc = Document::from_frame(&frame(&t, 0));
    let span = doc.find(0..doc.len(), "Aא", false, false).unwrap();
    let row = doc.row(span.start);
    let rows = doc.retained_rows(&span).unwrap();
    assert_eq!(doc.cell_maps[row].visual_ranges(0..2), [0..1, 5..6]);

    t.advance_bytes(b"\r\nfour\r\nfive");
    let hidden = frame(&t, 0);
    assert!(!rows.matches_visible(&hidden));
    assert_eq!(doc.visible_row(row, &hidden), None);

    let revealed = frame(&t, 1);
    assert!(rows.matches_visible(&revealed));
    assert_eq!(doc.visible_row(row, &revealed), Some(0));
    assert_eq!(revealed.cell_maps()[0].visual_ranges(0..2), [0..1, 5..6]);
    assert_eq!(doc.text(span), "Aא");
}

#[test]
fn retained_rectangles_disappear_if_any_covered_row_changed() {
    let mut t = terminal("alpha\r\nbeta\r\ngamma");
    let doc = Document::from_frame(&frame(&t, 0));
    let range = doc.retained_rows(&(0..doc.starts[2])).unwrap();
    let unchanged = doc.retained_rows(&(0..doc.starts[1])).unwrap();
    t.advance_bytes(b"\x1b[2;1HZ");
    let latest = frame(&t, 0);
    assert!(!range.matches_visible(&latest));
    assert!(unchanged.matches_visible(&latest));
}
