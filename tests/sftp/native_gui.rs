use super::*;
#[path = "../support/native_gui.rs"]
mod shared;
use shared::run_gui;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives the command palette across a loopback SSH session and a PowerShell tab"]
async fn native_command_palette_opens_powershell_and_restores_ssh_input() {
    run_palette_fixture("command-palette-ui.ps1", b"after").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "locks and unlocks input in a live SSH and PowerShell split through the native UI"]
async fn native_read_only_input_is_per_pane_in_a_mixed_split() {
    run_palette_fixture("read-only-pane-ui.ps1", b"resumed").await;
}

async fn run_palette_fixture(script: &str, expected_input: &[u8]) {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_connections(false, 1).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{
        "palette-ssh":ssh, "Palette PowerShell":{
            "backend":"local", "executable":"powershell.exe", "args":["-NoProfile","-NoLogo"]}}}))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        state.path().join("appearance.json"),
        br#"{"shortcuts":{"command_palette":"F7","select_tab_1":"F8","copy_mode":"F9"}}"#,
    )
    .unwrap();
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-File"])
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join(script),
        )
        .args([
            "-Gui",
            env!("CARGO_BIN_EXE_w11term"),
            "-Cli",
            env!("CARGO_BIN_EXE_w11term-cli"),
        ])
        .arg("-State")
        .arg(state.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(state.path().join("gui.log")).unwrap_or_default()
    );
    assert_eq!(fs::read(trace).unwrap(), expected_input);
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "moves a live SSH pane into an existing Windows PowerShell tab through the native UI"]
async fn native_move_to_existing_tab_preserves_ssh_and_powershell() {
    // Allow a replacement transport so the test can detect one without hanging.
    let transports = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, channels, server) =
        fixture_counting_accepts(false, 2, transports.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let data = state.path().join("data");
    fs::create_dir(&data).unwrap();
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{"move-ssh":ssh}})).unwrap(),
    )
    .unwrap();
    fs::write(
        state.path().join("appearance.json"),
        br#"{"shortcuts":{"move_pane_to_existing_tab":"F7","copy_mode":"F8","focus_right":"F9"}}"#,
    )
    .unwrap();
    let leaf = |slot, source: Value| json!({"focused":slot,"layout":{"type":"leaf","slot":slot,"source":source}});
    let workspace = json!({"active_tab":0,"tabs":[
        leaf(1, json!({"kind":"saved_profile","value":"move-ssh"})),
        leaf(2, json!({"kind":"local","value":{"name":"Windows PowerShell",
            "shell":{"executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}}}))
    ]});
    fs::write(
        data.join("workspaces.json"),
        serde_json::to_vec(&json!({"version":1,"workspaces":{"move":workspace}})).unwrap(),
    )
    .unwrap();
    let mut history = String::from("\x1b[2J\x1b[3J\x1b[Hmove-retained-row\r\n");
    for row in 0..180 {
        history.push_str(&format!("scroll line {row}\r\n"));
    }
    history.push_str("move-live-screen\r\n");
    fs::write(remote.path().join("copy-history.vt"), history).unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-File"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/move-pane-ui.ps1"))
        .args([
            "-Gui",
            env!("CARGO_BIN_EXE_w11term"),
            "-Cli",
            env!("CARGO_BIN_EXE_w11term-cli"),
        ])
        .arg("-State")
        .arg(state.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(state.path().join("gui.log")).unwrap_or_default()
    );
    assert_eq!(transports.load(Ordering::Relaxed), 1, "SSH TCP accepts");
    assert_eq!(channels.load(Ordering::Relaxed), 1, "SSH session opens");
    assert_eq!(fs::read(trace).unwrap(), b"typed-ssh/cli-ssh");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives keyboard copy mode against a loopback SSH peer"]
async fn native_keyboard_copy_reads_scrollback_and_keeps_mode_keys_off_the_wire() {
    let (Source::Ssh(profile), remote, _, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    let mut history = String::from("\x1b[2J\x1b[3J\x1b[H\x1b[32mhistory-start\x1b[0m\r\n");
    for row in 0..180 {
        history.push_str(&format!("fixture line {row}\r\n"));
    }
    history.push_str("history-end\x1b[>3u");
    fs::write(remote.path().join("copy-history.vt"), history).unwrap();
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{"copy-fixture":profile}})).unwrap(),
    )
    .unwrap();
    fs::write(
        state.path().join("appearance.json"),
        br#"{"shortcuts":{"copy_mode":"F8"}}"#,
    )
    .unwrap();
    let result = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-File"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/keyboard-copy-ui.ps1"))
        .args([
            "-Gui",
            env!("CARGO_BIN_EXE_w11term"),
            "-Cli",
            env!("CARGO_BIN_EXE_w11term-cli"),
        ])
        .arg("-State")
        .arg(state.path())
        .arg("-Trace")
        .arg(&trace)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
        fs::read_to_string(state.path().join("gui.log")).unwrap_or_default()
    );
    assert_eq!(fs::read(trace).unwrap(), b"!");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "saves a startup command in the native workspace editor and reopens it over SSH"]
async fn native_workspace_startup_is_sent_only_by_the_reopened_pane() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let trace = remote.path().join("workspace-exec.bin");
    fs::write(&trace, []).unwrap();
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{"fixture":ssh}})).unwrap(),
    )
    .unwrap();
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-File"])
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/workspace-startup-ui.ps1"),
        )
        .args([
            "-Gui",
            env!("CARGO_BIN_EXE_w11term"),
            "-Cli",
            env!("CARGO_BIN_EXE_w11term-cli"),
        ])
        .arg("-State")
        .arg(state.path())
        .arg("-Trace")
        .arg(&trace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(state.path().join("gui.log")).unwrap_or_default()
    );
    assert_eq!(opens.load(Ordering::Relaxed), 2);
    assert_eq!(
        fs::read_to_string(trace).unwrap(),
        r#"sh -c 'cd "$1" && exec "${SHELL:-/bin/sh}" -c "$2"' w11term '/srv/crew'\''s project' 'tail -f build.log'"#
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "edits keyboard settings and restarts a native SSH and Windows PowerShell workspace"]
async fn native_shortcut_rebind_releases_previous_key_and_persists_after_restart() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    fs::write(
        state.path().join("appearance.json"),
        br#"{"shortcuts":{"resize_right":"F9","resize_left":"F10"}}"#,
    )
    .unwrap();
    let data = state.path().join("data");
    fs::create_dir(&data).unwrap();
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{"fixture-ssh":ssh}})).unwrap(),
    )
    .unwrap();
    let workspace = json!({"active_tab":0, "tabs":[{"focused":1, "layout":{
        "type":"split", "axis":"columns", "ratio":0.5,
        "first":{"type":"leaf","slot":1,"source":{"kind":"saved_profile","value":"fixture-ssh"}},
        "second":{"type":"leaf","slot":2,"source":{"kind":"local","value":{"name":"Windows PowerShell",
            "shell":{"executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}}}}
    }}]});
    fs::write(
        data.join("workspaces.json"),
        serde_json::to_vec(&json!({"version":1,"workspaces":{"shortcuts":workspace}})).unwrap(),
    )
    .unwrap();
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-File"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/shortcuts-ui.ps1"))
        .args([
            "-Gui",
            env!("CARGO_BIN_EXE_w11term"),
            "-Cli",
            env!("CARGO_BIN_EXE_w11term-cli"),
        ])
        .arg("-State")
        .arg(state.path())
        .arg("-Trace")
        .arg(&trace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(state.path().join("gui.log")).unwrap_or_default()
    );
    // Only the released F7 reached SSH on each launch. Resize and focus
    // chords were consumed locally, and the text went to PowerShell.
    assert_eq!(fs::read(trace).unwrap(), b"\x1b[18~\x1b[18~");
    assert_eq!(opens.load(Ordering::Relaxed), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives the native pane picker with two SSH peers and Windows PowerShell"]
async fn native_pane_picker_routes_keys_across_tabs_and_restores_focus_on_escape() {
    let (source, _remote, opens, server) = fixture_connections(false, 2).await;
    let Source::Ssh(dev1) = source else {
        unreachable!()
    };
    let mut dev2 = dev1.clone();
    dev2.user = "navigator".into();
    let state = tempfile::tempdir().unwrap();
    // An unmodified key exercises the editable hints action on the fixture window.
    fs::write(
        state.path().join("appearance.json"),
        br#"{"shortcuts":{"keyboard_hints":"F8"}}"#,
    )
    .unwrap();
    let data = state.path().join("data");
    fs::create_dir(&data).unwrap();
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles": {"dev1": dev1, "dev2": dev2}})).unwrap(),
    )
    .unwrap();
    let leaf = |slot: u32, source: Value| json!({"type":"leaf", "slot":slot, "source":source});
    let ssh = |name| json!({"kind":"saved_profile", "value":name});
    let local = json!({"kind":"local","value":{"name":"Windows PowerShell",
        "shell":{"executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}}});
    let workspace = json!({"active_tab":0,"tabs":[
        {"focused":1,"layout":leaf(1, ssh("dev1"))},
        {"focused":3,"layout":{"type":"split","axis":"columns","ratio":0.5,
            "first":leaf(2, ssh("dev2")), "second":leaf(3, local)}}
    ]});
    fs::write(
        data.join("workspaces.json"),
        serde_json::to_vec(&json!({"version":1,"workspaces":{"picker":workspace}})).unwrap(),
    )
    .unwrap();
    let result = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-STA", "-File"])
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pane-picker-ui.ps1"))
        .args([
            "-Gui",
            env!("CARGO_BIN_EXE_w11term"),
            "-Cli",
            env!("CARGO_BIN_EXE_w11term-cli"),
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
        opens.load(Ordering::Relaxed),
        2,
        "switching panes reopened an SSH connection"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens an isolated native WinUI window"]
async fn native_ssh_split() {
    let (source, _root, opens, server) = fixture_server(false).await;
    let state = tempfile::tempdir().unwrap();
    run_gui(
        &source,
        state.path(),
        &[
            ("W11TERM_SPLIT_SMOKE", "1"),
            ("W11TERM_SSH_SPLIT_SMOKE", "1"),
        ],
        "Split pane smoke passed",
    )
    .await;
    assert_eq!(
        opens.load(Ordering::Relaxed),
        3,
        "three shells opened on the one accepted SSH transport"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens an isolated native WinUI window"]
async fn native_file_drawer() {
    let (source, remote, opens, server) = fixture_server(false).await;
    let shell_dir = remote.path().join("shell directory");
    fs::create_dir(&shell_dir).unwrap();
    fs::create_dir(shell_dir.join("src")).unwrap();
    fs::write(shell_dir.join("src/trace.rs"), b"hint target").unwrap();
    let state = tempfile::tempdir().unwrap();
    let bytes: Vec<_> = (0..(3 * w11term::sftp::CHUNK + 7))
        .map(|i| (i % 251) as u8)
        .collect();
    fs::write(state.path().join("upload.bin"), &bytes).unwrap();
    run_gui(
        &source,
        state.path(),
        &[("W11TERM_FILE_SMOKE", state.path().to_str().unwrap())],
        "File drawer smoke passed",
    )
    .await;
    assert_eq!(fs::read(shell_dir.join("upload.bin")).unwrap(), bytes);
    assert!(!remote.path().join("upload.bin").exists());
    assert_eq!(fs::read(state.path().join("download.bin")).unwrap(), bytes);
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one shell and one SFTP channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens an isolated mixed SSH and PowerShell window"]
async fn native_mixed_ssh_and_powershell() {
    let (source, _remote, opens, server) = fixture_connections(false, 2).await;
    let state = tempfile::tempdir().unwrap();
    run_gui(
        &source,
        state.path(),
        &[("W11TERM_LOCAL_SMOKE", state.path().to_str().unwrap())],
        "Mixed SSH and local shell smoke passed",
    )
    .await;
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one SSH shell before and one after restoring the workspace"
    );
    let baseline = image::open(state.path().join("direct.png"))
        .unwrap()
        .to_rgba8();
    let mixed = image::open(state.path().join("mixed.png"))
        .unwrap()
        .to_rgba8();
    let crop = |img: &image::RgbaImage| image::imageops::crop_imm(img, 0, 0, 130, 70).to_image();
    assert_eq!(crop(&baseline), crop(&mixed));
    assert!(
        crop(&baseline)
            .pixels()
            .any(|p| p[0] > 220 && p[1] < 45 && p[2] < 45),
        "expected the red kitty fixture"
    );
    server.abort();
}
