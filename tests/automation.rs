use std::sync::Arc;
use w11term::{
    control::{Pane, Registry, Request},
    options::Source,
    session::{Command, Session},
};
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_is_event_driven_matches_live_screen_during_scrollback_and_times_out() {
    let session = Session::start(Source::Demo);
    let registry = Registry::default();
    registry.replace([Pane {
        id: 1,
        label: "lab".into(),
        active: false,
        input: session.input.clone(),
    }]);
    let capture: w11term::control::CapturePng = Arc::new(|_, _, _| anyhow::bail!("unused"));
    let wait = |pattern: &str, regex| {
        registry.execute(
            Request::WaitForText {
                pane: 1,
                pattern: pattern.into(),
                regex,
                timeout_seconds: 1,
            },
            capture.clone(),
        )
    };
    session.input.send(Command::Text("ready".into())).unwrap();
    wait("ready", false).await.unwrap();
    let generation = session.frames.load().generation;
    assert!(
        wait("no such text", false)
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    assert!(
        session.frames.load().generation <= generation + 2,
        "wait-for-text caused polling captures"
    );
    session
        .input
        .send(Command::Text(
            (0..40).map(|n| format!("line{n}\r")).collect(),
        ))
        .unwrap();
    session.input.send(Command::Scroll(1000)).unwrap();
    session.input.send(Command::Text("fps=144".into())).unwrap();
    session.input.send(Command::Scroll(1000)).unwrap();
    let result = wait(r"fps=\d+", true).await.unwrap();
    assert!(result["text"].as_str().unwrap().contains("fps=144"));
    assert!(
        wait("[", true)
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid regular expression")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saturated_long_polls_return_busy_without_blocking_input_or_capture() {
    let _ = env_logger::try_init();
    let session = Session::start(Source::Demo);
    let registry = Registry::default();
    registry.replace([Pane {
        id: 1,
        label: "lab".into(),
        active: false,
        input: session.input.clone(),
    }]);
    let discovery = tempfile::tempdir().unwrap();
    let _server = w11term::control::Server::start_in_directory(
        registry,
        |_, _, _| anyhow::bail!("unused"),
        discovery.path().into(),
    )
    .unwrap();
    let pid = Some(std::process::id());
    let (replies, mut results) = tokio::sync::mpsc::unbounded_channel();
    for _ in 0..17 {
        let replies = replies.clone();
        tokio::spawn(async move {
            let result = w11term::control::call(
                pid,
                &Request::WaitForText {
                    pane: 1,
                    pattern: "capacity-marker".into(),
                    regex: false,
                    timeout_seconds: 10,
                },
            )
            .await
            .unwrap();
            replies.send(result).unwrap();
        });
    }
    let busy = tokio::time::timeout(std::time::Duration::from_secs(3), results.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(!busy.ok && busy.error.unwrap().starts_with("busy:"));
    assert!(
        w11term::control::call(pid, &Request::List)
            .await
            .unwrap()
            .ok
    );
    assert!(
        w11term::control::call(pid, &Request::GetText { pane: 1 })
            .await
            .unwrap()
            .ok
    );
    use base64::Engine;
    let input_reply = w11term::control::call(
        pid,
        &Request::SendText {
            pane: 1,
            data_base64: base64::engine::general_purpose::STANDARD.encode(b"capacity-marker"),
            no_paste: true,
        },
    )
    .await
    .unwrap();
    assert!(!input_reply.ok && !input_reply.error.unwrap().starts_with("busy:")); // Demo has no SSH channel.
    session
        .input
        .send(Command::Text("capacity-marker".into()))
        .unwrap();
    for _ in 0..16 {
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), results.recv())
                .await
                .unwrap()
                .unwrap()
                .ok
        );
    }
}

#[tokio::test]
async fn list_keeps_closed_sessions_with_an_explicit_error() {
    let (input, receiver) = tokio::sync::mpsc::unbounded_channel();
    drop(receiver);
    let registry = Registry::default();
    registry.replace([Pane {
        id: 17,
        label: "failed".into(),
        active: false,
        input,
    }]);
    let result = registry
        .execute(Request::List, Arc::new(|_, _, _| anyhow::bail!("unused")))
        .await
        .unwrap();
    assert_eq!(result["sessions"][0]["id"], 17);
    assert_eq!(result["sessions"][0]["status"], "unavailable");
    assert!(
        result["sessions"][0]["error"]
            .as_str()
            .unwrap()
            .contains("closed")
    );
}
