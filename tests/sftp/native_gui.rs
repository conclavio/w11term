use super::*;
#[path = "../support/native_gui.rs"]
mod shared;
use shared::run_gui;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "checks persisted Follow Windows colors and reports against native UISettings"]
async fn native_follow_windows_profile_matches_the_host_and_can_be_overridden_live() {
    let (Source::Ssh(ssh), remote, shells, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    fs::create_dir(remote.path().join("split-inputs")).unwrap();
    fs::write(
        state.path().join("peer-root"),
        remote.path().to_str().unwrap(),
    )
    .unwrap();
    drive_mixed_gui("follow-windows-colors-ui.ps1", &ssh, state.path());
    assert_eq!(
        shells.load(Ordering::Relaxed),
        2,
        "one SSH shell per process"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "switches the base colors of a read-only SSH split with retained OSC overrides"]
async fn native_live_pane_scheme_preserves_remote_colors_input_and_profile() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(mut ssh), remote, shells, server) =
        fixture_counting_accepts(false, 2, accepted.clone()).await
    else {
        unreachable!()
    };
    ssh.color_scheme = w11term::options::ColorScheme::OneHalfDark;
    let inputs = remote.path().join("split-inputs");
    fs::create_dir(&inputs).unwrap();
    fs::write(remote.path().join("pty-trace"), []).unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(
        state.path().join("peer-root"),
        remote.path().to_str().unwrap(),
    )
    .unwrap();
    drive_mixed_gui("live-pane-colors-ui.ps1", &ssh, state.path());
    assert!(
        fs::read_dir(inputs).unwrap().next().is_none(),
        "a UI action sent shell input"
    );
    assert_eq!(
        shells.load(Ordering::Relaxed),
        3,
        "original, split and fresh profile"
    );
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        2,
        "one shared login and one fresh profile"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "edits SSH and local profile schemes in WinUI and checks their restarted terminal PNGs"]
async fn native_profile_color_schemes_survive_restart_and_isolate_palette_resets() {
    let (Source::Ssh(ssh), _remote, shells, server) = fixture_connections(false, 3).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("profile-color-schemes-ui.ps1", &ssh, state.path());
    assert_eq!(shells.load(Ordering::Relaxed), 3);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "renders DEC reverse-screen changes in control PNGs from two live SSH panes"]
async fn native_reverse_screen_repaints_existing_text_and_isolates_ssh_panes() {
    let (Source::Ssh(ssh), _remote, channels, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("dec-reverse-screen-ui.ps1", &ssh, state.path());
    let sizes: Value =
        serde_json::from_slice(&fs::read(state.path().join("sizes.json")).unwrap()).unwrap();
    let range = |index: u32, pixels: u32, cells: u32| index * pixels / cells;
    for (name, size, fg, bg) in [
        ("normal", "source", [255, 255, 0], [0, 0, 255]),
        ("reverse", "source", [0, 0, 255], [255, 255, 0]),
        ("restored", "source", [255, 255, 0], [0, 0, 255]),
        ("neighbor-before", "neighbor", [0, 255, 0], [255, 0, 0]),
        ("neighbor-during", "neighbor", [0, 255, 0], [255, 0, 0]),
        ("neighbor-after", "neighbor", [0, 255, 0], [255, 0, 0]),
    ] {
        let png = image::open(state.path().join(format!("{name}.png")))
            .unwrap()
            .into_rgb8();
        let (width, height) = png.dimensions();
        let cols = sizes[size]["cols"].as_u64().unwrap() as u32;
        let rows = sizes[size]["rows"].as_u64().unwrap() as u32;
        let sample = |col: u32, row: u32| {
            png.get_pixel(
                range(2 * col + 1, width, 2 * cols),
                range(2 * row + 1, height, 2 * rows),
            )
            .0
        };
        assert_eq!(sample(2, 4), bg, "{name} empty default cell");
        assert_eq!(sample(2, 1), fg, "{name} blank SGR 7 cell");
        assert_eq!(sample(2, 2), [255, 0, 255], "{name} explicit background");
        // The first eight existing H glyphs must still paint in the effective foreground.
        let ink = (0..range(1, height, rows)).any(|y| {
            (0..range(8, width, cols)).any(|x| {
                png.get_pixel(x, y)
                    .0
                    .iter()
                    .zip(fg)
                    .all(|(&a, b)| a.abs_diff(b) < 48)
            })
        });
        assert!(ink, "{name} unchanged glyphs did not redraw in {fg:?}");
    }
    assert_eq!(channels.load(Ordering::Relaxed), 2, "one shell per pane");
    server.abort();
}

async fn serve_forward_http(
    state: &std::path::Path,
    root: &std::path::Path,
    connections: usize,
) -> (u16, tokio::task::JoinHandle<Vec<usize>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let token = state.file_name().unwrap().to_str().unwrap().to_owned();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    fs::write(root.join("forward-port"), port.to_string()).unwrap();
    fs::write(state.join("target-port"), port.to_string()).unwrap();
    fs::write(state.join("token"), &token).unwrap();
    // Accept only this fixture's small fixed HTTP request on each keep-alive connection.
    let service = tokio::spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        let request = format!("GET /{token} HTTP/1.1\r\nHost: fixture\r\n\r\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{token}",
            token.len()
        );
        for _ in 0..connections {
            let (mut socket, address) = listener.accept().await.unwrap();
            assert!(address.ip().is_loopback());
            let (request, response) = (request.clone(), response.clone());
            clients.spawn(async move {
                let mut buf = vec![0; request.len()];
                let mut served = 0;
                while socket.read_exact(&mut buf).await.is_ok() {
                    assert_eq!(buf, request.as_bytes());
                    socket.write_all(response.as_bytes()).await.unwrap();
                    served += 1;
                }
                served
            });
        }
        let mut counts = Vec::new();
        while let Some(count) = clients.join_next().await {
            counts.push(count.unwrap());
        }
        counts.sort();
        counts
    });
    (port, service)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "imports loopback SSH forward presets through WinUI and starts one only on explicit request"]
async fn native_imported_forward_preset_remains_inactive_across_save_open_and_reconnect() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), root, sessions, server) =
        fixture_counting_accepts(false, 2, accepted.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let (remote, service) = serve_forward_http(state.path(), root.path(), 1).await;
    let reservation = || std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let (supported, rejected) = (reservation(), reservation());
    let (local, wildcard) = (
        supported.local_addr().unwrap().port(),
        rejected.local_addr().unwrap().port(),
    );
    fs::write(state.path().join("config"), format!(
        "Host imported-loopback\n  HostName 127.0.0.1\n  User test\n  Port {}\n  LocalForward 127.0.0.1:{local} localhost:{remote}\n  LocalForward *:{wildcard} localhost:{remote}\n", ssh.port
    )).unwrap();
    fs::write(state.path().join("local-port"), local.to_string()).unwrap();
    fs::write(state.path().join("wildcard-port"), wildcard.to_string()).unwrap();
    let gate = root.path().join("keep-first-connection");
    fs::write(&gate, []).unwrap();
    fs::write(state.path().join("disconnect-gate"), gate.to_str().unwrap()).unwrap();
    for log in ["authenticated-users", "keyboard-input.bin", "forward-trace"] {
        fs::write(root.path().join(log), []).unwrap();
    }
    drop((supported, rejected));
    drive_mixed_gui("import-forward-preset-ui.ps1", &ssh, state.path());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), service)
            .await
            .unwrap()
            .unwrap(),
        [1]
    );
    let origins = fs::read_to_string(state.path().join("client-ports")).unwrap();
    let source_port: u16 = origins.trim().parse().unwrap();
    let trace = fs::read_to_string(root.path().join("forward-trace")).unwrap();
    assert_eq!(
        trace.trim(),
        format!("127.0.0.1:{remote} 127.0.0.1:{source_port}")
    );
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        2,
        "only the open and its reconnect logged in"
    );
    assert_eq!(
        sessions.load(Ordering::Relaxed),
        2,
        "one shell per transport"
    );
    let users = fs::read_to_string(root.path().join("authenticated-users")).unwrap();
    assert_eq!(users.lines().collect::<Vec<_>>(), ["test", "test"]);
    assert!(
        fs::read(root.path().join("keyboard-input.bin"))
            .unwrap()
            .is_empty()
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "forwards Windows loopback clients over an existing SSH transport beside PowerShell"]
async fn native_local_forward_stops_clients_and_expires_with_its_ssh_pane() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(mut ssh), root, sessions, server) =
        fixture_counting_accepts(false, 2, accepted.clone()).await
    else {
        unreachable!()
    };
    ssh.player.reconnect = false;
    let state = tempfile::tempdir().unwrap();
    let (port, service) = serve_forward_http(state.path(), root.path(), 3).await;
    for file in [
        "forward-trace",
        "authenticated-users",
        "keyboard-input.bin",
        "disconnect-on-eot",
    ] {
        fs::write(root.path().join(file), []).unwrap();
    }
    let data = state.path().join("data");
    fs::create_dir(&data).unwrap();
    let workspace = json!({"active_tab":0,"tabs":[{"title":"Forwarded service","focused":1,
        "layout":{"type":"leaf","slot":1,"source":{"kind":"startup","value":{
            "connection":{"kind":"saved_profile","value":"palette-ssh"},
            "launch":{"host":"127.0.0.1","port":ssh.port,"user":"test","command":"echo forward-workspace"}
        }}}
    }]});
    fs::write(
        data.join("workspaces.json"),
        serde_json::to_vec(&json!({"version":1,"workspaces":{"forwarding":workspace}})).unwrap(),
    )
    .unwrap();
    fs::write(root.path().join("workspace-exec.bin"), []).unwrap();
    fs::write(
        state.path().join("refuse-flag"),
        root.path().join("forward-refuse").to_str().unwrap(),
    )
    .unwrap();
    drive_mixed_gui("ssh-local-forward-ui.ps1", &ssh, state.path());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), service)
            .await
            .unwrap()
            .unwrap(),
        [1, 1, 3]
    );
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        2,
        "only the two explicitly opened SSH sessions authenticated"
    );
    assert_eq!(
        sessions.load(Ordering::Relaxed),
        2,
        "no extra shell channels"
    );
    let users = fs::read_to_string(root.path().join("authenticated-users")).unwrap();
    assert_eq!(users.lines().collect::<Vec<_>>(), ["test", "test"]);
    assert_eq!(
        fs::read(root.path().join("workspace-exec.bin")).unwrap(),
        b"echo forward-workspace"
    );
    let trace = fs::read_to_string(root.path().join("forward-trace")).unwrap();
    let origins: Vec<_> = trace
        .lines()
        .map(|line| {
            let (destination, origin) = line.split_once(' ').unwrap();
            assert_eq!(destination, format!("127.0.0.1:{port}"));
            origin.parse::<std::net::SocketAddr>().unwrap()
        })
        .collect();
    assert_eq!(
        origins.len(),
        4,
        "three working channels and one refused open"
    );
    let expected = fs::read_to_string(state.path().join("client-ports")).unwrap();
    assert_eq!(
        origins,
        expected
            .lines()
            .map(|line| format!("127.0.0.1:{line}")
                .parse::<std::net::SocketAddr>()
                .unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        fs::read(root.path().join("keyboard-input.bin")).unwrap(),
        b"ssh-forward-live\x04"
    );
    server.abort();
}

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
#[ignore = "scrolls a live SSH pane via Windows UI Automation and its pointer rail beside PowerShell"]
async fn native_pane_scrollbar_tracks_async_ssh_output_without_moving_its_neighbor() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let numbered: Vec<_> = (0..200).map(|i| format!("wire-{i:03}")).collect();
    let initial = format!("\x1b[2J\x1b[H\x1b[3J{}", numbered.join("\r\n"));
    fs::write(remote.path().join("copy-history.vt"), initial).unwrap();
    fs::write(
        remote.path().join("deferred-output.vt"),
        b"\r\nwire-200\r\nwire-201\r\nwire-202",
    )
    .unwrap();
    let gate = remote.path().join("hold-shell-output");
    fs::write(&gate, []).unwrap();
    fs::write(state.path().join("output-gate"), gate.to_str().unwrap()).unwrap();
    fs::write(remote.path().join("kept.txt"), []).unwrap();
    fs::write(remote.path().join("knot.txt"), []).unwrap();
    let input = remote.path().join("keyboard-input.bin");
    fs::write(&input, []).unwrap();
    drive_mixed_gui("pane-scrollbar-ui.ps1", &ssh, state.path());
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one SSH shell and one SFTP subsystem"
    );
    assert_eq!(
        fs::read(input).unwrap(),
        b"\x1b[?1049h\x1b[Halternate-proof\x1b[?1049l"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "edits word delimiters and verifies UIA words and double-click selection in a live SSH pane"]
async fn native_word_delimiters_update_reader_and_pointer_selection_in_running_ssh() {
    run_palette_fixture(
        "word-delimiters-ui.ps1",
        b"",
        Some("\r\nscope/srv/alpha/bin:tail;\r\nword-fixture-ready\r\n"),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reads OSC 133 captures from a read-only, inactive SSH pane across a live reconnect"]
async fn native_control_history_keeps_command_ids_bound_to_the_ssh_connection() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(mut ssh), remote, channels, server) =
        fixture_counting_accepts(false, 2, accepted.clone()).await
    else {
        unreachable!()
    };
    ssh.player.reconnect = true;
    let state = tempfile::tempdir().unwrap();
    let command = format!("show {} 界", "cmd".repeat(60));
    let output = format!("résultat-{}\r\n\r\nend", "out".repeat(60));
    let mut stream = format!(
        "\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07\x1b[31m{output}\x1b[0m\x1b]133;D;17\x07\r\n"
    );
    // A shell may omit both the command and its exit code. Empty captured
    // output still differs from output that has already fallen out of history.
    stream.push_str("\x1b]133;A\x07$ \x1b]133;C\x07\x1b]133;D\x07\r\n");
    stream.push_str("\x1b]133;A\x07$ \x1b]133;B\x07archived\r\n\x1b]133;C\x07");
    stream.push_str(&"older row\r\n".repeat(150));
    stream.push_str("\x1b[3J\x1b]133;D;3\x07\r\nhistory-one-ready\r\n");
    fs::write(remote.path().join("copy-history.vt"), stream).unwrap();
    fs::write(remote.path().join("keyboard-input.bin"), []).unwrap();
    fs::write(remote.path().join("keep-first-connection"), []).unwrap();
    fs::write(
        state.path().join("peer-root"),
        remote.path().to_str().unwrap(),
    )
    .unwrap();
    fs::write(
        state.path().join("replacement.vt"),
        b"\x1b]133;A\x07$ \x1b]133;B\x07replacement\r\n\x1b]133;C\x07second transport\x1b]133;D;0\x07\r\nhistory-two-ready\r\n",
    ).unwrap();
    drive_mixed_gui("control-command-history-ui.ps1", &ssh, state.path());
    eprintln!(
        "Command IDs on the original and reconnected SSH sessions: {}",
        fs::read_to_string(state.path().join("observed-command-ids")).unwrap()
    );
    assert_eq!(accepted.load(Ordering::Relaxed), 2, "two SSH transports");
    assert_eq!(
        channels.load(Ordering::Relaxed),
        2,
        "one shell on each connection"
    );
    assert!(
        fs::read(remote.path().join("keyboard-input.bin"))
            .unwrap()
            .is_empty()
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "searches captured OSC 133 command text and copies the selected command on a hosted Windows desktop"]
async fn native_ssh_command_history_searches_and_copies_the_full_command() {
    let long = format!("deploy {} [Ångström]", "x".repeat(128));
    let mut stream = String::new();
    for (command, output, status) in [
        (&long[..], "archived deploy output", 17),
        ("check-status", "[Ångström] appears only in this output", 0),
    ] {
        stream.push_str(&format!("\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07{output}\r\n\x1b]133;D;{status}\x07"));
    }
    stream.push_str("history-search-ready\r\n");
    run_palette_fixture(
        "search-command-history-ui.ps1",
        b"history-resumed",
        Some(&stream),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "samples focused and inactive GPU cursor cells in two native SSH panes"]
async fn native_focused_block_cursor_redraws_the_glyph_and_tracks_pane_focus() {
    let (Source::Ssh(ssh), _remote, shells, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("focused-block-cursor-ui.ps1", &ssh, state.path());
    assert_eq!(shells.load(Ordering::Relaxed), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "samples a cursor cell on the focused native SSH canvas"]
async fn native_ssh_cursor_blinks_while_the_terminal_is_idle() {
    let (Source::Ssh(ssh), _remote, shells, server) = fixture_connections(false, 1).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    // The input-recording fixture also prints every PTY resize. Keep this SSH
    // shell quiet so only the requested VT sequences can move the test cursor.
    drive_mixed_gui("cursor-blink-ui.ps1", &ssh, state.path());
    assert_eq!(shells.load(Ordering::Relaxed), 1);
    server.abort();
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
#[ignore = "duplicates, deletes and restores PowerShell profiles while SSH and a copied shell stay live"]
async fn native_local_profile_copy_delete_and_restore_defaults() {
    run_palette_fixture("local-profile-actions-ui.ps1", b"profile-actions-ok", None).await;
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
#[ignore = "verifies per-profile scrollback retention across a live SSH tab and two PowerShell processes"]
async fn native_profile_scrollback_limits_apply_to_new_panes_and_preserve_live_history() {
    let mut history = String::new();
    for row in 0..180 {
        history.push_str(&format!("ssh-row-{row:03}\r\n"));
    }
    history.push_str("ssh-history-ready\r\n");
    run_palette_fixture(
        "profile-scrollback-ui.ps1",
        b"history-focus-ok",
        Some(&history),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "clears only the focused SSH pane's retained history in a mixed SSH and PowerShell split"]
async fn native_clear_scrollback_preserves_the_ssh_prompt_and_neighboring_powershell_history() {
    let mut history = String::from("ssh-archived-order\r\n");
    for row in 0..140 {
        history.push_str(&format!("ssh-history-{row:03}\r\n"));
    }
    history.push_str("ssh-history-ready\r\n");
    run_palette_fixture(
        "clear-scrollback-ui.ps1",
        b"\x1b[?1049h\x1b[Halt-pilot\x1b[?1049l\r\nlive-heading\r\n\x1b]133;A\x07pilot> \x1b]133;B\x07status",
        Some(&history),
    )
    .await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "finds offscreen text across SSH and PowerShell tabs and checks both viewports and local Find state"]
async fn native_find_in_all_panes_preserves_views_and_reveals_the_chosen_ssh_match() {
    let mut history = String::new();
    for row in 0..280 {
        let content = match row {
            12 => "ssh-first Fleet.Target north".into(),
            105 => "ssh-second FLEET.TARGET south".into(),
            198 => "ssh-prior-anchor".into(),
            _ => format!("SSH history row {row}"),
        };
        history.push_str(&content);
        history.push_str("\r\n");
    }
    history.push_str("ssh-window-search-ready\r\n");
    run_palette_fixture("find-all-panes-ui.ps1", b"global-ssh-ok", Some(&history)).await;
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
#[ignore = "duplicates a named, zoomed SSH and PowerShell split and verifies two independent sets of sessions"]
async fn native_duplicate_tab_layout_starts_independent_ssh_and_powershell() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, channels, server) =
        fixture_counting_accepts(false, 4, accepted.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let data = state.path().join("data");
    fs::create_dir(&data).unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    let leaf = |slot, source: Value| json!({"type":"leaf","slot":slot,"source":source});
    let local = json!({"kind":"local","value":{"name":"Palette PowerShell",
        "shell":{"executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}}});
    let workspace = json!({"active_tab":0,"tabs":[{
        "title":"Operations", "focused":2, "layout":{
            "type":"split", "axis":"columns", "ratio":0.375,
            "first":leaf(1,json!({"kind":"saved_profile","value":"palette-ssh"})), "second":leaf(2,local)
        }
    }]});
    fs::write(
        data.join("workspaces.json"),
        serde_json::to_vec(&json!({"version":1,"workspaces":{"duplicate":workspace}})).unwrap(),
    )
    .unwrap();
    drive_mixed_gui("duplicate-tab-layout-ui.ps1", &ssh, state.path());
    assert_eq!(accepted.load(Ordering::Relaxed), 2, "SSH logins");
    assert_eq!(channels.load(Ordering::Relaxed), 2, "SSH shell channels");
    assert_eq!(
        fs::read(trace).unwrap(),
        b"source-before/duplicate-background/duplicate-focused/source-after/",
        "SSH input seen by the loopback server"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "rotates the nearest live SSH/PowerShell split and verifies the saved workspace geometry"]
async fn native_rotate_split_keeps_shells_and_persists_only_its_parent_axis() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, channels, server) =
        fixture_counting_accepts(false, 2, accepted.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let data = state.path().join("data");
    fs::create_dir(&data).unwrap();
    let input = remote.path().join("keyboard-input.bin");
    fs::write(&input, []).unwrap();
    let local = |slot, name| {
        json!({"type":"leaf","slot":slot,"source":{"kind":"local","value":{
            "name":name,"shell":{"executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}
        }}})
    };
    let workspace = json!({"active_tab":0,"tabs":[{"title":"Operations","focused":2,"layout":{
        "type":"split","axis":"columns","ratio":0.375,"first":local(1,"Outer PowerShell"),
        "second":{"type":"split","axis":"rows","ratio":0.3125,
          "first":{"type":"leaf","slot":2,"source":{"kind":"saved_profile","value":"palette-ssh"}},
          "second":local(3,"Inner PowerShell")}
    }}]});
    fs::write(
        data.join("workspaces.json"),
        serde_json::to_vec(&json!({"version":1,"workspaces":{"original":workspace}})).unwrap(),
    )
    .unwrap();
    drive_mixed_gui("rotate-split-ui.ps1", &ssh, state.path());
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        1,
        "rotation did not authenticate again"
    );
    assert_eq!(
        channels.load(Ordering::Relaxed),
        1,
        "one SSH shell throughout"
    );
    assert_eq!(fs::read(input).unwrap(), b"before/rotated/restored/");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "swaps live SSH and PowerShell neighbors in a nested native workspace and reopens the saved layout"]
async fn native_swap_neighbors_keeps_shells_and_persists_their_new_positions() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, channels, server) =
        fixture_counting_accepts(false, 4, accepted.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let data = state.path().join("data");
    fs::create_dir(&data).unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    let shell = |slot, name| {
        json!({"type":"leaf","slot":slot,"source":{"kind":"local","value":{
            "name":name,"shell":{"executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}
        }}})
    };
    let workspace = json!({"active_tab":0,"tabs":[{
        "title":"Operations","focused":1,"layout":{
            "type":"split","axis":"columns","ratio":0.375,
            "first":{"type":"leaf","slot":1,"source":{"kind":"saved_profile","value":"palette-ssh"}},
            "second":{"type":"split","axis":"rows","ratio":0.3125,
                "first":shell(2,"Upper PowerShell"),"second":shell(3,"Lower PowerShell")}
        }
    }]});
    fs::write(
        data.join("workspaces.json"),
        serde_json::to_vec(&json!({"version":1,"workspaces":{"original":workspace}})).unwrap(),
    )
    .unwrap();
    drive_mixed_gui("swap-pane-ui.ps1", &ssh, state.path());
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        2,
        "one SSH login per launch"
    );
    assert_eq!(
        channels.load(Ordering::Relaxed),
        2,
        "one shell per transport"
    );
    assert_eq!(fs::read(trace).unwrap(), b"before/right/up/reopened/");
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
#[ignore = "cancels real SFTP transfers in a native SSH and PowerShell split"]
async fn native_sftp_cancel_transfer_preserves_files_and_live_shells() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let picker = state.path().join("picker");
    let parent = remote.path().join("shell directory");
    fs::create_dir(&picker).unwrap();
    fs::create_dir(&parent).unwrap();
    let chunk = w11term::sftp::CHUNK;
    let bytes: Vec<u8> = (0..(4 * chunk + 7)).map(|i| (i % 251) as u8).collect();
    fs::write(picker.join("01-first-upload"), &bytes).unwrap();
    fs::write(picker.join("02-queued-upload"), b"must stay local").unwrap();
    fs::write(picker.join("retained-download"), b"keep these bytes").unwrap();
    fs::write(parent.join("retained-download"), &bytes).unwrap();
    fs::write(parent.join("verified-download"), b"same SFTP channel").unwrap();
    for marker in [
        "transfer-trace",
        "keyboard-input.bin",
        "hold-transfer-read",
        "hold-transfer-write",
    ] {
        fs::write(remote.path().join(marker), []).unwrap();
    }
    fs::write(
        state.path().join("fixture-root"),
        remote.path().to_str().unwrap(),
    )
    .unwrap();
    drive_mixed_gui("cancel-sftp-transfer-ui.ps1", &ssh, state.path());

    assert_eq!(
        fs::read(parent.join("01-first-upload")).unwrap(),
        &bytes[..2 * chunk]
    );
    assert!(!parent.join("02-queued-upload").exists());
    assert_eq!(
        fs::read(picker.join("retained-download")).unwrap(),
        b"keep these bytes"
    );
    assert_eq!(
        fs::read(picker.join("verified-download")).unwrap(),
        b"same SFTP channel"
    );
    assert_eq!(
        fs::read_dir(&picker).unwrap().count(),
        4,
        "partial download left a temporary file"
    );
    let trace = fs::read_to_string(remote.path().join("transfer-trace")).unwrap();
    assert_eq!(
        trace.lines().collect::<Vec<_>>(),
        [
            "OPEN /shell directory/01-first-upload",
            "WRITE 0",
            &format!("WRITE {chunk}"),
            "CLOSE",
            "OPEN /shell directory/retained-download",
            "READ 0",
            &format!("READ {chunk}"),
            "CLOSE",
            "OPEN /shell directory/verified-download",
            "READ 0",
            "READ 17",
            "CLOSE",
        ]
    );
    assert_eq!(
        fs::read(remote.path().join("keyboard-input.bin")).unwrap(),
        b"while-upload/after-cancel"
    );
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one shell and one SFTP channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "persists native SFTP bookmarks across launches and scopes them to the verified SSH profile"]
async fn native_sftp_bookmarks_reopen_on_the_active_channel_and_preserve_the_old_listing_on_error()
{
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, channels, server) =
        fixture_counting_accepts(false, 4, accepted.clone()).await
    else {
        unreachable!()
    };
    let target = remote.path().join("Pilot's Work");
    fs::create_dir_all(target.join("deeper")).unwrap();
    fs::create_dir(remote.path().join("stable")).unwrap();
    fs::write(target.join("bookmark-witness.txt"), b"target").unwrap();
    fs::write(target.join("deeper/child-witness.txt"), b"child").unwrap();
    fs::write(
        remote.path().join("stable/stable-witness.txt"),
        b"existing listing",
    )
    .unwrap();
    for log in ["authenticated-users", "keyboard-input.bin", "listing-trace"] {
        fs::write(remote.path().join(log), []).unwrap();
    }
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("sftp-directory-bookmarks-ui.ps1", &ssh, state.path());
    let catalog: Value =
        serde_json::from_slice(&fs::read(state.path().join("profiles.json")).unwrap()).unwrap();
    assert!(
        catalog["profiles"]["palette-ssh"]
            .get("sftp_bookmarks")
            .is_none()
    );
    let users = fs::read_to_string(remote.path().join("authenticated-users")).unwrap();
    assert_eq!(
        users.lines().collect::<Vec<_>>(),
        ["test", "other-account", "test"]
    );
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        3,
        "only explicitly opened SSH profiles logged in"
    );
    assert_eq!(
        channels.load(Ordering::Relaxed),
        6,
        "each original shell reused its one SFTP channel"
    );
    assert!(
        fs::read(remote.path().join("keyboard-input.bin"))
            .unwrap()
            .is_empty()
    );
    let listings = fs::read_to_string(remote.path().join("listing-trace")).unwrap();
    let directories: Vec<_> = listings
        .lines()
        .filter_map(|line| line.strip_prefix("OPENDIR "))
        .collect();
    assert_eq!(
        directories,
        [
            "/",
            "/Pilot's Work",
            "/Pilot's Work/deeper",
            "/Pilot's Work",
            "/",
            "/",
            "/stable",
            "/Pilot's Work",
            "/Pilot's Work",
        ]
    );
    assert!(target.join("bookmark-witness.txt").is_file());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "filters a paginated SFTP listing while checking shell input and directory reads"]
async fn native_sftp_filename_filter_searches_all_pages_without_remote_requests() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let parent = remote.path().join("shell directory");
    let child = parent.join("nested");
    fs::create_dir_all(&child).unwrap();
    fs::write(child.join("inside.txt"), []).unwrap();
    let fillers = 38;
    for index in 0..fillers {
        fs::write(parent.join(format!("build-{index:02}.log")), []).unwrap();
    }
    fs::write(parent.join("zz-[Ångström].txt"), []).unwrap();
    fs::write(parent.join("zz-Ångström.txt"), []).unwrap();
    let keyboard = remote.path().join("keyboard-input.bin");
    let wire = remote.path().join("listing-trace");
    fs::write(&keyboard, []).unwrap();
    fs::write(&wire, []).unwrap();

    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("filter-sftp-listing-ui.ps1", &ssh, state.path());

    let trace = fs::read_to_string(wire).unwrap();
    let mut listings: Vec<(&str, usize)> = vec![];
    for request in trace.lines() {
        if let Some(path) = request.strip_prefix("OPENDIR ") {
            listings.push((path, 0));
        } else {
            assert_eq!(request, "READDIR");
            listings.last_mut().expect("OPENDIR before READDIR").1 += 1;
        }
    }
    // This fixture returns one entry per READDIR plus one final EOF. Only
    // opening the drawer, refreshing, and entering the child may read a list.
    assert_eq!(
        listings,
        [
            ("/shell directory", fillers + 4),
            ("/shell directory", fillers + 5),
            ("/shell directory/nested", 2),
        ]
    );
    assert_eq!(
        fs::read(keyboard).unwrap(),
        b"after-filter-donefilter-ssh-ready"
    );
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one shell and one SFTP channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "toggles the previously focused live SSH or PowerShell pane across splits, tabs and Settings"]
async fn native_previous_pane_reuses_live_shells_across_tabs_and_skips_closed_panes() {
    let (Source::Ssh(ssh), remote, channels, server) = fixture_server(false).await else {
        unreachable!()
    };
    let keyboard = remote.path().join("keyboard-input.bin");
    fs::write(&keyboard, []).unwrap();
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("focus-previous-pane-ui.ps1", &ssh, state.path());
    assert_eq!(fs::read(keyboard).unwrap(), b"ssh-split;ssh-restored");
    assert_eq!(channels.load(Ordering::Relaxed), 1, "one SSH shell");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "keeps native drawer activation keys out of SSH and restores typing when the terminal canvas is clicked"]
async fn native_sftp_file_rows_own_keyboard_focus_until_the_canvas_is_clicked() {
    let (Source::Ssh(ssh), remote, channels, server) = fixture_server(false).await else {
        unreachable!()
    };
    fs::create_dir(remote.path().join("projects")).unwrap();
    fs::write(remote.path().join("note.txt"), []).unwrap();
    let keyboard = remote.path().join("keyboard-input.bin");
    fs::write(&keyboard, []).unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(
        state.path().join("drawer-key-trace"),
        keyboard.to_str().unwrap(),
    )
    .unwrap();
    drive_mixed_gui("sftp-drawer-focus-ui.ps1", &ssh, state.path());
    assert_eq!(fs::read(keyboard).unwrap(), b"canvas-typed");
    assert_eq!(channels.load(Ordering::Relaxed), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "navigates literal remote paths and folder history through the native SFTP drawer"]
async fn native_go_to_folder_recovers_and_preserves_the_listing_on_error() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let parent = remote.path().join("Pilot's $Logs [2026]");
    let child = parent.join("year 1 雪");
    fs::create_dir(remote.path().join("shell directory")).unwrap();
    fs::create_dir_all(&child).unwrap();
    fs::write(child.join("witness.txt"), b"still here").unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    let listings = remote.path().join("listing-trace");
    fs::write(&trace, []).unwrap();
    fs::write(&listings, []).unwrap();
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("sftp-go-to-folder-ui.ps1", &ssh, state.path());

    let wire = fs::read_to_string(listings).unwrap();
    let opened: Vec<_> = wire
        .lines()
        .filter_map(|line| line.strip_prefix("OPENDIR "))
        .collect();
    let parent = "/Pilot's $Logs [2026]";
    let child = "/Pilot's $Logs [2026]/year 1 雪";
    assert_eq!(
        opened,
        [
            "/shell directory",
            parent,
            child,
            "/Pilot's $Logs [2026]/year 1 雪/missing",
            parent,
            child, // Forward while the destination is missing.
            child, // Retry after the server restores it.
            parent,
            "/",
            parent,
            "/",
        ]
    );
    assert_eq!(fs::read(trace).unwrap(), b"after-navigation");
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one shell and one SFTP channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "selects visible SFTP entries and reaches drawer actions at the minimum native window size"]
async fn native_short_sftp_drawer_keeps_entries_and_actions_reachable() {
    let (Source::Ssh(ssh), remote, channels, server) = fixture_server(false).await else {
        unreachable!()
    };
    fs::create_dir(remote.path().join("projects")).unwrap();
    for i in 0..35 {
        fs::write(remote.path().join(format!("entry-{i:02}.txt")), []).unwrap();
    }
    let keyboard = remote.path().join("keyboard-input.bin");
    fs::write(&keyboard, []).unwrap();
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("short-sftp-drawer-ui.ps1", &ssh, state.path());
    assert!(fs::read(keyboard).unwrap().is_empty());
    assert_eq!(
        channels.load(Ordering::Relaxed),
        2,
        "one shell and one SFTP subsystem"
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
#[ignore = "deletes only explicitly confirmed SFTP entries through the native drawer and preserves link targets across reconnect"]
async fn native_delete_keeps_cancelled_entries_links_targets_and_nonempty_folders() {
    let (Source::Ssh(mut ssh), remote, _, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    ssh.player.reconnect = true;
    let root = remote.path();
    let parent = root.join("shell directory");
    fs::create_dir_all(parent.join("full")).unwrap();
    fs::create_dir(parent.join("empty")).unwrap();
    fs::create_dir(parent.join("missing-target")).unwrap();
    for (name, data) in [
        ("keep.txt", "cancelled"),
        ("purge.bin", "temporary"),
        ("race.txt", "changed before confirmation"),
        ("full/child", "untouched"),
    ] {
        fs::write(parent.join(name), data).unwrap();
    }
    directory_junction(&parent, "directory-link", "full");
    directory_junction(&parent, "dangling-link", "missing-target");
    fs::remove_dir(parent.join("missing-target")).unwrap();
    fs::write(root.join("keyboard-input.bin"), []).unwrap();
    fs::write(root.join("delete-trace.jsonl"), []).unwrap();
    fs::write(root.join("omit-listed-mode"), "purge.bin").unwrap();
    fs::write(root.join("disconnect-on-eot"), []).unwrap();
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("delete-entry-ui.ps1", &ssh, state.path());
    assert_eq!(fs::read(parent.join("keep.txt")).unwrap(), b"cancelled");
    assert_eq!(fs::read(parent.join("full/child")).unwrap(), b"untouched");
    assert_eq!(
        fs::read(parent.join("original-race.txt")).unwrap(),
        b"changed before confirmation"
    );
    assert!(parent.join("race.txt").is_dir());
    for removed in ["purge.bin", "directory-link", "dangling-link", "empty"] {
        assert!(
            fs::symlink_metadata(parent.join(removed)).is_err(),
            "{removed}"
        );
    }
    assert_eq!(
        super::remove::trace(root),
        [
            ["RMDIR", "/shell directory/full"],
            ["REMOVE", "/shell directory/purge.bin"],
            ["REMOVE", "/shell directory/directory-link"],
            ["REMOVE", "/shell directory/dangling-link"],
            ["RMDIR", "/shell directory/empty"],
        ]
    );
    assert_eq!(
        fs::read(root.join("keyboard-input.bin")).unwrap(),
        b"\x04delete-ssh-ready"
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
    let mut history = String::from(
        "\x1b[2J\x1b[3J\x1b[H\x1b[32mhistory-start\x1b[0m /srv/game/zone:pilot end\r\n",
    );
    history.push_str("01  731  running\r\n02  42   ready\r\n03  8    stopped\r\n");
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
        br#"{"shortcuts":{"copy_mode":"F8"},"interaction":{"wordDelimiters":":","trimBlockSelection":false}}"#,
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
#[ignore = "opens a new PTY in a bookmarked SFTP directory through the native drawer"]
async fn native_sftp_opens_a_shell_in_the_bookmarked_directory_on_the_original_transport() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, channels, server) =
        fixture_counting_accepts(false, 2, accepted.clone()).await
    else {
        unreachable!()
    };
    let target = remote.path().join("Pilot's Work");
    fs::create_dir_all(target.join("deeper")).unwrap();
    fs::write(target.join("bookmark-witness.txt"), []).unwrap();
    fs::write(target.join("deeper/child-witness.txt"), []).unwrap();
    let inputs = remote.path().join("split-inputs");
    fs::create_dir(&inputs).unwrap();
    for log in ["authenticated-users", "pty-trace", "workspace-exec.bin"] {
        fs::write(remote.path().join(log), []).unwrap();
    }
    fs::write(remote.path().join("hold-opendir"), "/Pilot's Work/deeper").unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(
        state.path().join("peer-root"),
        remote.path().to_str().unwrap(),
    )
    .unwrap();
    drive_mixed_gui("sftp-shell-here-ui.ps1", &ssh, state.path());
    assert_eq!(accepted.load(Ordering::Relaxed), 1, "SSH transports");
    assert_eq!(
        channels.load(Ordering::Relaxed),
        3,
        "two PTYs and one SFTP subsystem"
    );
    let users = fs::read_to_string(remote.path().join("authenticated-users")).unwrap();
    assert_eq!(users.lines().collect::<Vec<_>>(), ["test"]);
    let exec = fs::read_to_string(remote.path().join("workspace-exec.bin")).unwrap();
    assert_eq!(
        exec,
        r#"sh -c 'cd "$1" && exec "${SHELL:-/bin/sh}" -i' w11term '/Pilot'\''s Work'"#
    );
    let trace = fs::read_to_string(remote.path().join("pty-trace")).unwrap();
    let ptys: Vec<_> = trace.lines().collect();
    assert_eq!(ptys.len(), 2);
    assert_ne!(ptys[0], ptys[1]);
    assert_eq!(fs::read_dir(&inputs).unwrap().count(), 2);
    assert_eq!(
        fs::read_to_string(inputs.join(ptys[0])).unwrap(),
        "parent-live"
    );
    assert_eq!(
        fs::read_to_string(inputs.join(ptys[1])).unwrap(),
        "child-live"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens right and below splits from verified OSC 7 paths on real SSH channels"]
async fn native_splits_open_the_reported_directory_on_the_original_ssh_transport() {
    for direction in ["right", "below"] {
        let accepted = Arc::new(AtomicUsize::new(0));
        let (Source::Ssh(ssh), remote, opens, server) =
            fixture_counting_accepts(false, 2, accepted.clone()).await
        else {
            unreachable!()
        };
        let state = tempfile::tempdir().unwrap();
        fs::write(state.path().join("direction"), direction).unwrap();
        let inputs = remote.path().join("split-inputs");
        fs::create_dir(&inputs).unwrap();
        let exec = remote.path().join("workspace-exec.bin");
        fs::write(&exec, []).unwrap();
        drive_mixed_gui("split-ssh-directory-ui.ps1", &ssh, state.path());
        assert_eq!(
            accepted.load(Ordering::Relaxed),
            1,
            "must retain original SSH connection"
        );
        assert_eq!(
            opens.load(Ordering::Relaxed),
            2,
            "exactly one new session channel"
        );
        assert_eq!(
            fs::read_to_string(exec).unwrap(),
            r#"sh -c 'cd "$1" && exec "${SHELL:-/bin/sh}" -i' w11term '/srv/Pilot'\''s Work'"#,
        );
        let mut streams: Vec<_> = fs::read_dir(inputs)
            .unwrap()
            .map(|f| fs::read_to_string(f.unwrap().path()).unwrap())
            .collect();
        streams.sort();
        assert_eq!(
            streams,
            [
                "child-live",
                "report-currentreport-foreignreport-current-againparent-live"
            ]
        );
        server.abort();
    }
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
