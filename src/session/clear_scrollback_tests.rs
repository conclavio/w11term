use super::{
    test_worker::{capture, feed, text, with_demo},
    *,
};
use crate::interaction::{Point, Selection};
use tokio::sync::oneshot;

fn size() -> TerminalSize {
    TerminalSize {
        cols: 32,
        rows: 3,
        ..Default::default()
    }
}

#[tokio::test(start_paused = true)]
async fn clear_scrollback_keeps_live_rows_cursor_commands_and_recomputes_dirty_find() {
    with_demo(size(), async |input, frames| {
        feed(
            &input,
            "\x1b]133;C\x07old hit\x1b]133;D;0\x07\nold hit again\nlive A\nlive hit\nlive C",
        )
        .await;
        input
            .send(Command::Search {
                query: "hit".into(),
                sensitive: true,
                regex: true,
            })
            .unwrap();
        // The ordinary 150 ms Find refresh is still pending when Clear arrives.
        feed(&input, "\rhit updated").await;
        input.send(Command::ScrollEdge(true)).unwrap();
        input.send(Command::SetReadOnly(true)).unwrap();
        let before = capture(&input).await;
        let scrolled = frames.load();
        assert!(scrolled.scroll_offset > 0);
        assert_eq!(scrolled.search.as_ref().unwrap().total, 3);
        assert_eq!(
            before.command_history.back().unwrap().output.as_deref(),
            Ok("old hit")
        );
        let selection = Selection::new(
            Point {
                row: scrolled.first_stable_row,
                col: 0,
            },
            false,
            false,
        );
        input.send(Command::ClearScrollback).unwrap();
        let after = capture(&input).await;
        let view = frames.load();
        for frame in [&after, &*view] {
            assert_eq!(frame.scroll_offset, 0);
            assert_eq!(frame.first_stable_row, before.first_stable_row);
            assert_eq!(frame.oldest_stable_row, before.first_stable_row);
            assert_eq!(frame.scrollback_clear_floor, Some(before.first_stable_row));
            assert_eq!(frame.scrollback_clear_generation, 1);
            assert_eq!(frame.cursor, before.cursor);
            assert_eq!(text(frame), text(&before));
            assert!(Arc::ptr_eq(&frame.command_history, &before.command_history));
            assert_eq!(frame.connection_generation, before.connection_generation);
            let find = frame.search.as_ref().unwrap();
            assert_eq!(find.total, 2);
            assert_eq!(find.visible.len(), 2);
            assert!(find.regex && find.sensitive);
            assert!(find.visible.iter().any(|(selected, _)| *selected));
        }
        let (reply, result) = oneshot::channel();
        input
            .send(Command::CopyRange((&*scrolled).into(), selection, reply))
            .unwrap();
        assert_eq!(result.await.unwrap(), Err("Selected text left scrollback"));
        let (reply, result) = oneshot::channel();
        input.send(Command::ExportText(reply)).unwrap();
        assert_eq!(
            result.await.unwrap().unwrap(),
            "live A\r\nlive hit\r\nhit updated"
        );
        input.send(Command::ScrollEdge(true)).unwrap();
        input.send(Command::ClearScrollback).unwrap();
        assert_eq!(capture(&input).await.generation, after.generation);
        assert_eq!(frames.load().generation, view.generation);
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn clear_scrollback_is_a_noop_on_an_empty_history_or_the_alternate_screen() {
    with_demo(size(), async |input, frames| {
        let empty = feed(&input, "first").await;
        input.send(Command::ClearScrollback).unwrap();
        assert_eq!(capture(&input).await.generation, empty.generation);
        let primary = feed(&input, "\nsecond\nthird\nfourth").await;
        assert!(primary.oldest_stable_row < primary.first_stable_row);
        let alt = feed(&input, "\x1b[?1049halternate").await;
        assert!(alt.alternate);
        input.send(Command::ClearScrollback).unwrap();
        assert_eq!(capture(&input).await.generation, alt.generation);
        let restored = feed(&input, "\x1b[?1049l").await;
        assert_eq!(restored.scrollback_clear_generation, 0);
        assert_eq!(restored.scrollback_clear_floor, None);
        assert_eq!(restored.oldest_stable_row, primary.oldest_stable_row);
        assert_eq!(text(&restored), text(&primary));
        input.send(Command::ScrollEdge(true)).unwrap();
        capture(&input).await;
        assert!(text(&frames.load()).starts_with("first"));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn clear_scrollback_commits_its_selection_floor_and_find_with_synchronized_output() {
    with_demo(size(), async |input, frames| {
        feed(&input, "old\none\ntwo\nthree").await;
        input
            .send(Command::Search {
                query: "old".into(),
                sensitive: false,
                regex: false,
            })
            .unwrap();
        let held = feed(&input, "\x1b[?2026h\rpartial").await;
        let held_view = frames.load();
        assert_eq!(held.search.as_ref().unwrap().total, 1);
        input.send(Command::ClearScrollback).unwrap();
        let unchanged = capture(&input).await;
        assert_eq!(unchanged.generation, held.generation);
        assert_eq!(frames.load().generation, held_view.generation);
        assert_eq!(unchanged.scrollback_clear_floor, None);
        assert!(!text(&unchanged).contains("partial"));
        let committed = feed(&input, "\rfinal  \x1b[?2026l").await;
        assert_eq!(
            committed.generation,
            held.generation.max(held_view.generation) + 1
        );
        assert_eq!(committed.scrollback_clear_generation, 1);
        assert_eq!(
            committed.scrollback_clear_floor,
            Some(held.first_stable_row)
        );
        assert_eq!(committed.search.as_ref().unwrap().total, 0);
        assert!(text(&committed).contains("final"));
        let later = feed(&input, "\nnext\nagain").await;
        assert!(later.first_stable_row > committed.first_stable_row);
        assert_eq!(
            later.scrollback_clear_floor,
            committed.scrollback_clear_floor
        );
        assert_eq!(
            later.scrollback_clear_generation,
            committed.scrollback_clear_generation
        );
    })
    .await;
}
