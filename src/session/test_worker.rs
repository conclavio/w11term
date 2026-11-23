use super::*;
use tokio::sync::oneshot;

pub(super) async fn capture(input: &mpsc::UnboundedSender<Command>) -> Snapshot {
    let (send, receive) = oneshot::channel();
    input.send(Command::Capture(send)).unwrap();
    receive.await.unwrap().0
}

#[tokio::test(start_paused = true)]
async fn select_all_waits_for_a_committed_screen_and_keeps_the_scrolled_read_only_view() {
    let size = TerminalSize {
        cols: 16,
        rows: 2,
        ..Default::default()
    };
    with_demo(size, async |input, frames| {
        let select = async |screen| {
            let (reply, receive) = oneshot::channel();
            input.send(Command::SelectAll { screen, reply }).unwrap();
            receive.await.unwrap()
        };
        let copy = async |mode: &crate::copy_mode::CopyMode| {
            let (reply, receive) = oneshot::channel();
            input
                .send(Command::CopyRange {
                    screen: mode.screen,
                    selection: mode.selection().unwrap(),
                    trim_block: true,
                    reply,
                })
                .unwrap();
            receive.await.unwrap()
        };
        let before = feed(&input, "one\ntwo\nthree").await;
        let previous = select((&before).into()).await.unwrap();
        feed(&input, "\x1b[?2026h\nfour").await;
        assert!(
            select((&before).into())
                .await
                .unwrap_err()
                .contains("updating")
        );
        assert!(copy(&previous).await.unwrap_err().contains("updating"));
        feed(&input, "\x1b[?2026l").await;
        input.send(Command::ScrollEdge(true)).unwrap();
        input.send(Command::SetReadOnly(true)).unwrap();
        capture(&input).await;
        let scrolled = frames.load();
        assert!(scrolled.scroll_offset > 0);
        let mode = select((&*scrolled).into()).await.unwrap();
        assert!(mode.valid(&scrolled));
        assert_eq!(mode.original_top, Some(scrolled.first_stable_row));
        assert_eq!(
            copy(&mode).await.as_deref(),
            Ok("one\r\ntwo\r\nthree\r\nfour")
        );
        assert_eq!(frames.load().generation, scrolled.generation);
        input.send(Command::ClearScrollback).unwrap();
        assert!(copy(&mode).await.is_err());
    })
    .await;
}

#[tokio::test]
async fn raw_recording_viewer_uses_saved_grid_and_emits_only_title_alerts() {
    use crate::options::Source;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("raw example.vt");
    let size = TerminalSize {
        cols: 37,
        rows: 9,
        ..Default::default()
    };
    let mut recorder = crate::recorder::Recorder::start(&path, size).unwrap();
    recorder
        .write(b"\x1b]2;Viewer title\x07\x07\x1b]9;old notice\x07\x1b[999;1Hlast row")
        .unwrap();
    recorder.finish().unwrap();
    let source = Source::open_recording(path.clone()).unwrap();
    assert_eq!(source, Source::ViewRecording(path.clone()));
    let saved = crate::workspace::SavedSource::capture(&source, None).unwrap();
    assert_eq!(saved.resolve(&Default::default()).unwrap(), source);
    let pane = Session::start_view_recording_pane(path.clone(), TerminalSize::default());
    assert_eq!(capture(&pane.input).await.bytes_received, 0);
    pane.input
        .send(Command::Resize(TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 800,
            pixel_height: 480,
            dpi: 96,
        }))
        .unwrap();
    let frame = capture(&pane.input).await;
    assert_eq!((frame.cols, frame.rows.len()), (37, 9));
    assert_eq!(
        (frame.size.pixel_width, frame.size.pixel_height),
        (370, 180)
    );
    assert!(frame.rows[8].as_str().starts_with("last row"));
    assert_eq!(frame.title, "Viewer title");
    assert!(
        !pane
            .clipboard_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    let events = pane.events.since(0).events;
    assert!(events.iter().any(|e| e.kind == "title"));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind.as_str(), "bell" | "notification" | "sound"))
    );

    std::fs::write(path.with_extension("vt.jsonl"), b"interrupted header").unwrap();
    assert!(Source::open_recording(path).is_err());
}

#[tokio::test]
async fn gui_replay_uses_an_explicit_grid_or_waits_for_the_first_measured_grid() {
    let recording = tempfile::NamedTempFile::new().unwrap();
    let bytes = b"READY\x1b[999;2H\x1b_Ga=T,i=7,f=24,s=1,v=1,c=1,r=1,C=1,q=2;/wAA\x1b\\\x1b[2;1H";
    std::fs::write(recording.path(), bytes).unwrap();
    let measured = TerminalSize {
        rows: 6,
        cols: 16,
        pixel_width: 160,
        pixel_height: 96,
        dpi: 96,
    };
    for initial in [measured, TerminalSize::default()] {
        let pane = Session::start_replay_pane(recording.path().into(), initial, false);
        if initial == TerminalSize::default() {
            assert_eq!(capture(&pane.input).await.bytes_received, 0);
            // A resize without a whole pixel per cell cannot place images.
            pane.input
                .send(Command::Resize(TerminalSize {
                    pixel_width: measured.cols - 1,
                    ..measured
                }))
                .unwrap();
            assert_eq!(capture(&pane.input).await.bytes_received, 0);
            pane.input.send(Command::Resize(measured)).unwrap();
        }
        let frame = capture(&pane.input).await;
        assert_eq!(frame.bytes_received, bytes.len() as u64);
        assert!(frame.rows[0].as_str().starts_with("READY"));
        let last_row = &frame.rows[measured.rows - 1];
        let images = last_row.get_cell(1).unwrap().attrs().images().unwrap();
        assert_eq!(images[0].image_id(), Some(7));

        pane.input
            .send(Command::Resize(TerminalSize {
                pixel_width: measured.pixel_width * 2,
                ..measured
            }))
            .unwrap();
        // The recording ends with the cursor on row two. A second playback would write READY there.
        assert!(
            capture(&pane.input).await.rows[1]
                .as_str()
                .trim()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn headless_replay_runs_without_a_resize() {
    let recording = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(recording.path(), b"READY").unwrap();
    let pane = Session::start(Source::Replay(recording.path().into()));
    assert!(
        capture(&pane.input).await.rows[0]
            .as_str()
            .starts_with("READY")
    );
}

#[tokio::test(start_paused = true)]
async fn block_copy_applies_the_requested_trimming_to_retained_rows() {
    use crate::interaction::{Interaction, Point, Selection};
    let size = TerminalSize {
        cols: 16,
        rows: 2,
        ..Default::default()
    };
    with_demo(size, async |input, _| {
        let frame = feed(&input, "alpha\nb\nmore\nlatest").await;
        let first = frame.oldest_stable_row;
        assert!(first + 1 < frame.first_stable_row);
        let mut selection = Selection::new(Point { row: first, col: 0 }, true, false);
        selection.update(Point {
            row: first + 1,
            col: 4,
        });
        selection.cols = frame.cols;
        let default_trim = Interaction::default().trim_block_selection;
        for (trim_block, expected) in [(default_trim, "alpha\r\nb"), (false, "alpha\r\nb    ")] {
            let (reply, result) = oneshot::channel();
            input
                .send(Command::CopyRange {
                    screen: (&frame).into(),
                    selection: selection.clone(),
                    trim_block,
                    reply,
                })
                .unwrap();
            assert_eq!(result.await.unwrap().as_deref(), Ok(expected));
        }
    })
    .await;
}

pub(super) async fn feed(input: &mpsc::UnboundedSender<Command>, bytes: &str) -> Snapshot {
    input.send(Command::Paste(bytes.into())).unwrap();
    capture(input).await
}

pub(super) fn text(frame: &Snapshot) -> String {
    frame
        .rows
        .iter()
        .map(|line| line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Both the worker and its caller share the paused test clock, so real OS
/// scheduling cannot advance the synchronized-output or search timers.
pub(super) async fn with_demo(
    size: TerminalSize,
    exercise: impl AsyncFnOnce(mpsc::UnboundedSender<Command>, FrameStore),
) {
    with_demo_colors(
        size,
        colors::PaneColors::new(ColorScheme::Default, crate::windows_appearance::subscribe),
        None,
        exercise,
    )
    .await;
}

pub(super) async fn with_demo_colors(
    size: TerminalSize,
    colors: colors::PaneColors,
    viewer: Option<mpsc::UnboundedSender<TransportInput>>,
    exercise: impl AsyncFnOnce(mpsc::UnboundedSender<Command>, FrameStore),
) {
    with_worker(Source::Demo, size, colors, viewer, exercise).await;
}

pub(super) async fn with_worker(
    source: Source,
    size: TerminalSize,
    colors: colors::PaneColors,
    viewer: Option<mpsc::UnboundedSender<TransportInput>>,
    exercise: impl AsyncFnOnce(mpsc::UnboundedSender<Command>, FrameStore),
) {
    let (input, commands) = mpsc::unbounded_channel();
    let (writer, transport) = mpsc::unbounded_channel();
    let mut terminal =
        terminal::create_with_palette(size, writer.clone(), Default::default(), colors.effective());
    if let Some(viewer) = viewer {
        terminal::enter_herdr_viewer(&mut terminal, &viewer, &Default::default());
    }
    let initial = Snapshot::capture(&terminal, None, 1, 0, String::new(), 0);
    let frames = FrameStore::new(initial.clone());
    let (_notices, notices) = mpsc::channel(1);
    let worker = run(
        source,
        terminal,
        commands,
        writer,
        transport,
        frames.clone(),
        Services {
            colors,
            prompts: Default::default(),
            events: Default::default(),
            player: Default::default(),
            notices,
            live_frames: FrameStore::new(initial),
            sharing: Default::default(),
            durable: None,
            replay_ready: true,
        },
    );
    let client = async {
        exercise(input.clone(), frames).await;
        input.send(Command::Shutdown).unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(worker, client);
    })
    .await
    .unwrap();
}
