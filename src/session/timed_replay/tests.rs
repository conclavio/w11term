use crate::{
    options::{ColorScheme, Source},
    recorder::Recorder,
    session::{
        Command, ReplayAction, ReplayState, ReplayStatus, Session,
        colors::PaneColors,
        test_worker::{capture, with_worker},
    },
    terminal::Snapshot,
};
use std::time::{Duration, Instant};
use tokio::{
    sync::{mpsc, oneshot},
    time::advance,
};
use wezterm_term::TerminalSize;

fn original_size() -> TerminalSize {
    TerminalSize {
        rows: 6,
        cols: 16,
        pixel_width: 160,
        pixel_height: 96,
        dpi: 96,
    }
}

// I/O uses a real thread; exchanging requests keeps the virtual clock paused
// while that thread finishes preflight and fills its bounded channel.
async fn wait_for(
    input: &mpsc::UnboundedSender<Command>,
    condition: impl Fn(&Snapshot) -> bool,
) -> Snapshot {
    let limit = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = capture(input).await;
        if condition(&frame) {
            return frame;
        }
        assert!(
            Instant::now() < limit,
            "timed replay stopped at {:?}, {} bytes",
            frame.status,
            frame.bytes_received
        );
        tokio::task::yield_now().await;
    }
}

fn pixel(frame: &Snapshot) -> Vec<u8> {
    image_pixel(frame, 1, 7)
}
fn image_pixel(frame: &Snapshot, row: usize, id: u32) -> Vec<u8> {
    let cell = frame.rows[row].get_cell(0).unwrap();
    let tiles = cell.attrs().images().unwrap();
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].image_id(), Some(id));
    match frame.image_data(tiles[0].image_data()) {
        wezterm_term::image::ImageDataType::Rgba8 { data, .. } => data.clone(),
        _ => panic!("expected a decoded 1x1 kitty image"),
    }
}

async fn control(
    input: &mpsc::UnboundedSender<Command>,
    action: ReplayAction,
) -> Result<ReplayStatus, String> {
    let (reply, mut receive) = oneshot::channel();
    input.send(Command::Replay { action, reply }).unwrap();
    let limit = Instant::now() + Duration::from_secs(5);
    loop {
        match receive.try_recv() {
            Ok(result) => return result,
            Err(oneshot::error::TryRecvError::Closed) => panic!("worker closed during {action:?}"),
            Err(oneshot::error::TryRecvError::Empty) => {
                assert!(Instant::now() < limit, "worker did not finish {action:?}");
                tokio::task::yield_now().await;
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_late_read_keeps_record_order_and_pause_acknowledges_the_due_group() {
    use super::Update;
    use crate::recorder::ReplayEvent;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("late.vt");
    let mut record = Recorder::start_timed(&path, original_size()).unwrap();
    for byte in b"ABC" {
        advance(Duration::from_millis(20)).await;
        record.write(&[*byte]).unwrap();
    }
    advance(Duration::from_millis(20)).await;
    record.finish().unwrap();
    let mut playback = super::Playback::open(path, false);
    assert!(matches!(
        playback.next(None).await,
        Update::Event(_, ReplayEvent::Resize(_))
    ));
    // No event is serviced until three capture timestamps have elapsed.
    advance(Duration::from_millis(70)).await;
    let wall = tokio::time::Instant::now();
    let Update::Event(first, ReplayEvent::Output(a)) = playback.next(None).await else {
        panic!("expected first output")
    };
    assert_eq!(a, b"A");
    assert_eq!(wall, tokio::time::Instant::now());
    let (tx, mut result) = oneshot::channel();
    playback.control(ReplayAction::Pause, tx);
    playback.published();
    assert!(matches!(
        result.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    let Update::Event(second, ReplayEvent::Output(b)) = playback.next(None).await else {
        panic!("expected second output")
    };
    assert_eq!(b, b"B");
    assert_eq!(second - first, Duration::from_millis(20));
    let Update::Event(third, ReplayEvent::Output(c)) = playback.next(None).await else {
        panic!("expected third output")
    };
    assert_eq!(c, b"C");
    assert_eq!(third - second, Duration::from_millis(20));
    let Update::Clock(paused) = playback.next(None).await else {
        panic!("expected pause in silence")
    };
    assert_eq!(paused - third, Duration::from_millis(10));
    playback.published();
    let status = result.await.unwrap().unwrap();
    assert_eq!(
        (status.state, status.position_us),
        (ReplayState::Paused, 70_000)
    );
}

#[tokio::test(start_paused = true)]
async fn paused_launch_steps_whole_timestamps_then_freezes_sync_and_trailing_silence() {
    use ReplayAction::*;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("frames.vt");
    let initial = original_size();
    let resized = TerminalSize {
        cols: 20,
        pixel_width: 200,
        ..initial
    };
    let mut record = Recorder::start_timed(&path, initial).unwrap();
    record.write(b"A").unwrap();
    record.resize(resized).unwrap();
    record.write(b"B").unwrap();
    advance(Duration::from_millis(20)).await;
    record.write(b"\x1b[?2026hC").unwrap();
    advance(Duration::from_millis(200)).await;
    record.write(b"D").unwrap();
    advance(Duration::from_millis(80)).await;
    record.finish().unwrap();

    with_worker(
        Source::PausedReplay(path),
        TerminalSize::default(),
        PaneColors::new(ColorScheme::Default, crate::windows_appearance::subscribe),
        None,
        async |input, _| {
            wait_for(&input, |f| f.status.starts_with("Timed replay paused")).await;
            let initial_status = control(&input, Status).await.unwrap();
            assert_eq!(initial_status.position_us, 0);
            assert_eq!(initial_status.duration_us, Some(300_000));
            assert_eq!(capture(&input).await.bytes_received, 0);
            let stepped = control(&input, Step).await.unwrap();
            assert_eq!(
                (stepped.state, stepped.position_us),
                (ReplayState::Paused, 0)
            );
            let first = capture(&input).await;
            assert_eq!(first.rows[0].as_str().trim_end(), "AB");
            assert_eq!(first.size, resized);
            assert_eq!(control(&input, Step).await.unwrap().position_us, 20_000);
            assert_eq!(capture(&input).await.rows[0].as_str().trim_end(), "AB");
            advance(Duration::from_millis(500)).await;
            assert_eq!(control(&input, Status).await.unwrap().position_us, 20_000);
            assert_eq!(capture(&input).await.rows[0].as_str().trim_end(), "AB");
            assert_eq!(
                control(&input, Resume).await.unwrap().state,
                ReplayState::Playing
            );
            advance(Duration::from_millis(100)).await;
            assert_eq!(control(&input, Pause).await.unwrap().position_us, 120_000);
            advance(Duration::from_millis(500)).await;
            assert_eq!(capture(&input).await.rows[0].as_str().trim_end(), "AB");
            assert_eq!(control(&input, Step).await.unwrap().position_us, 220_000);
            // The 170 ms watchdog deadline precedes D's 220 ms output.
            assert_eq!(capture(&input).await.rows[0].as_str().trim_end(), "ABCD");
            let end = control(&input, Step).await.unwrap();
            assert_eq!(
                (end.state, end.position_us),
                (ReplayState::Complete, 300_000)
            );
            for action in [Pause, Resume, Step] {
                assert!(control(&input, action).await.is_err());
            }
            assert_eq!(control(&input, Status).await.unwrap(), end);
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn kitty_animations_follow_the_media_clock_and_freeze_on_the_footer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("animation.vt");
    let mut record = Recorder::start_timed(&path, original_size()).unwrap();
    record
        .write(include_bytes!("../../../tests/fixtures/kitty/animation.vt"))
        .unwrap();
    advance(Duration::from_millis(30)).await;
    record.write(b"\x1b[2;1Hsecond").unwrap();
    advance(Duration::from_millis(45)).await;
    record.finish().unwrap();
    with_worker(
        Source::PausedReplay(path),
        TerminalSize::default(),
        PaneColors::new(ColorScheme::Default, crate::windows_appearance::subscribe),
        None,
        async |input, _| {
            wait_for(&input, |f| f.status.starts_with("Timed replay paused")).await;
            let first = control(&input, ReplayAction::Step).await.unwrap();
            assert_eq!(first.position_us, 0);
            assert_eq!(image_pixel(&capture(&input).await, 0, 8), [255, 0, 0, 255]);
            advance(Duration::from_millis(300)).await;
            assert_eq!(image_pixel(&capture(&input).await, 0, 8), [255, 0, 0, 255]);
            let second = control(&input, ReplayAction::Step).await.unwrap();
            assert_eq!(second.position_us, 30_000);
            assert_eq!(image_pixel(&capture(&input).await, 0, 8), [0, 255, 0, 255]);
            let footer = control(&input, ReplayAction::Step).await.unwrap();
            assert_eq!(footer.state, ReplayState::Complete);
            assert_eq!(image_pixel(&capture(&input).await, 0, 8), [0, 0, 255, 255]);
            advance(Duration::from_millis(300)).await;
            assert_eq!(image_pixel(&capture(&input).await, 0, 8), [0, 0, 255, 255]);
        },
    )
    .await;
}

#[tokio::test]
async fn loading_controls_are_bounded_and_invalid_attachments_remain_inspectable() {
    let dir = tempfile::tempdir().unwrap();
    let mut reader = super::Playback::open(dir.path().join("missing.vt"), false);
    let (reply, receive) = oneshot::channel();
    reader.control(ReplayAction::Pause, reply);
    reader.published();
    let loading = receive.await.unwrap().unwrap();
    assert_eq!(
        (loading.state, loading.duration_us),
        (ReplayState::Loading, None)
    );
    let (step_reply, step) = oneshot::channel();
    reader.control(ReplayAction::Step, step_reply);
    let (busy_reply, busy) = oneshot::channel();
    reader.control(ReplayAction::Step, busy_reply);
    assert!(busy.await.unwrap().is_err());
    assert!(matches!(reader.next(None).await, super::Update::State));
    assert!(step.await.unwrap().is_err());
    let (reply, receive) = oneshot::channel();
    reader.control(ReplayAction::Status, reply);
    let failed = receive.await.unwrap().unwrap();
    assert_eq!(failed.state, ReplayState::Failed);
    assert!(failed.error.is_some());
    let pane = Session::start(Source::Demo);
    assert!(control(&pane.input, ReplayAction::Status).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn plays_intermediate_image_frames_and_resizes_at_recorded_times() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cockpit.vt");
    let initial = original_size();
    let resized = TerminalSize {
        rows: 7,
        cols: 20,
        pixel_width: 220,
        pixel_height: 140,
        dpi: 120,
    };
    let zoomed = TerminalSize {
        pixel_width: 300,
        pixel_height: 161,
        dpi: 144,
        ..resized
    };
    let mut record = Recorder::start_timed(&path, initial).unwrap();
    record
        .write(b"FIRST\x1b[2;1H\x1b_Ga=T,i=7,p=1,f=24,s=1,v=1,c=1,r=1,C=1;/wAA\x1b\\")
        .unwrap();
    advance(Duration::from_millis(25)).await;
    record.resize(resized).unwrap();
    advance(Duration::from_millis(15)).await;
    record.resize(zoomed).unwrap();
    advance(Duration::from_millis(10)).await;
    record
        .write(b"\x1b[HFINAL\x1b[2;1H\x1b_Ga=T,i=7,p=1,f=24,s=1,v=1,c=1,r=1,C=1;AAD/\x1b\\")
        .unwrap();
    advance(Duration::from_millis(100)).await;
    record.finish().unwrap();

    with_worker(
        Source::TimedReplay(path),
        TerminalSize::default(),
        PaneColors::new(ColorScheme::Default, crate::windows_appearance::subscribe),
        None,
        async |input, _| {
            let red = wait_for(&input, |frame| frame.rows[0].as_str().starts_with("FIRST")).await;
            assert_eq!(red.size, initial);
            assert_eq!(pixel(&red), [255, 0, 0, 255]);
            input
                .send(Command::Resize(TerminalSize::default()))
                .unwrap();
            advance(Duration::from_millis(24)).await;
            assert_eq!(capture(&input).await.size, initial);
            advance(Duration::from_millis(1)).await;
            wait_for(&input, |frame| frame.size == resized).await;
            advance(Duration::from_millis(15)).await;
            let middle = wait_for(&input, |frame| frame.size == zoomed).await;
            assert!(middle.rows[0].as_str().starts_with("FIRST"));
            advance(Duration::from_millis(10)).await;
            let blue = wait_for(&input, |frame| frame.rows[0].as_str().starts_with("FINAL")).await;
            assert_eq!(pixel(&blue), [0, 0, 255, 255]);
            assert_eq!(pixel(&red), [255, 0, 0, 255]);
            advance(Duration::from_millis(99)).await;
            assert!(capture(&input).await.status.starts_with("Timed replay ·"));
            advance(Duration::from_millis(1)).await;
            let done = wait_for(&input, |frame| {
                frame.status.starts_with("Timed replay complete")
            })
            .await;
            assert_eq!(done.size, zoomed);
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_pause_in_output_runs_the_synchronized_presentation_watchdog() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("held.vt");
    let initial = original_size();
    let mut record = Recorder::start_timed(&path, initial).unwrap();
    record.write(b"A\x1b[?2026hB").unwrap();
    advance(Duration::from_millis(200)).await;
    record.write(b"\x1b[?2026lC").unwrap();
    record.finish().unwrap();
    with_worker(
        Source::TimedReplay(path),
        TerminalSize::default(),
        PaneColors::new(ColorScheme::Default, crate::windows_appearance::subscribe),
        None,
        async |input, _| {
            wait_for(&input, |f| f.rows[0].as_str().trim_end() == "A").await;
            let (reply, result) = oneshot::channel();
            input.send(Command::ReadModes(reply)).unwrap();
            assert!(result.await.unwrap().1.synchronized);
            advance(Duration::from_millis(149)).await;
            assert_eq!(capture(&input).await.rows[0].as_str().trim_end(), "A");
            advance(Duration::from_millis(1)).await;
            wait_for(&input, |f| f.rows[0].as_str().trim_end() == "AB").await;
            advance(Duration::from_millis(50)).await;
            wait_for(&input, |f| f.rows[0].as_str().trim_end() == "ABC").await;
        },
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn silent_capture_remains_open_for_its_recorded_duration() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("silent.vt");
    let initial = original_size();
    let record = Recorder::start_timed(&path, initial).unwrap();
    advance(Duration::from_millis(300)).await;
    record.finish().unwrap();
    with_worker(
        Source::TimedReplay(path),
        TerminalSize::default(),
        PaneColors::new(ColorScheme::Default, crate::windows_appearance::subscribe),
        None,
        async |input, _| {
            wait_for(&input, |f| f.size == initial).await;
            advance(Duration::from_millis(299)).await;
            assert!(capture(&input).await.status.starts_with("Timed replay ·"));
            advance(Duration::from_millis(1)).await;
            wait_for(&input, |f| f.status.starts_with("Timed replay complete")).await;
        },
    )
    .await;
}

#[tokio::test]
async fn timed_replay_exposes_titles_without_enabling_desktop_actions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("alerts.vt");
    let mut record = Recorder::start_timed(&path, original_size()).unwrap();
    record
        .write(b"\x1b]0;Cockpit\x07\x07\x1b]9;read me\x07\x1b]440;1;play;shot\x07")
        .unwrap();
    record.finish().unwrap();
    let pane = Session::start(Source::TimedReplay(path));
    let done = wait_for(&pane.input, |f| {
        f.status.starts_with("Timed replay complete")
    })
    .await;
    assert_eq!(done.title, "Cockpit");
    assert!(
        !pane
            .clipboard_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    let events = pane.events.since(0).events;
    assert!(events.iter().any(|event| event.kind == "title"));
    assert!(!events.iter().any(|event| matches!(
        event.kind.as_str(),
        "bell" | "sound" | "notification" | "assets-syncing"
    )));
}

#[test]
fn rejected_starts_and_failed_finishes_have_distinct_recording_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("record.vt");
    let size = original_size();
    let events = crate::events::Events::default();
    let mut recording = None;
    let change = super::super::change_recording;
    assert!(
        change(
            &mut recording,
            &events,
            Some(path.clone()),
            size,
            false,
            true
        )
        .is_err()
    );
    assert!(events.since(0).events.is_empty());
    assert!(!path.exists());
    change(
        &mut recording,
        &events,
        Some(path.clone()),
        size,
        true,
        true,
    )
    .unwrap();
    assert!(change(&mut recording, &events, Some(path), size, true, true).is_err());
    assert!(
        recording
            .as_mut()
            .unwrap()
            .resize(TerminalSize {
                cols: usize::MAX,
                ..size
            })
            .is_err()
    );
    assert!(change(&mut recording, &events, None, size, true, false).is_err());
    assert!(recording.is_none());
    let entries = events.since(0).events;
    assert_eq!(
        entries.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
        ["recording-started", "recording-error"]
    );
}
