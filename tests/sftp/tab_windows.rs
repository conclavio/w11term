//! Same-process WinUI handoff, using an actual SSH channel and two local ConPTY children.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "moves a live split tab between native HWNDs and closes the source window first"]
async fn native_move_live_split_to_window_then_close_origin() {
    run("Origin", b"ssh-probe/origin-survived/only-tab/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "moves a live split tab between native HWNDs and closes the destination window first"]
async fn native_move_live_split_to_window_then_close_destination() {
    run("Destination", b"ssh-probe/").await;
}

async fn run(first_close: &str, expected_wire: &[u8]) {
    let transports = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, sessions, server) =
        fixture_counting_accepts(false, 2, transports.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let data = state.path().join("data");
    fs::create_dir(&data).unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{"window-ssh": ssh}})).unwrap(),
    )
    .unwrap();
    fs::write(
        state.path().join("appearance.json"),
        br#"{"shortcuts":{"command_palette":"F7","focus_right":"F8","focus_left":"F9"}}"#,
    )
    .unwrap();
    let local = |name| {
        json!({"kind":"local","value":{"name":name,
        "shell":{"executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}}})
    };
    let leaf = |slot, source: Value| json!({"type":"leaf","slot":slot,"source":source});
    let workspace = json!({"active_tab":0,"tabs":[
        {"title":"Cockpit tools","focused":1,"layout":{"type":"split","axis":"columns","ratio":0.375,
            "first":leaf(1,json!({"kind":"saved_profile","value":"window-ssh"})),
            "second":leaf(2,local("Moved PowerShell"))}},
        {"focused":3,"layout":leaf(3,local("Source PowerShell"))}
    ]});
    fs::write(
        data.join("workspaces.json"),
        serde_json::to_vec(&json!({"version":1,"workspaces":{"two-windows":workspace}})).unwrap(),
    )
    .unwrap();
    let result = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-File"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/tab-windows-ui.ps1"))
        .args([
            "-Gui",
            env!("CARGO_BIN_EXE_w11term"),
            "-Cli",
            env!("CARGO_BIN_EXE_w11term-cli"),
            "-FirstClose",
            first_close,
        ])
        .arg("-State")
        .arg(state.path())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
        fs::read_to_string(state.path().join("gui.log")).unwrap_or_default()
    );
    assert_eq!(
        transports.load(Ordering::Relaxed),
        1,
        "SSH transport restarted"
    );
    assert_eq!(sessions.load(Ordering::Relaxed), 1, "SSH channel restarted");
    assert_eq!(
        fs::read(trace).unwrap(),
        expected_wire,
        "window-scoped keyboard input"
    );
    server.abort();
}
