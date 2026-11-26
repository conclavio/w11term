use std::sync::Arc;
use w11term::{
    control::{Pane, Registry, Request},
    options::Source,
    session::{Command, Session},
};

#[tokio::test(start_paused = true)]
async fn scrollback_search_reports_every_window_and_distinguishes_an_unavailable_pane() {
    let registry = Registry::window(10, None);
    let second_window = registry.register_window(20, None).unwrap();
    let (stalled, mut pending) = tokio::sync::mpsc::unbounded_channel();
    let (ready, mut requests) = tokio::sync::mpsc::unbounded_channel();
    let (closed, receiver) = tokio::sync::mpsc::unbounded_channel();
    drop(receiver);
    let pane = |id, input| Pane {
        id,
        tab_id: id + 100,
        tab_title: None,
        pane_name: None,
        label: format!("pane {id}"),
        active: false,
        visible: false,
        read_only: true,
        capture: None,
        input,
    };
    // Occupy every scan slot. Their timeouts must leave room for the
    // healthy pane in the second window before the overall deadline.
    registry.replace([
        pane(1, stalled.clone()),
        pane(2, stalled.clone()),
        pane(3, stalled.clone()),
        pane(4, stalled),
        pane(7, closed),
    ]);
    second_window.replace([pane(5, ready)]);
    let worker = tokio::spawn(async move {
        let Some(Command::SearchPreview { query, reply }) = requests.recv().await else {
            panic!("expected a read-only search")
        };
        assert_eq!(query.text(), "missing");
        reply.send(Default::default()).unwrap();
    });
    let capture: w11term::control::CapturePng =
        Arc::new(|_, _, _| anyhow::bail!("unexpected capture"));
    let request = |pane, all_panes| Request::FindText {
        pattern: "missing".into(),
        pane,
        all_panes,
        regex: false,
        match_case: false,
    };
    let result = registry
        .execute(request(None, true), capture.clone())
        .await
        .unwrap();
    worker.await.unwrap();
    let rows = result["panes"].as_array().unwrap();
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0]["pane"], 1);
    assert_eq!(rows[0]["window_id"], 10);
    assert!(rows[0]["error"].as_str().unwrap().contains("timed out"));
    assert!(rows[0].get("count").is_none());
    let Some(Command::SearchPreview { reply, .. }) = pending.recv().await else {
        panic!("missing stalled request")
    };
    assert!(reply.is_closed(), "timed out scan must see cancellation");
    assert_eq!(
        rows[4],
        serde_json::json!({"pane":5,"tab_id":105,"window_id":20,"count":0,"truncated":false,"first":null})
    );
    assert_eq!(rows[5]["pane"], 7);
    assert!(rows[5]["error"].as_str().unwrap().contains("closed"));
    for (filter, all, expected) in [
        (Some(7), false, "closed"),
        (Some(99), false, "unknown pane"),
        (None, false, "select"),
        (Some(4), true, "select"),
    ] {
        let error = registry
            .execute(request(filter, all), capture.clone())
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scrollback_regex_is_case_configurable_and_reports_incomplete_results() {
    let state = tempfile::tempdir().unwrap();
    let path = state.path().join("regex.vt");
    std::fs::write(
        &path,
        format!("{}\r\nApp-78\r\napp-79\r\nsearch-ready", "z".repeat(66_000)),
    )
    .unwrap();
    let session = Session::start(Source::Replay(path));
    let registry = Registry::default();
    registry.replace([Pane {
        id: 1,
        tab_id: 10,
        tab_title: None,
        pane_name: None,
        label: "replay".into(),
        active: false,
        visible: false,
        read_only: true,
        capture: None,
        input: session.input.clone(),
    }]);
    let capture: w11term::control::CapturePng = Arc::new(|_, _, _| anyhow::bail!("unused"));
    registry
        .execute(
            Request::WaitForText {
                pane: 1,
                pattern: "search-ready".into(),
                regex: false,
                connection_generation: None,
                after_frame: None,
                screenshot_path: None,
                timeout_seconds: 5,
            },
            capture.clone(),
        )
        .await
        .unwrap();
    let find = |pattern: &str, regex, match_case| {
        registry.execute(
            Request::FindText {
                pane: Some(1),
                all_panes: false,
                pattern: pattern.into(),
                regex,
                match_case,
            },
            capture.clone(),
        )
    };
    let insensitive = find(r"App-\d+", true, false).await.unwrap();
    assert_eq!(insensitive["panes"][0]["count"], 2);
    assert_eq!(insensitive["panes"][0]["incomplete"], true);
    let sensitive = find(r"App-\d+", true, true).await.unwrap();
    assert_eq!(sensitive["panes"][0]["count"], 1);
    assert_eq!(sensitive["panes"][0]["first"]["excerpt"], "App-78");
    let literal = find("APP-78", false, true).await.unwrap();
    assert_eq!(literal["panes"][0]["count"], 0);
    assert!(literal["panes"][0].get("incomplete").is_none());
    let invalid = find("[", true, false).await.unwrap_err();
    assert!(invalid.to_string().contains("invalid regular expression"));
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_is_event_driven_matches_live_screen_during_scrollback_and_times_out() {
    let session = Session::start(Source::Demo);
    let registry = Registry::default();
    registry.replace([Pane {
        id: 1,
        tab_id: 10,
        tab_title: None,
        pane_name: None,
        label: "lab".into(),
        active: false,
        visible: false,
        read_only: false,
        capture: None,
        input: session.input.clone(),
    }]);
    let capture: w11term::control::CapturePng = Arc::new(|_, _, _| anyhow::bail!("unused"));
    let wait = |pattern: &str, regex| {
        registry.execute(
            Request::WaitForText {
                pane: 1,
                pattern: pattern.into(),
                regex,
                connection_generation: None,
                after_frame: None,
                screenshot_path: None,
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
        tab_id: 10,
        tab_title: None,
        pane_name: None,
        label: "lab".into(),
        active: false,
        visible: false,
        read_only: false,
        capture: None,
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
                    connection_generation: None,
                    after_frame: None,
                    screenshot_path: None,
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
            connection_generation: None,
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
        tab_id: 3,
        tab_title: None,
        pane_name: Some("Deploy".into()),
        label: "failed".into(),
        active: false,
        visible: true,
        read_only: false,
        capture: None,
        input,
    }]);
    let result = registry
        .execute(Request::List, Arc::new(|_, _, _| anyhow::bail!("unused")))
        .await
        .unwrap();
    assert_eq!(result["sessions"][0]["id"], 17);
    assert_eq!(result["sessions"][0]["tab_id"], 3);
    assert_eq!(result["sessions"][0]["pane_name"], "Deploy");
    assert_eq!(result["sessions"][0]["label"], "failed");
    assert!(result["sessions"][0].get("tab_title").is_none());
    assert_eq!(result["sessions"][0]["active"], false);
    assert_eq!(result["sessions"][0]["visible"], true);
    assert_eq!(result["sessions"][0]["status"], "unavailable");
    assert!(
        result["sessions"][0]["error"]
            .as_str()
            .unwrap()
            .contains("closed")
    );
}
