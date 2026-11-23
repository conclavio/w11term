use std::sync::Arc;
use w11term::{
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
async fn inspector_reports_parser_modes_while_rendered_output_is_held() {
    use termwiz::input::KeyboardEncoding;
    use wezterm_input_types::KittyKeyboardFlags;
    let session = Session::start(Source::Demo);
    let old = feed(&session, "committed").await;
    session.input.send(Command::Inspect(true)).unwrap();
    capture(&session).await;
    let initial = session.frames.inspection().unwrap();
    assert_eq!((initial.cols, initial.rows), (old.size.cols, old.size.rows));
    assert!(!initial.alternate && !initial.synchronized && !initial.bracketed_paste);
    assert_eq!(initial.keyboard, KeyboardEncoding::Xterm);

    let pending = feed(
        &session,
        "\x1b[?2026h\x1b[?1049h\x1b[?1002;1006;2004h\x1b[>3uprivate update",
    )
    .await;
    assert_eq!(pending.generation, old.generation);
    let active = session.frames.inspection().unwrap();
    assert!(active.synchronized && active.alternate && active.bracketed_paste && active.mouse_drag);
    assert!(!active.mouse_click && !active.mouse_motion);
    assert_eq!(active.mouse_encoding, wezterm_term::MouseEncoding::SGR);
    assert_eq!(
        active.keyboard,
        KeyboardEncoding::Kitty(KittyKeyboardFlags::from_bits_truncate(3))
    );
    assert_eq!(text(&session.frames.load()), text(&old));

    // Let the engine's existing synchronized-output watchdog release the batch.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    session
        .frames
        .wait_for_view(old.generation, Some(Some(active)), || {
            std::time::Instant::now() >= deadline
        });
    capture(&session).await;
    assert!(!session.frames.inspection().unwrap().synchronized);
    assert!(text(&session.frames.load()).contains("private update"));
    feed(&session, "\x1b[?1049l\x1b[?1002;1006;2004l").await;
    assert_eq!(session.frames.inspection().unwrap(), initial);
    let resized = TerminalSize {
        cols: 99,
        rows: 36,
        ..old.size
    };
    session.input.send(Command::Resize(resized)).unwrap();
    capture(&session).await;
    let modes = session.frames.inspection().unwrap();
    assert_eq!((modes.cols, modes.rows), (99, 36));
    session.input.send(Command::Inspect(false)).unwrap();
    feed(&session, "\x1b[?2004h").await;
    assert!(session.frames.inspection().is_none());
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
async fn xterm_private_mode_restore_preserves_synchronized_frame_boundaries() {
    let session = Session::start(Source::Demo);
    feed(&session, "\x1b[?2026h\x1b[?2026s\x1b[?2026lold").await;
    let held = feed(&session, "\rfirst\x1b[?2026r\rhidden").await;
    assert!(text(&held).starts_with("first"));
    let released = feed(&session, "\x1b[?2026l").await;
    assert!(text(&released).starts_with("hidden"));
    assert_eq!(released.generation, held.generation + 1);

    let held = feed(
        &session,
        "\x1b[?2026s\x1b[?2026h\rsecond\x1b[?2026r\x1b[?2026h\rpending",
    )
    .await;
    assert!(text(&held).starts_with("second"));
    let released = feed(&session, "\x1b[?2026l").await;
    assert!(text(&released).starts_with("pending"));
    assert_eq!(released.generation, held.generation + 1);
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
