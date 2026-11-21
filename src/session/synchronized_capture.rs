use super::*;
use crate::control::{CapturePng, Pane, Registry, Request};
use std::{future::poll_fn, pin::Pin, task::Poll, time::Duration};
use tokio::sync::oneshot;

async fn capture(input: &mpsc::UnboundedSender<Command>) -> Snapshot {
    let (send, receive) = oneshot::channel();
    input.send(Command::Capture(send)).unwrap();
    receive.await.unwrap().0
}

async fn feed(input: &mpsc::UnboundedSender<Command>, bytes: &str) -> Snapshot {
    input.send(Command::Paste(bytes.into())).unwrap();
    capture(input).await
}

fn text(frame: &Snapshot) -> String {
    frame
        .rows
        .iter()
        .map(|line| line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn assert_pending<F: Future>(mut future: Pin<&mut F>) {
    assert!(
        poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await,
        "wait-for-text completed before the synchronized update was committed"
    );
}

#[tokio::test(start_paused = true)]
async fn fragmented_update_and_control_capture_share_the_committed_frame() {
    // Run the normal session loop on the test clock. In the desktop app the
    // worker owns a separate runtime, so pausing only the control client would
    // still let OS scheduling consume the synchronized-output watchdog.
    let size = TerminalSize {
        cols: 40,
        rows: 8,
        pixel_width: 400,
        pixel_height: 160,
        dpi: 96,
    };
    let (input, commands) = mpsc::unbounded_channel();
    let (writer, transport) = mpsc::unbounded_channel();
    let terminal = terminal::create(size, writer.clone());
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
            prompts: Default::default(),
            events: Default::default(),
            player: Default::default(),
            notices,
            live_frames: FrameStore::new(initial),
            sharing: Default::default(),
        },
    );
    let registry = Registry::default();
    registry.replace([Pane {
        id: 1,
        tab_id: 5,
        tab_title: None,
        label: "test".into(),
        active: true,
        visible: true,
        read_only: false,
        input: input.clone(),
    }]);

    let exercise = async {
        let old = feed(&input, "old HUD").await;
        for piece in ["\x1b[?20", "26h", "\rnew HUD", "\x1b[2;1Hnew picture"] {
            let partial = feed(&input, piece).await;
            assert_eq!(partial.generation, old.generation);
            assert_eq!(text(&partial), text(&old));
            assert_eq!(text(&frames.load()), text(&old));
        }
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
        assert_eq!(screen["generation"], old.generation);
        // The callback inspects the snapshot without writing a PNG. No
        // temporary directory or filesystem access is needed for this capture.
        let image = registry
            .execute(
                Request::Screenshot {
                    pane: 1,
                    path: std::env::temp_dir().join("unused.png"),
                },
                screenshot.clone(),
            )
            .await
            .unwrap();
        assert_eq!(image["generation"], old.generation);
        let waiting = registry.execute(
            Request::WaitForText {
                pane: 1,
                pattern: "new picture".into(),
                regex: false,
                timeout_seconds: 1,
            },
            screenshot,
        );
        tokio::pin!(waiting);
        assert_pending(waiting.as_mut()).await; // queues Command::Frames
        capture(&input).await; // FIFO barrier: the worker has replied to Frames
        assert_pending(waiting.as_mut()).await; // matcher has checked the held frame
        let done = feed(&input, "\x1b[?2026l").await;
        let found = waiting.await.unwrap();
        assert!(text(&done).contains("new HUD\nnew picture"));
        assert_eq!(done.generation, old.generation + 1);
        assert_eq!(found["generation"], done.generation);
        input.send(Command::Shutdown).unwrap();
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(worker, exercise)
    })
    .await
    .unwrap();
}
