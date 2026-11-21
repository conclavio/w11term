use super::*;
#[path = "../support/native_gui.rs"]
mod shared;
use shared::run_gui;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manages a verified profile's read-only and writable folder consent with the native Windows folder picker"]
async fn native_shared_folders_keep_consent_bound_to_the_verified_ssh_profile() {
    let (Source::Ssh(mut ssh), remote, opens, server) = fixture_connections(false, 3).await else {
        unreachable!()
    };
    // Launch two intentionally disconnects before closing. Its automatic retry
    // must not take the third server connection reserved for the consent check.
    ssh.player.reconnect = false;
    let state = tempfile::tempdir().unwrap();
    let selected = state.path().join("Selected folder");
    fs::create_dir(&selected).unwrap();
    fs::write(selected.join("sample.txt"), b"unchanged").unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    fs::write(remote.path().join("disconnect-on-eot"), []).unwrap();
    drive_mixed_gui("folder-share-ui.ps1", &ssh, state.path());
    assert_eq!(
        fs::read(trace).unwrap(),
        b"share-shell-before-restartshare-shell-after-restart\x04share-shell-after-revocation"
    );
    assert_eq!(fs::read(selected.join("sample.txt")).unwrap(), b"unchanged");
    assert_eq!(
        opens.load(Ordering::Relaxed),
        7,
        "three live shells and four refused mount attempts"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives the command palette across a loopback SSH session and a PowerShell tab"]
async fn native_command_palette_opens_powershell_and_restores_ssh_input() {
    run_palette_fixture("command-palette-ui.ps1", b"after", None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "watches OSC 133 commands in a background SSH tab while a real PowerShell stays active"]
async fn native_command_finish_is_one_shot_and_returns_to_the_same_ssh_pane() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_connections(false, 1).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    let start = "\x1b]133;A\x07$ \x1b]133;B\x07build\r\n\x1b]133;C\x07";
    fs::write(
        remote.path().join("copy-history.vt"),
        format!("{start}command-started\r\n"),
    )
    .unwrap();

    // The peer echoes these packets. Each witness is rendered after the OSC, so
    // the UI driver can check both positive and negative outcomes without sleeps.
    let mut packets = serde_json::Map::new();
    let mut expected = Vec::new();
    for (witness, prefix, completion) in [
        ("first-complete", "", "\x1b]133;D;17\x07"),
        ("not-rearmed", start, "\x1b]133;D;0\x07"),
        ("cancel-complete", start, "\x1b]133;D;9\x07"),
        ("next-complete", start, "\x1b]133;D\x07"),
    ] {
        let bytes = format!("{prefix}{completion}{witness}\r\n");
        expected.extend_from_slice(bytes.as_bytes());
        packets.insert(witness.to_owned(), Value::String(bytes));
    }
    fs::write(
        state.path().join("commands.json"),
        serde_json::to_vec(&packets).unwrap(),
    )
    .unwrap();
    drive_mixed_gui("command-finish-ui.ps1", &ssh, state.path());
    expected.extend_from_slice(b"ssh-resumed");
    assert_eq!(fs::read(trace).unwrap(), expected);
    assert_eq!(opens.load(Ordering::Relaxed), 1, "one live shell channel");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "enables PowerShell shell integration in Settings and observes a real command beside SSH"]
async fn native_powershell_shell_integration_records_history_and_completion() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_connections(false, 1).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    drive_mixed_gui("powershell-integration-ui.ps1", &ssh, state.path());
    assert_eq!(fs::read(trace).unwrap(), b"neighbor-ok");
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "keeps an open native tab menu actionable through SSH progress repaints"]
async fn native_open_caret_keeps_its_settings_item_while_the_header_repaints() {
    let mut expected = Vec::new();
    for percent in [20, 40, 60, 80] {
        expected.extend(format!("\x1b]9;4;1;{percent}\x07").bytes());
    }
    run_palette_fixture("caret-repaint-ui.ps1", &expected, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reorders saved SSH and local profiles in Settings and launches them from the native menu"]
async fn native_profile_order_survives_restart_and_launches_the_selected_backend() {
    run_palette_fixture("profile-order-ui.ps1", b"ordered-ssh", None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "locks and unlocks input in a live SSH and PowerShell split through the native UI"]
async fn native_read_only_input_is_per_pane_in_a_mixed_split() {
    run_palette_fixture("read-only-pane-ui.ps1", b"resumed", None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "searches wrapped SSH history with native regex Find and resumes typing in both panes"]
async fn native_regex_find_in_mixed_panes_keeps_search_input_off_the_wire() {
    let filler = "x".repeat(360);
    let mut history = format!(
        "CaseOne-start{filler}-End-17\r\nCaseOne-start{filler}-End-29\r\ncaseone-start{filler}-end-31\r\n"
    );
    for row in 0..180 {
        history.push_str(&format!("scrollback filler {row}\r\n"));
    }
    history.push_str("find-fixture-ready\r\n");
    run_palette_fixture("regex-find-ui.ps1", b"find-ssh-resumed", Some(&history)).await;
}

async fn run_palette_fixture(script: &str, expected_input: &[u8], history: Option<&str>) {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_connections(false, 1).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    if let Some(history) = history {
        fs::write(remote.path().join("copy-history.vt"), history).unwrap();
    }
    drive_mixed_gui(script, &ssh, state.path());
    assert_eq!(fs::read(trace).unwrap(), expected_input);
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    server.abort();
}

fn drive_mixed_gui(script: &str, ssh: &SshOptions, state: &std::path::Path) {
    fs::write(
        state.join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{
        "palette-ssh":ssh, "Palette PowerShell":{
            "backend":"local", "executable":"powershell.exe", "args":["-NoProfile","-NoLogo"]}}}))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        state.join("appearance.json"),
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
        .arg(state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string(state.join("gui.log")).unwrap_or_default()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "verifies normal and raw clipboard paste decisions through WinUI and a reconnecting loopback peer"]
async fn native_clipboard_paste_confirms_original_bytes_for_the_original_pane() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(mut ssh), remote, channels, server) =
        fixture_counting_accepts(false, 3, accepted.clone()).await
    else {
        unreachable!()
    };
    ssh.player.reconnect = true;
    let state = tempfile::tempdir().unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    fs::write(remote.path().join("disconnect-on-eot"), []).unwrap();
    fs::write(
        state.path().join("peer-trace-path.txt"),
        trace.to_str().unwrap(),
    )
    .unwrap();
    drive_mixed_gui("clipboard-paste-ui.ps1", &ssh, state.path());
    let wire = fs::read_to_string(trace).unwrap();
    assert!(wire.ends_with("ssh-still-live/"));
    assert!(wire.contains("\x1b[200~wrapped 雪\r\n\x1b[201~"));
    assert!(wire.contains("raw 雪\r\n\t \r\n"));
    assert!(!wire.contains("discard-"));
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        2,
        "original and reconnected SSH transports"
    );
    assert_eq!(
        channels.load(Ordering::Relaxed),
        2,
        "one shell on each transport"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives OSC progress across a real SSH split, a reconnect and a PowerShell tab"]
async fn native_terminal_progress_updates_background_tab_and_resets_on_reconnect() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_connections(false, 3).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    fs::write(remote.path().join("disconnect-on-eot"), []).unwrap();
    drive_mixed_gui("terminal-progress-ui.ps1", &ssh, state.path());
    let mut expected = Vec::new();
    for state in ["1;42", "4;68", "2", "0", "3", "0", "1;31"] {
        expected.extend_from_slice(format!("\x1b]9;4;{state}\x07").as_bytes());
    }
    expected.extend_from_slice(b"\x04\x1b]9;4;1;77\x07\x1b]9;4;0\x07ssh-alive");
    assert_eq!(fs::read(trace).unwrap(), expected);
    assert_eq!(
        opens.load(Ordering::Relaxed),
        3,
        "two shells, one reconnect"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "exports a live SSH pane through the native Save dialog beside a PowerShell split"]
async fn native_scrollback_export_saves_the_selected_pane_and_respects_cancel_and_replace() {
    let (Source::Ssh(ssh), _remote, opens, server) = fixture_connections(false, 1).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let directory = state.path();
    let plain_header = "archived 雪 café 🚀";
    let mut expected = format!(
        "{plain_header}\r\n\r\nwrapped-start:{}:wrapped-end\r\n",
        "abcdef".repeat(85)
    );
    for row in 0..160 {
        expected.push_str(&format!("history row {row:03}\r\n"));
    }
    expected.push_str("snapshot-end\r\n");
    let formatted = expected.replacen(plain_header, &format!("\x1b[35m{plain_header}\x1b[0m"), 1);
    fs::write(
        directory.join("history.vt"),
        format!("\x1b[2J\x1b[3J\x1b[H{formatted}"),
    )
    .unwrap();
    let sentinel = b"original\x00\xffcontents";
    for filename in ["cancel.txt", "replace.txt"] {
        fs::write(directory.join(filename), sentinel).unwrap();
    }
    drive_mixed_gui("export-scrollback-ui.ps1", &ssh, directory);
    let saved = fs::read_to_string(directory.join("new.txt")).unwrap();
    assert_eq!(saved, expected, "exact UTF-8 transcript at invocation");
    assert_eq!(fs::read(directory.join("cancel.txt")).unwrap(), sentinel);
    let replaced = fs::read_to_string(directory.join("replace.txt")).unwrap();
    assert_eq!(replaced, format!("{expected}arrived-after-picker"));
    assert_eq!(opens.load(Ordering::Relaxed), 1, "one live SSH shell");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "restarts exited SSH and PowerShell panes from the native menus without replacing their split"]
async fn native_restart_exited_panes_preserves_history_and_their_live_neighbor() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, channels, server) =
        fixture_counting_accepts(false, 3, accepted.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let auth = remote.path().join("authenticated-users");
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&auth, []).unwrap();
    fs::write(&trace, []).unwrap();
    fs::write(remote.path().join("exit-on-eot"), []).unwrap();
    fs::write(remote.path().join("sftp-proof.bin"), [0, 9, 255]).unwrap();
    let mut history = String::from("archived SSH 雪\r\n");
    for row in 0..120 {
        history.push_str(&format!("SSH output row {row}\r\n"));
    }
    history.push_str("before SSH exit\r\n");
    fs::write(state.path().join("ssh-history.vt"), &history).unwrap();
    drive_mixed_gui("restart-exited-pane-ui.ps1", &ssh, state.path());
    let mut expected_input = history.into_bytes();
    expected_input.extend_from_slice(b"\x04restarted-ssh-neighbor-alive");
    assert_eq!(fs::read(trace).unwrap(), expected_input);
    assert_eq!(
        fs::read_to_string(auth)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["test", "test"]
    );
    assert_eq!(accepted.load(Ordering::Relaxed), 2, "two SSH transports");
    assert_eq!(
        channels.load(Ordering::Relaxed),
        4,
        "one shell and one SFTP channel per login"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "creates remote folders through the native SFTP drawer while a PowerShell pane is live"]
async fn native_new_folder_uses_the_selected_sftp_parent_and_preserves_existing_entries() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let parent = remote.path().join("shell directory").join("projects");
    let existing = parent.join("kept");
    fs::create_dir_all(&existing).unwrap();
    let directory_sentinel = b"keep\0the\x80child";
    fs::write(existing.join("sentinel.bin"), directory_sentinel).unwrap();
    let state = tempfile::tempdir().unwrap();
    let raced_file = [0, 255, 17, 13, 10, 128];
    fs::write(state.path().join("collision.bin"), raced_file).unwrap();
    let mkdir_trace = remote.path().join("mkdir-trace");
    fs::write(&mkdir_trace, []).unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();

    drive_mixed_gui("new-folder-ui.ps1", &ssh, state.path());

    let mut names: Vec<_> = fs::read_dir(&parent)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["Logs 雪 (1)", "kept", "raced.bin"]);
    assert!(parent.join("Logs 雪 (1)").is_dir());
    assert_eq!(
        fs::read(existing.join("sentinel.bin")).unwrap(),
        directory_sentinel
    );
    assert_eq!(fs::read(parent.join("raced.bin")).unwrap(), raced_file);
    assert_eq!(
        fs::read_to_string(mkdir_trace)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "/shell directory/projects/kept",
            "/shell directory/projects/raced.bin",
            "/shell directory/projects/Logs 雪 (1)",
        ],
        "cancel sends no MKDIR and each confirmed action uses the canonical parent"
    );
    assert_eq!(fs::read(trace).unwrap(), b"folder-ssh-resumed");
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one SSH shell and one SFTP channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "renames a file and a nonempty directory through a live mixed SSH/PowerShell drawer"]
async fn native_rename_uses_the_selected_sftp_entry_and_preserves_existing_destinations() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let parent = remote.path().join("shell directory");
    fs::create_dir_all(parent.join("original folder")).unwrap();
    let file = b"file\x00\x80\xff";
    let child = b"directory contents\x00\x95";
    let collision = b"created while the dialog was open\x00\xf0";
    fs::write(parent.join("before.bin"), file).unwrap();
    fs::write(parent.join("original folder/inside.bin"), child).unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(state.path().join("collision.bin"), collision).unwrap();
    let wire = remote.path().join("rename-trace.jsonl");
    let keyboard = remote.path().join("keyboard-input.bin");
    fs::write(&wire, []).unwrap();
    fs::write(&keyboard, []).unwrap();

    drive_mixed_gui("rename-entry-ui.ps1", &ssh, state.path());

    assert!(!parent.join("before.bin").exists());
    assert!(!parent.join("original folder").exists());
    assert_eq!(fs::read(parent.join("Report 雪.bin")).unwrap(), file);
    assert_eq!(fs::read(parent.join("raced.bin")).unwrap(), collision);
    assert_eq!(
        fs::read(parent.join("build results/inside.bin")).unwrap(),
        child
    );
    let pairs: Vec<[String; 2]> = fs::read_to_string(wire)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        pairs,
        [
            [
                "/shell directory/before.bin",
                "/shell directory/Report 雪.bin"
            ],
            [
                "/shell directory/original folder",
                "/shell directory/build results"
            ],
        ]
    );
    assert_eq!(fs::read(keyboard).unwrap(), b"rename-ssh-ready");
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one SSH shell and one SFTP channel"
    );
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
