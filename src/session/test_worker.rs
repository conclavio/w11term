use super::*;
use tokio::sync::oneshot;

pub(super) async fn capture(input: &mpsc::UnboundedSender<Command>) -> Snapshot {
    let (send, receive) = oneshot::channel();
    input.send(Command::Capture(send)).unwrap();
    receive.await.unwrap().0
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
