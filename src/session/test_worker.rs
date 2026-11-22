use super::*;
use tokio::sync::oneshot;

pub(super) async fn capture(input: &mpsc::UnboundedSender<Command>) -> Snapshot {
    let (send, receive) = oneshot::channel();
    input.send(Command::Capture(send)).unwrap();
    receive.await.unwrap().0
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
        Source::Demo,
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
