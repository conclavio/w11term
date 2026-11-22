use super::*;
use crate::{
    options::Source,
    session::{RangeScroll, Session},
    terminal::create_with_scrollback,
};
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use wezterm_term::TerminalSize;

fn size() -> TerminalSize {
    TerminalSize {
        cols: 16,
        rows: 3,
        ..Default::default()
    }
}

fn terminal(limit: u32) -> Terminal {
    let (writer, _) = mpsc::unbounded_channel();
    create_with_scrollback(size(), writer, limit.try_into().unwrap())
}

fn frame(term: &Terminal, offset: usize) -> Snapshot {
    Snapshot::capture(term, None, 1, offset, String::new(), 0)
}

const LINES: &str = "A\r\nB\r\nC\r\nD\r\nE\r\nF";

#[test]
fn drag_targets_keep_the_same_row_or_clamp_to_the_oldest_survivor() {
    let mut term = terminal(4);
    term.advance_bytes(LINES);
    let view = frame(&term, 2).scrollback_view().unwrap();
    assert_eq!((view.oldest, view.top, view.live, view.rows), (0, 1, 3, 3));
    let captured_live_row = view.target(Some(view.live));
    let evicted = view.target(Some(view.top));
    let follow_output = view.target(None);

    term.advance_bytes("\r\nG\r\nH\r\nI");
    let next = frame(&term, 0).scrollback_view().unwrap();
    assert!(view.same_screen(&next));
    assert_eq!((next.oldest, next.top, next.live), (2, 6, 6));
    for (target, expected_top, expected_line) in [
        (captured_live_row, 3, "D"),
        (evicted, 2, "C"),
        (view.target(Some(isize::MIN)), 2, "C"),
        (view.target(Some(isize::MAX)), 6, "G"),
        (follow_output, 6, "G"),
    ] {
        let shown = frame(&term, target.offset(&term, 0).unwrap());
        assert_eq!(shown.first_stable_row, expected_top);
        assert_eq!(shown.rows[0].as_str().trim_end(), expected_line);
    }
}

#[test]
fn absent_history_alternate_screen_and_uncommitted_output_are_ineligible() {
    let mut disabled = terminal(0);
    disabled.advance_bytes(LINES);
    assert!(frame(&disabled, 1).scrollback_view().is_none());
    let mut term = terminal(4);
    assert!(frame(&term, 0).scrollback_view().is_none());
    term.advance_bytes(LINES);
    let view = frame(&term, 0).scrollback_view().unwrap();
    let target = view.target(Some(view.oldest));
    term.advance_bytes("\x1b[?2026h");
    assert_eq!(target.offset(&term, 0), None);
    term.advance_bytes("\x1b[?2026l");
    assert_eq!(target.offset(&term, 0), Some(3));
    term.advance_bytes("\x1b[?1049h");
    term.advance_bytes(LINES);
    assert!(frame(&term, 3).scrollback_view().is_none());
    assert_eq!(target.offset(&term, 0), None);
    term.advance_bytes("\x1b[?1049l");
    let restored = frame(&term, 0).scrollback_view().unwrap();
    assert!(!view.same_screen(&restored));
    assert_eq!(target.offset(&term, 0), None);
}

#[test]
fn remote_erase_invalidates_a_drag_even_after_history_regrows_at_the_same_size() {
    let mut term = terminal(8);
    term.advance_bytes(LINES);
    let original = frame(&term, 0);
    let view = original.scrollback_view().unwrap();
    let target = view.target(Some(view.live));
    let surviving_line = original.retained_rows(view.live, view.live).unwrap();
    term.advance_bytes("\x1b[3J");
    assert!(frame(&term, 0).scrollback_view().is_none());
    assert_eq!(target.offset(&term, 0), None);
    term.advance_bytes("\r\nG\r\nH\r\nI");
    let next = frame(&term, 0).scrollback_view().unwrap();
    assert_eq!(next.live - next.oldest, view.live - view.oldest);
    assert_eq!(next.oldest, view.live);
    assert!(!view.same_screen(&next));
    assert_eq!(target.offset(&term, 0), None);
    assert_eq!(next.target(Some(next.oldest)).offset(&term, 0), Some(3));
    // Erasing offscreen rows still preserves a UIA text range on a surviving live row.
    assert_eq!(surviving_line.scroll_offset(&term, 0, true), Some(3));
}

#[test]
fn a_target_never_applies_to_a_resized_reconnected_or_different_screen() {
    let mut term = terminal(8);
    term.advance_bytes(LINES);
    let view = frame(&term, 0).scrollback_view().unwrap();
    let target = view.target(Some(view.oldest));
    assert_eq!(target.offset(&term, 1), None);

    let mut other = terminal(8);
    other.advance_bytes(LINES);
    assert!(!view.same_screen(&frame(&other, 0).scrollback_view().unwrap()));
    assert_eq!(target.offset(&other, 0), None);

    term.resize(TerminalSize {
        cols: 8,
        rows: 2,
        ..size()
    });
    assert_eq!(target.offset(&term, 0), None);
    term.resize(size());
    let resized = frame(&term, 0).scrollback_view().unwrap();
    assert!(!view.same_screen(&resized));
    assert_eq!(target.offset(&term, 0), None);
    let before_reconnect = resized.target(None);
    term.reset_connection();
    term.advance_bytes(LINES);
    assert!(!resized.same_screen(&frame(&term, 0).scrollback_view().unwrap()));
    assert_eq!(before_reconnect.offset(&term, 0), None);
}

#[tokio::test]
async fn worker_publishes_pointer_and_accessibility_scrolls_and_rejects_a_cleared_target() {
    let session = Session::start_configured(Source::Demo, size(), false);
    let capture = async || {
        let (tx, rx) = oneshot::channel();
        session.input.send(Command::Capture(tx)).unwrap();
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .unwrap()
            .unwrap()
            .0
    };
    let accessible_scroll = |target, alive| {
        let (command, ack) = RangeScroll::viewport(target, Arc::new(AtomicBool::new(alive)));
        session.input.send(command).unwrap();
        ack.wait(Duration::from_secs(5))
    };
    session
        .input
        .send(Command::Paste("A\nB\nC\nD\nE\nF".into()))
        .unwrap();
    let live = capture().await;
    let original = live.scrollback_view().unwrap();
    session.input.send(Command::SetReadOnly(true)).unwrap();
    session
        .input
        .send(original.scroll_to(Some(original.oldest)))
        .unwrap();
    assert_eq!(capture().await.first_stable_row, original.live);
    assert_eq!(session.frames.load().first_stable_row, original.oldest);
    assert!(!accessible_scroll(original.target(None), false));
    assert_eq!(session.frames.load().first_stable_row, original.oldest);
    assert!(accessible_scroll(original.target(None), true));
    assert_eq!(session.frames.load().first_stable_row, original.live);

    session.input.send(Command::ClearScrollback).unwrap();
    assert!(capture().await.scrollback_view().is_none());
    session.input.send(Command::SetReadOnly(false)).unwrap();
    session
        .input
        .send(Command::Paste("\nG\nH\nI".into()))
        .unwrap();
    let current = capture().await.scrollback_view().unwrap();
    assert!(!original.same_screen(&current));
    session
        .input
        .send(original.scroll_to(Some(original.oldest)))
        .unwrap();
    capture().await;
    assert!(!accessible_scroll(
        original.target(Some(original.oldest)),
        true
    ));
    assert_eq!(session.frames.load().first_stable_row, current.live);
    assert!(accessible_scroll(
        current.target(Some(current.oldest)),
        true
    ));
    assert_eq!(session.frames.load().first_stable_row, current.oldest);
}
