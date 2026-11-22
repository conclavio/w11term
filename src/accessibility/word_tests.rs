use super::*;
use w11term::terminal::create;
use wezterm_term::TerminalSize;

fn frame(text: &str, cols: usize, rows: usize) -> Snapshot {
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    let mut t = create(
        TerminalSize {
            rows,
            cols,
            ..Default::default()
        },
        tx,
    );
    t.advance_bytes(text.as_bytes());
    Snapshot::capture(&t, None, 1, 0, String::new(), 0)
}

fn assert_words(doc: &Document, expected: &[&str]) {
    let mut start = 0;
    for &word in expected {
        let (end, moved) = doc.advance(start, TextUnit_Word, 1);
        assert_eq!(moved, 1);
        assert_eq!(doc.text(start..end), word);
        for cell in start..end {
            assert_eq!(doc.unit(cell, TextUnit_Word), start..end);
        }
        assert_eq!(doc.advance(end, TextUnit_Word, -1), (start, -1));
        start = end;
    }
    assert_eq!(start, doc.len());
    assert_eq!(doc.advance(start, TextUnit_Word, 1), (start, 0));
    assert_eq!(doc.advance(0, TextUnit_Word, -1), (0, 0));
    assert_eq!(
        doc.advance(0, TextUnit_Word, i32::MAX),
        (start, expected.len() as i32)
    );
}

#[test]
fn words_group_configured_punctuation_and_trailing_space_between_hard_breaks() {
    let doc = Document::from_frame(&frame("  Aא界e\u{301}:-   β\u{a0}z\r\n  _cat!", 32, 2));
    assert_words(
        &doc,
        &[
            "  ",
            "Aא界e\u{301}",
            ":-   ",
            "β\u{a0}",
            "z",
            "\r\n",
            "  ",
            "_cat",
            "!",
        ],
    );
    assert_words(&Document::from_frame(&frame("", 8, 1)), &[]);
    assert_words(
        &Document::from_frame(&frame("\r\n\r\nword", 8, 3)),
        &["\r\n", "\r\n", "word"],
    );
}

#[test]
fn words_and_punctuation_cross_soft_wraps_but_stop_at_a_hard_break() {
    let doc = Document::from_frame(&frame("A界βZxx:-   next\r\nend", 4, 5));
    assert_eq!(doc.starts.len(), 5);
    assert_words(&doc, &["A界βZxx", ":-   ", "next", "\r\n", "end"]);
}

#[test]
fn updated_delimiters_rebuild_the_cached_document_without_mutating_old_ranges() {
    let frame = frame("a-b..e\u{301}界", 20, 1);
    let appearance = RwLock::new(Appearance::default());
    let mut state = State {
        geometry: Geometry {
            bounds: (0, 0, 0, 0),
            cell_width: 1.0,
            cell_height: 1.0,
            padding: 0.0,
        },
        selected: None,
        cached: None,
        requested: None,
    };
    let original = state.document(&frame, &appearance).unwrap();
    assert!(Arc::ptr_eq(
        &original,
        &state.document(&frame, &appearance).unwrap()
    ));
    appearance.write().unwrap().interaction.word_delimiters = "\u{301}界".into();
    let custom = state.document(&frame, &appearance).unwrap();
    assert!(!Arc::ptr_eq(&original, &custom));
    assert_words(&custom, &["a-b..", "e\u{301}界"]);
    assert_words(&original, &["a", "-", "b", "..", "e\u{301}界"]);
    assert!(Arc::ptr_eq(
        &custom,
        &state.document(&frame, &appearance).unwrap()
    ));
    appearance
        .write()
        .unwrap()
        .interaction
        .word_delimiters
        .clear();
    assert_words(
        &state.document(&frame, &appearance).unwrap(),
        &["a-b..e\u{301}界"],
    );
}
