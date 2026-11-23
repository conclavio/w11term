use super::*;
use std::time::Duration;

#[test]
fn native_recording_requires_a_live_shell_or_attached_durable_session() {
    use ssh::DurablePhase::*;
    assert!(can_start_native_recording(true, Phase::Running, None));
    assert!(can_start_native_recording(
        true,
        Phase::Restarting { reset: true },
        None
    ));
    for durable in [Attached, Direct] {
        assert!(can_start_native_recording(
            true,
            Phase::Running,
            Some(durable)
        ));
    }
    for durable in [
        Discovering,
        Choosing,
        Connecting,
        Detached,
        Reconnecting,
        Missing,
        Unavailable,
        Ended,
        Failed,
    ] {
        assert!(!can_start_native_recording(
            true,
            Phase::Running,
            Some(durable)
        ));
    }
    assert!(!can_start_native_recording(false, Phase::Running, None));
    for phase in [Phase::PublishingExit, Phase::Exited] {
        assert!(!can_start_native_recording(true, phase, None));
    }
}

#[tokio::test]
async fn stale_stop_cannot_finish_a_replacement_recording_on_the_same_worker() {
    type Reply = Result<serde_json::Value, String>;
    async fn request(
        session: &Session,
        command: impl FnOnce(tokio::sync::oneshot::Sender<Reply>) -> Command,
    ) -> Reply {
        let (reply, result) = tokio::sync::oneshot::channel();
        session.input.send(command(reply)).unwrap();
        tokio::time::timeout(Duration::from_secs(5), result)
            .await
            .unwrap()
            .unwrap()
    }
    let directory = tempfile::tempdir().unwrap();
    let session = Session::start(Source::Demo);
    let record = |path| {
        move |reply| Command::Record {
            path,
            timed: false,
            reply,
        }
    };
    let stop = |id| move |reply| Command::StopRecordingIfCurrent { id, reply };

    let first = request(&session, record(Some(directory.path().join("first.vt"))))
        .await
        .unwrap();
    let first = first["id"].as_u64().unwrap();
    assert_eq!(session.events.recording(), Some(first));
    let rejected = directory.path().join("unavailable.vt");
    assert!(
        request(&session, |reply| Command::StartTimedRecordingIfLive {
            path: rejected.clone(),
            reply
        })
        .await
        .is_err()
    );
    assert!(!rejected.exists());
    assert_eq!(session.events.recording(), Some(first));
    assert_eq!(
        request(&session, record(None)).await.unwrap()["recording"],
        false
    );
    let next_path = directory.path().join("second.vt");
    let second = request(&session, record(Some(next_path.clone())))
        .await
        .unwrap();
    let second = second["id"].as_u64().unwrap();
    assert!(second > first);
    assert_eq!(
        session.events.recording_path().as_deref(),
        Some(next_path.as_path())
    );
    let cursor = session.events.since(0).next;
    assert_eq!(
        request(&session, stop(first)).await.unwrap_err(),
        "recording has changed"
    );
    assert_eq!(session.events.recording(), Some(second));
    assert_eq!(session.events.since(cursor).next, cursor);
    let stopped = request(&session, stop(second)).await.unwrap();
    assert_eq!(stopped["path"], serde_json::json!(next_path));
    assert_eq!(stopped["recording"], false);
    assert_eq!(session.events.recording(), None);
    assert_eq!(
        request(&session, stop(second)).await.unwrap_err(),
        "recording has changed"
    );
}

#[test]
fn failed_start_and_recording_errors_leave_the_correct_live_state() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("data.vt");
    let events = crate::events::Events::default();
    let mut recorder = None;
    std::fs::write(&path, b"existing").unwrap();
    assert!(
        change_recording(
            &mut recorder,
            &events,
            Some(path.clone()),
            TerminalSize::default(),
            true,
            true
        )
        .is_err()
    );
    assert_eq!(events.recording(), None);
    assert!(events.since(0).events.is_empty());
    let second = directory.path().join("second.vt");
    change_recording(
        &mut recorder,
        &events,
        Some(second),
        TerminalSize::default(),
        true,
        true,
    )
    .unwrap();
    let id = events.recording().unwrap();
    assert!(
        change_recording(
            &mut recorder,
            &events,
            Some(path),
            TerminalSize::default(),
            true,
            false
        )
        .is_err()
    );
    assert_eq!(events.recording(), Some(id));
    // An invalid resize fails the real timed writer.
    update_recording(&mut recorder, &events, |active| {
        active.resize(TerminalSize {
            rows: 0,
            ..TerminalSize::default()
        })
    });
    assert!(recorder.is_none());
    assert_eq!(events.recording(), None);
    assert_eq!(events.since(id).events[0].kind, "recording-error");
}
