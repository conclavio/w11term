use std::sync::Arc;
use w11term::{
    control::{CapturePng, Pane, Registry, Request},
    options::Source,
    session::{Command, Session},
    terminal::Snapshot,
};
use wezterm_term::TerminalSize;

async fn capture(session: &Session) -> Snapshot {
    let (tx, rx) = tokio::sync::oneshot::channel();
    session.input.send(Command::Capture(tx)).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), rx)
        .await
        .unwrap()
        .unwrap()
        .0
}
async fn feed(session: &Session, bytes: &str) -> Snapshot {
    session.input.send(Command::Paste(bytes.into())).unwrap();
    capture(session).await
}
fn text(snapshot: &Snapshot) -> String {
    snapshot
        .rows
        .iter()
        .map(|l| l.as_str().into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}
fn registry(session: &Session) -> Registry {
    let registry = Registry::default();
    registry.replace([Pane {
        id: 1,
        label: "test".into(),
        active: true,
        input: session.input.clone(),
    }]);
    registry
}

#[tokio::test]
async fn fragmented_update_and_control_capture_share_the_committed_frame() {
    let session = Session::start_sized(
        Source::Demo,
        TerminalSize {
            cols: 40,
            rows: 8,
            pixel_width: 400,
            pixel_height: 160,
            dpi: 96,
        },
    );
    let old = feed(&session, "old HUD").await;
    for piece in ["\x1b[?20", "26h", "\rnew HUD", "\x1b[2;1Hnew picture"] {
        let partial = feed(&session, piece).await;
        assert_eq!(partial.generation, old.generation);
        assert_eq!(text(&partial), text(&old));
        assert_eq!(text(&session.frames.load()), text(&old));
    }
    let registry = registry(&session);
    let expected = text(&old);
    let screenshot: CapturePng = Arc::new(move |frame, _, _| {
        assert_eq!(text(frame), expected);
        Ok((400, 160))
    });
    let screen = registry
        .execute(Request::GetText { pane: 1 }, screenshot.clone())
        .await
        .unwrap();
    assert!(screen["text"].as_str().unwrap().contains("old HUD"));
    let directory = tempfile::tempdir().unwrap();
    registry
        .execute(
            Request::Screenshot {
                pane: 1,
                path: directory.path().join("unused.png"),
            },
            screenshot,
        )
        .await
        .unwrap();
    let done = feed(&session, "\x1b[?2026l").await;
    assert!(text(&done).contains("new HUD\nnew picture"));
    assert_eq!(done.generation, old.generation + 1);
}

#[tokio::test]
async fn no_op_input_and_control_queries_do_not_publish_frames() {
    let session = Session::start(Source::Demo);
    let old = feed(&session, "ready").await;
    for _ in 0..20 {
        session.input.send(Command::Scroll(0)).unwrap();
        session.input.send(Command::Focus(false)).unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        session.input.send(Command::Events(tx)).unwrap();
        rx.await.unwrap();
        assert_eq!(capture(&session).await.generation, old.generation);
    }
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(session.frames.load().generation, old.generation);
}

#[tokio::test]
async fn same_read_boundaries_preserve_completed_frames_and_pending_prefix() {
    let session = Session::start(Source::Demo);
    feed(&session, "old").await;
    let completed = feed(&session, "\x1b[?2026h\rfirst\x1b[?2026l\x1b[?2026h\rsecond").await;
    assert!(text(&completed).starts_with("first"));
    let finished = feed(&session, "\x1b[?2026l").await;
    assert!(text(&finished).starts_with("second"));
    assert_eq!(finished.generation, completed.generation + 1);
    let prefix = feed(&session, "\rprefix\x1b[?2026h\rhidden").await;
    assert!(text(&prefix).starts_with("prefix"));
}

#[tokio::test]
async fn missing_sync_end_times_out_and_survives_resize() {
    let session = Session::start(Source::Demo);
    let old = feed(&session, "old").await;
    feed(&session, "\x1b[?2026h\rnew").await;
    for _ in 0..5 {
        feed(&session, "\x1b[?2026h").await;
    }
    let start = tokio::time::Instant::now();
    loop {
        let frame = capture(&session).await;
        if text(&frame).starts_with("new") {
            assert_eq!(frame.generation, old.generation + 1);
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "missing sync-end froze presentation"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    feed(&session, "\x1b[?2026h\rresized").await;
    session
        .input
        .send(Command::Resize(TerminalSize {
            cols: 100,
            rows: 30,
            pixel_width: 1000,
            pixel_height: 600,
            dpi: 96,
        }))
        .unwrap();
    let done = feed(&session, "\x1b[?2026l").await;
    assert_eq!(done.size.cols, 100);
}

#[tokio::test]
async fn in_place_kitty_edits_do_not_mutate_committed_pixels() {
    use wezterm_term::image::{ImageData, ImageDataType};
    fn image(snapshot: &Snapshot) -> Arc<ImageData> {
        snapshot
            .rows
            .iter()
            .flat_map(|l| l.visible_cells())
            .find_map(|c| {
                c.attrs()
                    .images()
                    .and_then(|images| images.first().map(|i| i.image_data().clone()))
            })
            .unwrap()
    }
    fn pixels(snapshot: &Snapshot) -> Vec<u8> {
        match snapshot.image_data(&image(snapshot)) {
            ImageDataType::Rgba8 { data, .. } => data.clone(),
            _ => panic!("unexpected format"),
        }
    }
    let session = Session::start_sized(
        Source::Demo,
        TerminalSize {
            cols: 40,
            rows: 8,
            pixel_width: 400,
            pixel_height: 160,
            dpi: 96,
        },
    );
    let old = feed(
        &session,
        "\x1b_Ga=T,t=d,f=32,s=2,v=2,i=1,c=2,r=2,q=2;AQID/wQFBv8HCAn/CgsM/w==\x1b\\",
    )
    .await;
    let original = pixels(&old);
    let held = feed(
        &session,
        "\x1b[?2026h\x1b_Ga=f,t=d,f=32,s=1,v=1,i=1,r=1,x=0,y=0,q=2;/wAA/w==\x1b\\",
    )
    .await;
    assert_eq!(pixels(&old), original);
    assert_eq!(pixels(&held), original);
    let done = feed(&session, "\x1b[?2026l").await;
    assert_eq!(&pixels(&done)[..4], &[255, 0, 0, 255]);
    assert_eq!(pixels(&old), original);
    assert_eq!(done.generation, old.generation + 1);
}
