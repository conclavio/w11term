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
