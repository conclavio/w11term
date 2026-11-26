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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "moves a live SSH and PowerShell split into an existing HWND"]
async fn native_move_live_split_to_existing_window() {
    run("Existing", b"ssh-probe/existing-window/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "consolidates live SSH and PowerShell tabs without moving utility tabs"]
async fn native_move_all_terminal_tabs_between_windows() {
    run("AllTabs", b"before-bulk/after-bulk/after-return/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "moves a background split from its tab header and preserves the other window's active session"]
async fn native_move_background_tab_from_context_menu() {
    run("Context", b"ssh-probe/existing-window/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "finds and activates a background tab in a minimized sibling HWND"]
async fn native_find_tab_across_windows() {
    run("Search", b"ssh-probe/searched-window/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "previews and focuses live panes in a sibling native HWND"]
async fn native_find_pane_across_windows() {
    run(
        "PaneSearch",
        b"ssh-probe/preview-before-live/\r\nfocused-remote/",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reveals a historical SSH match in a sibling HWND and handles a closed result"]
async fn native_find_text_across_windows() {
    let mut wire = String::from("ssh-probe/\r\nold fleet needle\r\n");
    for row in 1..=70 {
        wire.push_str(&format!("scrollback row {row}\r\n"));
    }
    wire.push_str("ssh latest\r\n");
    run("TextSearch", wire.as_bytes()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "moves one live PowerShell pane and then the last SSH pane to an existing tab in a sibling HWND"]
async fn native_move_live_panes_to_a_sibling_tab() {
    run("PaneMove", b"ssh-probe/split-stays/only-pane-moved/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "detaches live SSH and PowerShell panes into their own native windows"]
async fn native_move_split_pane_to_a_new_window() {
    run("PaneWindow", b"ssh-stays/ssh-moved/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "names one native window, finds its live tabs and panes, and restores the default title"]
async fn native_named_window_is_searchable_and_can_restore_the_default_title() {
    run("WindowName", b"ssh-probe/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "finds and restores a named window without changing its active tab or pane"]
async fn native_find_window_preserves_the_current_tab_and_drops_closed_windows() {
    run("WindowSearch", b"ssh-probe/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens fresh SSH and PowerShell profiles in new native windows on the same pipe"]
async fn native_open_profiles_in_new_windows_preserves_the_original_sessions() {
    run("ProfileWindow", b"fresh-window/source-window/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "duplicates a pinned SSH and PowerShell layout from its background tab into a new window"]
async fn native_duplicate_tab_layout_in_a_new_window_starts_fresh_sessions() {
    run("DuplicateWindow", b"source-layout/copy-window/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens a saved workspace in a second native window in the same instance"]
async fn native_saved_workspace_opens_in_a_new_window() {
    run("WorkspaceWindow", b"workspace-copy/original-workspace/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "renames and removes a saved workspace while its original SSH and PowerShell panes stay live"]
async fn native_manage_saved_workspace_keeps_its_open_sessions() {
    run("ManageWorkspace", b"after-management/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reviews a saved layout and refuses to overwrite concurrent workspace changes"]
async fn native_workspace_replacement_requires_a_current_review() {
    run("ReplaceWorkspace", b"after-review/").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "duplicates the saved recipe before opening it in an independently registered window"]
async fn native_duplicate_saved_workspace_registers_independently_when_opened() {
    run("CopyWorkspace", b"copy-recipe/original-recipe/").await;
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
    if first_close == "ProfileWindow" {
        fs::write(
            data.join("windows.json"),
            br#"{"Destination PowerShell":{"x":32,"y":40,"width":850,"height":600,"maximized":false,"cols":96,"rows":28}}"#,
        )
        .unwrap();
    }
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{
            "window-ssh": ssh,
            "Destination PowerShell":{
                "backend":"local", "executable":"powershell.exe", "args":["-NoProfile","-NoLogo"]
            }
        }}))
        .unwrap(),
    )
    .unwrap();
    let mut shortcuts = json!({"command_palette":"F7","focus_right":"F8","focus_left":"F9",
        "copy_mode":"F10","find_tab":"F12","find_window":"F1","pane_picker":"F6","find_all_panes":"F4",
        "move_pane_to_existing_tab":"F3","move_pane_to_new_window":"F2"});
    if first_close == "DuplicateWindow" {
        shortcuts["find_all_panes"] = Value::Null;
        shortcuts["zoom_pane"] = json!("F4");
    }
    fs::write(
        state.path().join("appearance.json"),
        serde_json::to_vec(&json!({"shortcuts":shortcuts})).unwrap(),
    )
    .unwrap();
    let local = |name| {
        json!({"kind":"local","value":{"name":name,
        "shell":{"executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}}})
    };
    let leaf = |slot, source: Value| json!({"type":"leaf","slot":slot,"source":source});
    let workspace = json!({"active_tab": usize::from(matches!(first_close, "Context" | "WorkspaceWindow" | "CopyWorkspace")),"tabs":[
        {"title":"Cockpit tools","focused":1,"layout":{"type":"split","axis":"columns","ratio":0.375,
            "first":leaf(1,json!({"kind":"saved_profile","value":"window-ssh"})),
            "second":leaf(2,local("Moved PowerShell"))}},
        {"focused":3,"layout":leaf(3,local("Source PowerShell"))}
    ]});
    let mut workspaces = json!({"two-windows":workspace});
    if matches!(first_close, "WorkspaceWindow" | "ManageWorkspace") {
        workspaces["broken-copy"] = json!({"active_tab":0,"tabs":[
            {"focused":1,"layout":leaf(1,json!({"kind":"saved_profile","value":"missing-window-profile"}))}
        ]});
    }
    if first_close == "CopyWorkspace" {
        workspaces["two-windows copy"] = json!({"active_tab":0,"tabs":[
            {"focused":1,"layout":leaf(1,json!({"kind":"demo"}))}
        ]});
    }
    fs::write(
        data.join("workspaces.json"),
        serde_json::to_vec(&json!({"version":1,"workspaces":workspaces})).unwrap(),
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
        "{}\n{}\n{}\nSSH input: {:?}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
        fs::read_to_string(state.path().join("gui.log")).unwrap_or_default(),
        fs::read_to_string(&trace).unwrap_or_default()
    );
    let expected_ssh = 1 + usize::from(matches!(
        first_close,
        "ProfileWindow" | "DuplicateWindow" | "WorkspaceWindow" | "CopyWorkspace"
    ));
    assert_eq!(
        transports.load(Ordering::Relaxed),
        expected_ssh,
        "SSH transport count"
    );
    assert_eq!(
        sessions.load(Ordering::Relaxed),
        expected_ssh,
        "SSH channel count"
    );
    assert_eq!(
        fs::read(trace).unwrap(),
        expected_wire,
        "window-scoped keyboard input"
    );
    server.abort();
}
