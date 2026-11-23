use crate::{
    options::{ColorScheme, Source},
    recorder::Recorder,
    session::{
        Command, Session,
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
    let cell = frame.rows[1].get_cell(0).unwrap();
    let tiles = cell.attrs().images().unwrap();
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].image_id(), Some(7));
    match frame.image_data(tiles[0].image_data()) {
        wezterm_term::image::ImageDataType::Rgba8 { data, .. } => data.clone(),
        _ => panic!("expected a decoded 1x1 kitty image"),
    }
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
