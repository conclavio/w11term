use super::{Command, Session};
use crate::{options::Source, search::PaneSearchPreview};
use tokio::sync::oneshot;
use wezterm_term::TerminalSize;

async fn preview(pane: &Session, query: &str) -> PaneSearchPreview {
    let (reply, response) = oneshot::channel();
    pane.input
        .send(Command::SearchPreview {
            query: query.into(),
            reply,
        })
        .unwrap();
    response.await.unwrap()
}

async fn barrier(pane: &Session) {
    let (reply, response) = oneshot::channel();
    pane.input.send(Command::Frames(reply)).unwrap();
    response.await.unwrap();
}

#[tokio::test]
async fn pane_search_previews_and_failed_or_cancelled_choices_preserve_find_and_view() {
    let size = TerminalSize {
        cols: 24,
        rows: 2,
        ..Default::default()
    };
    let pane = Session::start_sized(Source::Demo, size);
    let other = Session::start_sized(Source::Demo, size);
    for p in [&pane, &other] {
        p.input
            .send(Command::Paste(
                "hit here\nsecond\nthird\nfourth\nend".into(),
            ))
            .unwrap();
        p.input.send(Command::SetReadOnly(true)).unwrap();
        p.input
            .send(Command::Search {
                query: "^end".into(),
                sensitive: true,
                regex: true,
            })
            .unwrap();
    }
    barrier(&pane).await;
    let before = pane.frames.load();
    assert_eq!(before.scroll_offset, 0);
    assert_eq!(before.search.as_ref().unwrap().total, 1);
    let anchor = preview(&pane, "hit").await.first.unwrap().1;
    let wrong = preview(&other, "hit").await.first.unwrap().1;
    let (reply, response) = oneshot::channel();
    pane.input
        .send(Command::SearchReveal {
            query: "hit".into(),
            anchor: wrong,
            reply,
        })
        .unwrap();
    assert!(!response.await.unwrap());
    let (reply, response) = oneshot::channel();
    drop(response);
    pane.input
        .send(Command::SearchReveal {
            query: "hit".into(),
            anchor: anchor.clone(),
            reply,
        })
        .unwrap();
    barrier(&pane).await;
    let after = pane.frames.load();
    assert_eq!(before.generation, after.generation);
    assert_eq!(before.scroll_offset, after.scroll_offset);
    assert_eq!(before.search, after.search);

    let (reply, response) = oneshot::channel();
    pane.input
        .send(Command::SearchReveal {
            query: "hit".into(),
            anchor,
            reply,
        })
        .unwrap();
    assert!(response.await.unwrap());
    barrier(&pane).await;
    let after = pane.frames.load();
    let search = after.search.as_ref().unwrap();
    assert_eq!(search.query, "hit");
    assert!(!search.regex && !search.sensitive);
    assert_eq!(search.selected, Some(0));
    assert_eq!(search.visible.len(), 1);
    assert!(after.scroll_offset > 0);
    assert_eq!(other.frames.load().search, before.search);
}
