use super::*;
#[path = "../support/native_gui.rs"]
mod shared;
use shared::run_gui;

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
