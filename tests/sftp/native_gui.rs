use super::*;
#[path = "../support/drive_gui.rs"]
mod drive;
use drive::{drive_gui, drive_mixed_gui};
#[path = "mixed_drop_ui.rs"]
mod mixed_drop_ui;
#[path = "recording_ui.rs"]
mod recording_ui;
#[path = "replay_controls.rs"]
mod replay_controls;
#[path = "select_all.rs"]
mod select_all;
#[path = "../support/native_gui.rs"]
mod shared;
#[path = "sort_ui.rs"]
mod sort_ui;
#[path = "upload_folder_ui.rs"]
mod upload_folder_ui;
use shared::run_gui;
#[path = "properties_ui.rs"]
mod properties_ui;

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
#[ignore = "searches bookmarked SSH history using the native Windows command palette and UIA"]
async fn native_bookmark_search_reveals_the_selected_history_and_restores_input() {
    let mut history = String::from("Deploy Alpha checkpoint\r\nDeploy Omega checkpoint\r\n");
    for row in 0..200 {
        history.push_str(&format!("deploy-log-{row:03}\r\n"));
    }
    history.push_str("deploy-stream-ready");
    run_palette_fixture(
        "scrollback-bookmark-ui.ps1",
        b"after-bookmark",
        Some(&history),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives pointer boundaries and capture across a live SSH and PowerShell split"]
async fn native_pixel_mouse_leave_follows_the_pointer_and_preserves_capture() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    fs::write(
        remote.path().join("copy-history.vt"),
        b"\x1b[?1003h\x1b[?1016h\r\npixel-ready",
    )
    .unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    fs::write(state.path().join("trace-path"), trace.to_str().unwrap()).unwrap();
    drive_mixed_gui("pixel-pointer-leave-ui.ps1", &ssh, state.path());
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "scrolls a live SSH pane via Windows UI Automation and its pointer rail beside PowerShell"]
async fn native_pane_scrollbar_tracks_async_ssh_output_without_moving_its_neighbor() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let numbered: Vec<_> = (0..200)
        .map(|i| {
            let marker = match i {
                0 => "\x1b]633;E;fixture build\x07\x1b]633;C\x07",
                145 => "\x1b]633;D;0\x07",
                _ => "",
            };
            format!("{marker}wire-{i:03}")
        })
        .collect();
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
#[ignore = "reads back Direct2D Find and command-failure marks in a live SSH and PowerShell split"]
async fn native_find_scrollbar_marks_show_the_current_result_and_clear_with_find() {
    let lines = (0..201)
        .map(|row| match row {
            20 | 100 | 175 => format!("line {row:03} has a rail-witness"),
            40 | 135 => format!("\x1b]133;C\x07line {row:03} failed\x1b]133;D;17\x07"),
            60 => format!("\x1b]133;C\x07line {row:03} succeeded\x1b]133;D;0\x07"),
            155 => format!("\x1b]133;C\x07line {row:03} unknown\x1b]133;D\x07"),
            _ => format!("line {row:03}"),
        })
        .collect::<Vec<_>>();
    let history = format!("\x1b[H\x1b[2J\x1b[3J{}", lines.join("\r\n"));
    run_palette_fixture(
        "find-scrollbar-marks-ui.ps1",
        b"\x1b[?1049h\x1b[Halt-screen-ready\x1b[?1049lmark-search-closed",
        Some(&history),
    )
    .await;
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
#[ignore = "filters OSC 133 and 633 command history through native controls and the CLI"]
async fn native_ssh_command_history_searches_and_copies_the_full_command() {
    let long = format!("deploy {} [Ångström]", "x".repeat(128));
    let encoded = percent_encoding::utf8_percent_encode(&long, percent_encoding::NON_ALPHANUMERIC);
    let line = "y".repeat(90);
    let mut stream = format!(
        "\x1b]133;A\x07$ \x1b]133;B\x07visible deployment prompt\r\n\
         \x1b]133;C;cmdline_url={encoded}\x07archived deploy output \x1b[32m[Ångström]\x1b[0m {line}\r\n\
         \r\nsecond line\r\n\x1b]133;D;17\x07"
    );
    stream.push_str(concat!(
        "\x1b]633;A\x07$ \x1b]633;B\x07wrapped command\r\n",
        "\x1b]633;E;echo first\\x0a  echo second;fixture-nonce\x07\x1b]633;C\x07",
        "multiline output\r\n\x1b]633;D;0\x07"
    ));
    let successful_output = format!(
        "{}[Ångström] appears only in this output",
        "progress details for finding the final line beyond the preview\r\n".repeat(150),
    );
    for (command, output, status) in [
        ("deploy audit", "no status reported", ""),
        ("deploy retry", "retry failed", ";2"),
        ("check-status", successful_output.as_str(), ";0"),
    ] {
        stream.push_str(&format!("\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07{output}\r\n\x1b]133;D{status}\x07"));
    }
    for row in 0..64 {
        stream.push_str(&format!("newer-output-{row:03}\r\n"));
    }
    stream.push_str("history-search-ready\r\n");
    run_palette_fixture(
        "search-command-history-ui.ps1",
        b"deploy auditdeploy retry\x1b[?2004hhistory-paste-ready\r\n\x1b[200~echo first\n  echo second\x1b[201~history-resumed",
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
#[ignore = "samples the visual bell outline on a muted SSH split beside another focused pane"]
async fn native_visual_bell_outlines_only_the_originating_pane() {
    let (Source::Ssh(mut ssh), _remote, shells, server) = fixture_server(false).await else {
        unreachable!()
    };
    ssh.player.mute = true;
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("visual-bell-ui.ps1", &ssh, state.path());
    assert_eq!(shells.load(Ordering::Relaxed), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reads native pointer handles across two SSH splits and an alternate screen"]
async fn native_pointer_shape_is_scoped_to_the_hovered_ssh_pane() {
    let (Source::Ssh(ssh), _remote, shells, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("pointer-shape-ui.ps1", &ssh, state.path());
    assert_eq!(shells.load(Ordering::Relaxed), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "activates Kitty notification buttons in a native inbox with two loopback SSH panes"]
async fn native_kitty_notifications_use_the_originating_ssh_pane() {
    let (Source::Ssh(ssh), remote, shells, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    fs::create_dir(remote.path().join("split-inputs")).unwrap();
    fs::write(remote.path().join("notification-fixture"), []).unwrap();
    fs::write(
        state.path().join("peer-root"),
        remote.path().to_str().unwrap(),
    )
    .unwrap();
    drive_mixed_gui("kitty-notification-ui.ps1", &ssh, state.path());
    assert_eq!(shells.load(Ordering::Relaxed), 2);
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
        ("short-automatic-command", start, "\x1b]133;D;3\x07"),
        ("long-command-started", start, ""),
        ("long-command-finished", "", "\x1b]133;D;23\x07"),
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
#[ignore = "opens the reported PowerShell folder and reveals a diagnostic file beside a live SSH pane"]
async fn native_local_path_hint_selects_the_file_in_explorer() {
    run_palette_fixture("reveal-local-path-ui.ps1", b"ssh-neighbor-ok", None).await;
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
#[ignore = "locks, unlocks, and broadcasts input in a live SSH and PowerShell split through the native UI"]
async fn native_read_only_input_is_per_pane_in_a_mixed_split() {
    run_palette_fixture(
        "read-only-pane-ui.ps1",
        b"resumed'fanout-' + (40+7)\rraw-only",
        None,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "edits saved commands and inserts them into SSH and PowerShell through the native palette"]
async fn native_saved_commands_insert_into_the_focused_shell() {
    run_palette_fixture(
        "saved-commands-ui.ps1",
        b"\x1b[200~'saved-' + (40+7)\x1b[201~",
        Some("\x1b[?2004hcommand-fixture-ready\r\n"),
    )
    .await;
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
    run_palette_fixture(
        "regex-find-ui.ps1",
        b"find-ssh-resumed\x1bOR",
        Some(&history),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens real Find from an accessible or keyboard selection in a mixed SSH and PowerShell window"]
async fn native_find_from_selected_text_uses_a_literal_and_keeps_input_on_the_original_pane() {
    let tail = "n".repeat(100);
    let needle = format!("Issue.[雪]+/{tail}/end!");
    let history = format!(
        "\x1b[H\x1b[2J\x1b[3JKEY[42]\r\nprevious token\r\npreviews token\r\nhard-line-first\r\nhard-line-second\r\n\
        {needle}\r\n{needle}\r\nissue.[雪]+/{tail}/end!\r\nIssueX雪雪/{tail}/end!\r\nselect-find-ready\r\n"
    );
    run_palette_fixture(
        "find-selected-text-ui.ps1",
        b"selected-find-shell-ok",
        Some(&history),
    )
    .await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives a native group across two SSH endpoints and a local PowerShell tab"]
async fn native_ssh_host_group_delivers_to_selected_fresh_prompts() {
    let state = tempfile::tempdir().unwrap();
    let mut runtimes = Vec::new();
    let mut profiles = serde_json::Map::new();
    for (name, use_633) in [("East", false), ("West", true)] {
        let (wire, mut received) = tokio::sync::mpsc::unbounded_channel();
        let (Source::Ssh(ssh), root, opens, server) = fixture_with_wire(
            false,
            if use_633 { 4 } else { 2 },
            Arc::default(),
            Some(wire),
        )
        .await
        else {
            unreachable!()
        };
        let trace = Arc::new(std::sync::Mutex::new(Vec::new()));
        let peer_trace = trace.clone();
        if use_633 {
            let gate = root.path().join("hold-shell-request");
            fs::write(&gate, b"").unwrap();
            fs::write(
                state.path().join("west-shell-gate-path"),
                gate.to_string_lossy().as_bytes(),
            )
            .unwrap();
        }
        let release_preview = state.path().join("release-west-preview");
        let release_paused = state.path().join("release-west-paused");
        let release_away = state.path().join("release-west-away");
        let release_slow = state.path().join("release-west-result");
        let release_recording = state.path().join("release-record-fast");
        let responder = tokio::spawn(async move {
            use in_band_resize::Wire;
            let prompt = if use_633 {
                b"\x1b[?2004h\x1b]633;A\x07west$ \x1b]633;B\x07".as_slice()
            } else {
                b"\x1b]133;A\x07east$ \x1b]133;B\x07".as_slice()
            };
            let mut peer = None;
            let mut connections = 0;
            let mut waiting_interrupt = false;
            while let Some(message) = received.recv().await {
                match message {
                    Wire::Shell(id, handle) => {
                        connections += 1;
                        if connections == 1 || !use_633 || connections == 3 {
                            handle.data(id, prompt).await.unwrap();
                        }
                        peer = Some(handle);
                    }
                    Wire::Window(..) => {}
                    Wire::Input(id, bytes) => {
                        peer_trace.lock().unwrap().extend_from_slice(&bytes);
                        let handle = peer.as_ref().unwrap();
                        let response = match bytes.as_slice() {
                            b"part" => b"part".to_vec(),
                            b"\x04" => {
                                handle.exit_status_request(id, 23).await.unwrap();
                                handle.eof(id).await.unwrap();
                                handle.close(id).await.unwrap();
                                continue;
                            }
                            b"\x03" => {
                                assert!(
                                    waiting_interrupt,
                                    "Ctrl+C arrived outside an interrupt test command"
                                );
                                waiting_interrupt = false;
                                let finish = if use_633 {
                                    b"west interrupted\r\n\x1b]633;D;130\x07".as_slice()
                                } else {
                                    b"east interrupted\r\n\x1b]133;D;130\x07".as_slice()
                                };
                                [finish, prompt].concat()
                            }
                            b"alt" => b"\x1b[?1049h\x1b[Halternate-proof".to_vec(),
                            b"prompt" => [b"\x1b[?1049l\r\n".as_slice(), prompt].concat(),
                            b"drop" => {
                                handle
                                    .disconnect(
                                        russh::Disconnect::ByApplication,
                                        "test reconnect".into(),
                                        "en".into(),
                                    )
                                    .await
                                    .unwrap();
                                continue;
                            }
                            ordinary if ordinary.ends_with(b"\r") => {
                                let line = ordinary.strip_suffix(b"\r").unwrap();
                                let line = if use_633 {
                                    line.strip_prefix(b"\x1b[200~")
                                        .unwrap()
                                        .strip_suffix(b"\x1b[201~")
                                        .unwrap()
                                } else {
                                    line
                                };
                                let line = std::str::from_utf8(line).unwrap();
                                if matches!(line, "interrupt-pair" | "interrupt-both") {
                                    assert!(!waiting_interrupt);
                                    waiting_interrupt = true;
                                    let start = if use_633 {
                                        format!("\r\n\x1b]633;E;{line}\x07\x1b]633;C\x07")
                                    } else {
                                        format!("{line}\r\n\x1b]133;C\x07")
                                    };
                                    handle.data(id, start.into_bytes()).await.unwrap();
                                    continue;
                                }
                                if use_633 && line == "both" {
                                    handle
                                        .data(
                                            id,
                                            b"\r\n\x1b]633;E;both\x07\x1b]633;C\x07".as_slice(),
                                        )
                                        .await
                                        .unwrap();
                                    let finish = [
                                        b"west-accepted both\r\n\x1b]633;D;17\x07".as_slice(),
                                        prompt,
                                    ]
                                    .concat();
                                    let preview_gate = release_preview.clone();
                                    let paused_gate = release_paused.clone();
                                    let away_gate = release_away.clone();
                                    let gate = release_slow.clone();
                                    let handle = handle.clone();
                                    tokio::spawn(async move {
                                        for (gate, text) in [
                                            (preview_gate, b"west live first\r\n".as_slice()),
                                            (paused_gate, b"west while paused\r\n".as_slice()),
                                            (away_gate, b"west while away\r\n".as_slice()),
                                            (gate, finish.as_slice()),
                                        ] {
                                            while !gate.is_file() {
                                                tokio::time::sleep(Duration::from_millis(20)).await;
                                            }
                                            if handle.data(id, text.to_vec()).await.is_err() {
                                                break;
                                            }
                                        }
                                    });
                                    continue;
                                }
                                if use_633 && line == "record-fast" {
                                    let output = format!(
                                        "\r\n\x1b]633;E;record-fast\x07\x1b]633;C\x07{}\r\nwest record tail\x1b]633;D;17\x07\r\n",
                                        recording_burst("west")
                                    );
                                    let response = [output.as_bytes(), prompt].concat();
                                    let gate = release_recording.clone();
                                    let handle = handle.clone();
                                    tokio::spawn(async move {
                                        while !gate.is_file() {
                                            tokio::time::sleep(Duration::from_millis(20)).await;
                                        }
                                        let _ = handle.data(id, response).await;
                                    });
                                    continue;
                                }
                                let complete = if use_633 {
                                    let (output, code) = match line {
                                        "shared-status" => ("fleet-status-ok".to_owned(), 0),
                                        _ => (format!("west-accepted {line}"), 17),
                                    };
                                    format!(
                                        "\r\n\x1b]633;C\x07{output}\r\n\x1b]633;E;{line}\x07\x1b]633;D;{code}\x07"
                                    )
                                } else {
                                    let details = if line == "while-slow" {
                                        format!(
                                            "{}east-report-tail\r\n",
                                            "east-report-line\r\n".repeat(35)
                                        )
                                    } else {
                                        String::new()
                                    };
                                    let output = match line {
                                        "shared-status" => "fleet-status-ok".into(),
                                        "record-fast" => recording_burst("east"),
                                        _ => format!("east-accepted {line}"),
                                    };
                                    format!(
                                        "{line}\r\n\x1b]133;C\x07{output}\r\n{details}\x1b]133;D;0\x07"
                                    )
                                };
                                [complete.as_bytes(), prompt].concat()
                            }
                            _ => continue,
                        };
                        handle.data(id, response).await.unwrap();
                    }
                }
            }
        });
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        runtimes.push((root, opens, server, responder, trace));
    }
    profiles.insert(
        "Local witness".into(),
        json!({
            "backend":"local", "executable":"powershell.exe", "args":["-NoProfile","-NoLogo"]
        }),
    );
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":profiles})).unwrap(),
    )
    .unwrap();
    drive_gui("ssh-host-group-ui.ps1", state.path());
    let expected: [&[u8]; 2] = [
        b"one\rboth\rwhile-slow\rwhile-edit\rwhile-alt\rwhile-reconnecting\rafter-reconnect\rinterrupt-pair\r\x03interrupt-both\r\x03\x04shared-status\rrecord-fast\rrecord-one\rcheck-expected\rcheck-each-host\rcli-expect\r",
        b"\x1b[200~both\x1b[201~\rpartaltpromptdropprompt\x1b[200~after-reconnect\x1b[201~\r\x1b[200~interrupt-pair\x1b[201~\r\x03\x1b[200~interrupt-both\x1b[201~\r\x03\x1b[200~after-reconnect-review\x1b[201~\r\x1b[200~show  fleet-health\x1b[201~\r\x1b[200~snapshot  status\x1b[201~\r\x04\x1b[200~snapshot  status\x1b[201~\r\x1b[200~shared-status\x1b[201~\r\x1b[200~record-fast\x1b[201~\raltprompt\x1b[200~check-each-host\x1b[201~\raltprompt\x1b[200~cli-only  west\x1b[201~\r\x1b[200~cli-expect\x1b[201~\r",
    ];
    for (index, (_, opens, server, responder, trace)) in runtimes.into_iter().enumerate() {
        assert_eq!(
            opens.load(Ordering::Relaxed),
            if index == 0 { 2 } else { 4 }
        );
        assert_eq!(
            *trace.lock().unwrap(),
            expected[index],
            "host index {index}"
        );
        responder.abort();
        server.abort();
    }
}

fn recording_burst(host: &str) -> String {
    (0..80)
        .map(|index| format!("{host} record row {index:02}"))
        .collect::<Vec<_>>()
        .join("\r\n")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reviews two real SSH prompts from a named native host group"]
async fn native_ssh_host_group_opens_filtered_authentication_prompts() {
    let state = tempfile::tempdir().unwrap();
    let (Source::Ssh(mut trust), _trust_root, trust_opens, trust_server) =
        fixture_server(false).await
    else {
        unreachable!()
    };
    trust.host_key = None;
    trust.player.reconnect = false;
    trust.host_key_alias = Some(format!(
        "w11term-native-{}.invalid",
        state.path().file_name().unwrap().to_string_lossy()
    ));
    let (Source::Ssh(mut login), _login_root, login_opens, login_server) =
        fixture_server(false).await
    else {
        unreachable!()
    };
    login.authentication = Authentication::Password;
    login.player.reconnect = false;
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({
            "profiles": {"Trust host": trust, "Password host": login},
            "ssh_host_groups": {"Prompt review": ["Trust host", "Password host"]},
        }))
        .unwrap(),
    )
    .unwrap();

    drive_gui("ssh-host-group-prompts-ui.ps1", state.path());
    assert_eq!(trust_opens.load(Ordering::Relaxed), 0);
    assert_eq!(login_opens.load(Ordering::Relaxed), 0);
    trust_server.abort();
    login_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "stops and closes selected SSH panes from the Windows roster"]
async fn native_ssh_host_group_stops_selected_pending_attempts() {
    let state = tempfile::tempdir().unwrap();
    let mut profiles = serde_json::Map::new();
    let mut peers = Vec::new();
    for (name, attempts, pending) in [
        ("Pending East", 3, true),
        ("Pending West", 1, true),
        ("Live shell", 1, false),
    ] {
        let accepted = Arc::new(AtomicUsize::new(0));
        let (Source::Ssh(mut ssh), root, opens, server) =
            fixture_counting_accepts(false, attempts + 1, accepted.clone()).await
        else {
            unreachable!()
        };
        if name == "Pending East" {
            ssh.authentication = Authentication::Password;
        } else if name == "Pending West" {
            ssh.host_key = None;
            ssh.host_key_alias = Some(format!(
                "w11term-stop-{}.invalid",
                state.path().file_name().unwrap().to_string_lossy()
            ));
        }
        ssh.player.reconnect = true;
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        peers.push((name, attempts, pending, accepted, root, opens, server));
    }
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({
            "profiles": profiles,
            "ssh_host_groups": {"Stop attempts": ["Pending East", "Pending West", "Live shell"]},
        }))
        .unwrap(),
    )
    .unwrap();

    drive_gui("ssh-host-group-stop-ui.ps1", state.path());
    for (name, attempts, pending, accepted, _root, opens, server) in peers {
        assert_eq!(accepted.load(Ordering::Relaxed), attempts, "{name}");
        assert_eq!(
            opens.load(Ordering::Relaxed),
            usize::from(!pending),
            "{name}"
        );
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reviews and disconnects selected SSH shells in a Windows roster"]
async fn native_ssh_host_group_disconnects_reviewed_shells_and_keeps_panes() {
    let state = tempfile::tempdir().unwrap();
    let mut profiles = serde_json::Map::new();
    let mut peers = Vec::new();
    for (name, launches) in [("Fleet East", 3), ("Fleet West", 2), ("Witness", 1)] {
        let (Source::Ssh(ssh), root, opens, server) = fixture_connections(false, launches).await
        else {
            unreachable!()
        };
        fs::write(root.path().join("exit-on-eot"), b"").unwrap();
        if name == "Fleet East" {
            let scrollback: String = (0..64)
                .map(|line| format!("east history {line:02}\r\n"))
                .collect();
            fs::write(root.path().join("copy-history.vt"), scrollback).unwrap();
        }
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        peers.push((name, root, opens, launches, server));
    }
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({
            "profiles": profiles,
            "ssh_host_groups": {"Disconnect review": ["Fleet East", "Fleet West", "Witness"]},
        }))
        .unwrap(),
    )
    .unwrap();

    drive_gui("ssh-host-group-disconnect-ui.ps1", state.path());
    for (name, _root, opens, launches, server) in peers {
        assert_eq!(opens.load(Ordering::Relaxed), launches, "{name}");
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "checks reviewed rank, shell syntax and prompt scope on the native SSH group tab"]
async fn native_ssh_host_group_exports_ranked_variables() {
    use in_band_resize::Wire;

    let state = tempfile::tempdir().unwrap();
    let mut peers = Vec::new();
    let mut profiles = serde_json::Map::new();
    for name in ["Fleet East", "Fleet West", "Witness"] {
        let (wire, mut received) = tokio::sync::mpsc::unbounded_channel();
        let (Source::Ssh(ssh), root, opens, server) =
            fixture_with_wire(false, 1, Arc::default(), Some(wire)).await
        else {
            unreachable!()
        };
        let trace = Arc::new(std::sync::Mutex::new(Vec::new()));
        let inputs = trace.clone();
        let peer = tokio::spawn(async move {
            let prompt = b"\x1b]133;A\x07fleet$ \x1b]133;B\x07";
            let mut handle = None;
            while let Some(message) = received.recv().await {
                match message {
                    Wire::Shell(id, peer) => {
                        peer.data(id, prompt.as_slice()).await.unwrap();
                        handle = Some(peer);
                    }
                    Wire::Window(..) => {}
                    Wire::Input(id, bytes) => {
                        let response = match bytes.as_slice() {
                            b"alt" => b"\x1b[?1049h\x1b[Halternate-proof".to_vec(),
                            b"prompt" => [b"\x1b[?1049l\r\n".as_slice(), prompt].concat(),
                            line if line.ends_with(b"\r") => [
                                line.strip_suffix(b"\r").unwrap(),
                                b"\r\n\x1b]133;C\x07variables-set\r\n\x1b]133;D;0\x07",
                                prompt,
                            ]
                            .concat(),
                            _ => panic!("unexpected input on {name}: {bytes:?}"),
                        };
                        inputs.lock().unwrap().push(bytes);
                        handle.as_ref().unwrap().data(id, response).await.unwrap();
                    }
                }
            }
        });
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        peers.push((name, root, opens, server, peer, trace));
    }
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":profiles})).unwrap(),
    )
    .unwrap();
    drive_gui("ssh-host-group-environment-ui.ps1", state.path());

    for (rank, (name, _root, opens, server, peer, trace)) in peers.into_iter().enumerate() {
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        let mut expected = Vec::new();
        if rank == 1 {
            expected.extend([b"alt".to_vec(), b"prompt".to_vec()]);
        }
        if rank < 2 {
            expected.extend([
                format!(
                    "export POLYSH_NR_SHELLS=2 POLYSH_RANK={rank} POLYSH_NAME='127.0.0.1' POLYSH_DISPLAY_NAME='{name}'\r"
                ).into_bytes(),
                format!(
                    "set -gx POLYSH_NR_SHELLS 2 && set -gx POLYSH_RANK {rank} && set -gx POLYSH_NAME '127.0.0.1' && set -gx POLYSH_DISPLAY_NAME '{name}'\r"
                ).into_bytes(),
            ]);
        }
        assert_eq!(*trace.lock().unwrap(), expected, "{name}");
        peer.abort();
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives two simultaneous native SSH group tabs and their independent live rosters"]
async fn native_ssh_host_group_tabs_keep_live_scopes_independent() {
    use in_band_resize::Wire;

    let state = tempfile::tempdir().unwrap();
    let mut peers = Vec::new();
    let mut profiles = serde_json::Map::new();
    for name in ["East", "West", "North"] {
        let (wire, mut received) = tokio::sync::mpsc::unbounded_channel();
        let (Source::Ssh(ssh), root, opens, server) =
            fixture_with_wire(false, 1, Arc::default(), Some(wire)).await
        else {
            unreachable!()
        };
        let input = Arc::new(std::sync::Mutex::new(Vec::new()));
        let trace = input.clone();
        let release = state.path().join("release-east");
        let release_shared = state.path().join("release-shared");
        let responder = tokio::spawn(async move {
            let prompt = b"\x1b]133;A\x07fixture$ \x1b]133;B\x07";
            let mut peer = None;
            while let Some(event) = received.recv().await {
                match event {
                    Wire::Shell(id, handle) => {
                        handle.data(id, prompt.as_slice()).await.unwrap();
                        peer = Some(handle);
                    }
                    Wire::Window(..) => {}
                    Wire::Input(id, bytes) => {
                        trace.lock().unwrap().extend_from_slice(&bytes);
                        let line = std::str::from_utf8(&bytes)
                            .unwrap()
                            .strip_suffix('\r')
                            .unwrap();
                        let handle = peer.as_ref().unwrap();
                        handle
                            .data(id, format!("{line}\r\n\x1b]133;C\x07").into_bytes())
                            .await
                            .unwrap();
                        let gate = match (name, line) {
                            ("East", _) => Some(&release),
                            ("West", "shared-probe") => Some(&release_shared),
                            _ => None,
                        };
                        if let Some(release) = gate {
                            while !release.is_file() {
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                        }
                        let end = format!("{name} finished {line}\r\n\x1b]133;D;0\x07");
                        handle
                            .data(id, [end.as_bytes(), prompt.as_slice()].concat())
                            .await
                            .unwrap();
                    }
                }
            }
        });
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        peers.push((name, root, opens, server, responder, input));
    }
    let mut lab = Vec::new();
    for index in 1..=28 {
        let name = format!("Lab {index:02}");
        let mut profile = profiles["East"].clone();
        profile["host"] = json!("catalog-only.test");
        profile["port"] = json!(2200 + index);
        profiles.insert(name.clone(), profile);
        lab.push(name);
    }
    lab.extend(["East", "North"].map(str::to_owned));
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({
            "profiles": profiles,
            "ssh_host_groups": {
                "Fleet East": ["East"], "Fleet West": ["West"], "Fleet Mixed": ["East", "North"], "Fleet Lab": lab
            }
        }))
        .unwrap(),
    )
    .unwrap();
    drive_gui("ssh-host-group-tabs-ui.ps1", state.path());
    for (name, _root, opens, server, responder, input) in peers {
        assert_eq!(opens.load(Ordering::Relaxed), 1, "{name}");
        let expected = match name {
            "East" => b"east-probe\rreuse-probe\r".as_slice(),
            "West" => b"west-probe\rshared-probe\r".as_slice(),
            _ => b"".as_slice(),
        };
        assert_eq!(*input.lock().unwrap(), expected, "{name}");
        responder.abort();
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives the native SSH group against an appliance shell without OSC markers"]
async fn native_ssh_group_prompt_matches_fresh_appliance_output() {
    use in_band_resize::Wire;
    let state = tempfile::tempdir().unwrap();
    let (wire, mut received) = tokio::sync::mpsc::unbounded_channel();
    let (Source::Ssh(mut ssh), _root, opens, server) =
        fixture_with_wire(false, 3, Arc::default(), Some(wire)).await
    else {
        unreachable!()
    };
    ssh.group_prompt = Some("rack-[12]>>".to_owned().try_into().unwrap());
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{"Appliance":ssh}})).unwrap(),
    )
    .unwrap();
    let (sent, mut trace) = tokio::sync::mpsc::unbounded_channel();
    let responder = tokio::spawn(async move {
        let mut peer = None;
        while let Some(message) = received.recv().await {
            match message {
                Wire::Shell(id, handle) => {
                    if peer.is_none() {
                        handle.data(id, b"rack-1>> ".as_slice()).await.unwrap();
                    }
                    peer = Some(handle);
                }
                Wire::Window(..) => {}
                Wire::Input(id, bytes) => {
                    sent.send(bytes.clone()).unwrap();
                    let handle = peer.as_ref().unwrap();
                    let response = match bytes.as_slice() {
                        b"show\r" => b"show\r\nappliance-output".as_slice(),
                        b"finish" => b"\r\nrack-2>> ".as_slice(),
                        b"redraw" => b"\r\nrack-1>> ".as_slice(),
                        b"drop" => {
                            handle
                                .disconnect(
                                    russh::Disconnect::ByApplication,
                                    "test reconnect".into(),
                                    "en".into(),
                                )
                                .await
                                .unwrap();
                            continue;
                        }
                        _ => panic!("unexpected appliance input: {bytes:?}"),
                    };
                    handle.data(id, response).await.unwrap();
                }
            }
        }
    });
    drive_gui("ssh-group-prompt-matcher-ui.ps1", state.path());
    let mut observed = Vec::new();
    while let Ok(bytes) = trace.try_recv() {
        observed.extend(bytes);
    }
    assert_eq!(
        observed,
        b"show\rfinishdropredrawfinishredrawdropfinishredrawredraw"
    );
    assert_eq!(opens.load(Ordering::Relaxed), 3);
    responder.abort();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reopens persisted group profiles as selected SSH tabs and edits the group in WinUI"]
async fn native_saved_ssh_host_group_reopens_profiles_after_restart() {
    let state = tempfile::tempdir().unwrap();
    let mut peers = Vec::new();
    let mut profiles = serde_json::Map::new();
    for name in ["East", "West"] {
        let (Source::Ssh(ssh), root, opens, server) = fixture_server(false).await else {
            unreachable!()
        };
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        peers.push((root, opens, server));
    }
    for index in 1..=30 {
        let mut profile = profiles["East"].clone();
        profile["host"] = json!("catalog-only.test");
        profile["port"] = json!(2200 + index);
        profiles.insert(format!("Candidate {index:02}"), profile);
    }
    profiles.insert(
        "Local witness".into(),
        json!({"backend":"local", "executable":"powershell.exe", "args":["-NoProfile"]}),
    );
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":profiles, "other":17})).unwrap(),
    )
    .unwrap();
    drive_gui("saved-ssh-host-group-ui.ps1", state.path());
    for (_, opens, server) in peers {
        assert_eq!(opens.load(Ordering::Relaxed), 1);
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens saved SSH groups in the background with the real CLI and WinUI"]
async fn native_cli_opens_saved_ssh_groups_without_switching_tabs() {
    let state = tempfile::tempdir().unwrap();
    let mut peers = Vec::new();
    let mut profiles = serde_json::Map::new();
    for name in ["West", "East"] {
        let accepted = Arc::new(AtomicUsize::new(0));
        let (Source::Ssh(ssh), root, _, server) =
            fixture_counting_accepts(false, 2, accepted.clone()).await
        else {
            unreachable!()
        };
        fs::write(
            root.path().join("copy-history.vt"),
            b"\x1b]133;A\x07ready$ \x1b]133;B\x07",
        )
        .unwrap();
        let gate = root.path().join("hold-session-open");
        fs::write(&gate, b"").unwrap();
        fs::write(
            state.path().join(format!("{name}-gate")),
            gate.to_str().unwrap(),
        )
        .unwrap();
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        peers.push((root, accepted, server));
    }
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({
            "profiles": profiles,
            "ssh_host_groups": {
                "Fleet Ops": ["West", "East"],
                "Broken Ops": ["West", "not-in-catalog"],
            },
            "ssh_host_group_defaults": {
                "Fleet Ops": {"login_limit":"one", "require_all_ready":true},
            },
        }))
        .unwrap(),
    )
    .unwrap();
    drive_gui("ssh-host-group-cli-open-ui.ps1", state.path());
    for (_, accepted, server) in peers {
        assert_eq!(accepted.load(Ordering::Relaxed), 2);
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens a saved group in a second native window and closes just its sessions"]
async fn native_saved_ssh_group_opens_in_an_independent_window() {
    let state = tempfile::tempdir().unwrap();
    let mut peers = Vec::new();
    let mut profiles = serde_json::Map::new();
    for (name, count) in [("East", 3), ("West", 2)] {
        let (Source::Ssh(ssh), root, opens, server) = fixture_connections(false, count).await
        else {
            unreachable!()
        };
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        peers.push((root, opens, server, count));
    }
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({
            "profiles": profiles,
            "ssh_host_groups": {"Fleet":["West","East"]},
            "ssh_recipient_sets": {"Fleet":{"Canary":["East"]}},
            "ssh_host_group_defaults": {"Fleet":{"require_all_ready":true}}
        }))
        .unwrap(),
    )
    .unwrap();
    drive_gui("ssh-host-group-window-ui.ps1", state.path());
    for (_, opens, server, count) in peers {
        assert_eq!(opens.load(Ordering::Relaxed), count);
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reconnects reviewed failed SSH logins and rejects a stale report review"]
async fn native_ssh_group_notifies_when_watched_logins_settle() {
    let state = tempfile::tempdir().unwrap();
    let mut profiles = serde_json::Map::new();
    let mut gates = serde_json::Map::new();
    let mut peers = Vec::new();
    for name in ["West", "East", "North"] {
        let connections = if name == "West" { 1 } else { 2 };
        let (Source::Ssh(ssh), root, opens, server) = fixture_connections(false, connections).await
        else {
            unreachable!()
        };
        let gate = root.path().join("hold-shell-request");
        fs::write(&gate, []).unwrap();
        if name != "West" {
            fs::write(root.path().join("reject-shell-request"), []).unwrap();
        }
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        gates.insert(name.into(), serde_json::to_value(gate).unwrap());
        peers.push((root, opens, server, connections));
    }
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({
            "profiles": profiles,
            "ssh_host_groups": {"Notices":["West","East","North"]}
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        state.path().join("shell-gates.json"),
        serde_json::to_vec(&gates).unwrap(),
    )
    .unwrap();
    drive_gui("ssh-host-group-login-notice-ui.ps1", state.path());
    for (_, opens, server, connections) in peers {
        assert_eq!(opens.load(Ordering::Relaxed), connections);
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "verifies manual and paced SSH rollouts, queue controls, canary approval, and notices"]
async fn native_ssh_group_bounds_parallel_commands() {
    use in_band_resize::Wire;

    let state = tempfile::tempdir().unwrap();
    let mut peers = Vec::new();
    let mut profiles = serde_json::Map::new();
    let paced_arrivals = Arc::new(std::sync::Mutex::new(HashMap::new()));
    for name in ["West", "East", "North"] {
        let (wire, mut messages) = tokio::sync::mpsc::unbounded_channel();
        let (Source::Ssh(ssh), root, _, server) =
            fixture_with_wire(false, 1, Arc::default(), Some(wire)).await
        else {
            unreachable!()
        };
        let inputs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let trace = inputs.clone();
        let fixtures = state.path().to_owned();
        let arrivals = paced_arrivals.clone();
        let responder = tokio::spawn(async move {
            let prompt = b"\x1b]133;A\x07fixture$ \x1b]133;B\x07";
            let mut peer = None;
            while let Some(wire) = messages.recv().await {
                match wire {
                    Wire::Shell(id, handle) => {
                        handle.data(id, prompt.as_slice()).await.unwrap();
                        peer = Some(handle);
                    }
                    Wire::Input(id, bytes) => {
                        trace.lock().unwrap().extend_from_slice(&bytes);
                        let input = std::str::from_utf8(&bytes).unwrap();
                        if input == "restore-prompt" {
                            peer.as_ref()
                                .unwrap()
                                .data(
                                    id,
                                    [format!("\r\n{name} restored prompt\r\n").as_bytes(), prompt]
                                        .concat(),
                                )
                                .await
                                .unwrap();
                            continue;
                        }
                        let Some(line) = input.strip_suffix('\r') else {
                            continue;
                        };
                        let paced = match line {
                            "pace" => Some("pace"),
                            "verify-pacing" => Some("verify-pacing"),
                            _ => None,
                        };
                        if let Some(command) = paced {
                            arrivals
                                .lock()
                                .unwrap()
                                .insert((command, name), std::time::Instant::now());
                        }
                        if name == "East" && line == "inspect" {
                            assert!(
                                fixtures.join("resume-inspect").is_file(),
                                "East received the command while paused"
                            );
                        }
                        if name == "West" && line.starts_with("canary-") {
                            assert_eq!(line, "canary-ok", "a failed canary reached another host");
                            assert!(
                                fixtures.join("approve-canary-ok").is_file(),
                                "West received the command without approval"
                            );
                        }
                        if name == "East" && line == "verify-override" {
                            assert!(
                                fixtures.join("approve-verify-override").is_file(),
                                "East received the override before West was approved"
                            );
                        }
                        if name == "East" && line == "verify-parallel" {
                            assert!(
                                fixtures.join("release-verify-parallel").is_file(),
                                "East received the command while the serial West slot was occupied"
                            );
                        }
                        let handle = peer.as_ref().unwrap();
                        handle
                            .data(
                                id,
                                format!("{line}\r\n\x1b]133;C\x07{name} started {line}\r\n")
                                    .into_bytes(),
                            )
                            .await
                            .unwrap();
                        if name == "West" && paced.is_none() {
                            let gate = fixtures.join(format!("release-{line}"));
                            while !gate.is_file() {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                        }
                        if name == "East" && matches!(line, "roll" | "canary-ok" | "canary-unknown")
                        {
                            let gate = fixtures.join(format!("release-east-{line}"));
                            while !gate.is_file() {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                        }
                        let status = match (name, line) {
                            (_, "halt-on-failure" | "canary-fails")
                            | ("West", "policy-stop" | "policy-continue" | "policy-canary") => {
                                ";17"
                            }
                            (_, "canary-unknown") => "",
                            _ => ";0",
                        };
                        let end = format!("{name} finished {line}\r\n\x1b]133;D{status}\x07");
                        handle
                            .data(id, [end.as_bytes(), prompt.as_slice()].concat())
                            .await
                            .unwrap();
                    }
                    Wire::Window(..) => {}
                }
            }
        });
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        peers.push((name, root, server, responder, inputs));
    }
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({
            "profiles": profiles,
            "ssh_host_groups": {"Fleet":["West","East","North"]}
        }))
        .unwrap(),
    )
    .unwrap();
    drive_gui("ssh-host-group-command-limit-ui.ps1", state.path());
    let arrivals = paced_arrivals.lock().unwrap();
    for (command, minimum_ms) in [("pace", 4_750), ("verify-pacing", 950)] {
        let east = arrivals[&(command, "East")];
        let west = arrivals[&(command, "West")];
        let elapsed = east.saturating_duration_since(west);
        // Allow for SSH packet delivery jitter. The queue interval has an exact unit test.
        assert!(
            elapsed >= Duration::from_millis(minimum_ms),
            "{command} reached East {elapsed:?} after West"
        );
    }
    drop(arrivals);
    for (name, _root, server, responder, inputs) in peers {
        let expected = match name {
            "West" => {
                b"roll\rkeep-first\rhalt-on-failure\rinspect\rcanary-ok\rxrestore-promptpace\rskip-middle\rdeadline\rverify-override\rverify-parallel\rverify-pacing\rverify-expiry\rpolicy-stop\rpolicy-continue\rpolicy-canary\r"
                    .as_slice()
            }
            "East" => {
                b"roll\rinspect\rcanary-ok\rcanary-fails\rcanary-unknown\rcanary-stale\rpace\rdeadline-approval\rverify-override\rverify-parallel\rverify-pacing\rpolicy-stop\rpolicy-continue\r"
            }
            _ => b"xrestore-promptskip-middle\r",
        };
        assert_eq!(inputs.lock().unwrap().as_slice(), expected, "{name}");
        responder.abort();
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "queues limited launches, bulk reconnects and imported SSH hosts"]
async fn native_reviewed_ssh_group_limits_parallel_logins() {
    let state = tempfile::tempdir().unwrap();
    let mut peers = Vec::new();
    let mut profiles = serde_json::Map::new();
    for (name, needs_password, connections) in [
        ("Login East", true, 3),
        ("Login West", true, 3),
        ("Ready", false, 2),
        ("Imported East", true, 1),
        ("Imported West", true, 1),
    ] {
        let accepted = Arc::new(AtomicUsize::new(0));
        let (Source::Ssh(mut ssh), root, opens, server) =
            fixture_counting_accepts(false, connections, accepted.clone()).await
        else {
            unreachable!()
        };
        if needs_password {
            ssh.authentication = Authentication::Password;
        }
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        peers.push((
            name,
            needs_password,
            connections,
            accepted,
            root,
            opens,
            server,
        ));
    }
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({
            "profiles": profiles,
            "ssh_host_groups": {"Limited":["Login East","Login West","Ready"]}
        }))
        .unwrap(),
    )
    .unwrap();
    drive_gui("ssh-host-group-launch-limit-ui.ps1", state.path());
    for (name, needs_password, connections, accepted, _, opens, server) in peers {
        assert_eq!(accepted.load(Ordering::Relaxed), connections, "{name}");
        assert_eq!(
            opens.load(Ordering::Relaxed),
            connections * usize::from(!needs_password),
            "{name}"
        );
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "previews an SSH range and opens only reviewed saved and one-time sessions through WinUI"]
async fn native_ssh_host_list_import_checks_profiles_and_opens_only_reviewed_hosts() {
    let state = tempfile::tempdir().unwrap();
    let (Source::Ssh(east), _east_root, east_opens, east_server) = fixture_server(false).await
    else {
        unreachable!()
    };
    let (Source::Ssh(west), _west_root, west_opens, west_server) = fixture_server(false).await
    else {
        unreachable!()
    };
    let (Source::Ssh(direct), _direct_root, direct_opens, direct_server) =
        fixture_server(false).await
    else {
        unreachable!()
    };
    fs::write(state.path().join("one-time-port"), direct.port.to_string()).unwrap();
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({ "profiles": {
            "East": east,
            "East alternative": east,
            "West": west,
        }}))
        .unwrap(),
    )
    .unwrap();
    drive_gui("ssh-host-list-import-ui.ps1", state.path());
    assert_eq!(east_opens.load(Ordering::Relaxed), 1);
    assert_eq!(west_opens.load(Ordering::Relaxed), 1);
    assert_eq!(direct_opens.load(Ordering::Relaxed), 1);
    for server in [east_server, west_server, direct_server] {
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives the native group file picker and verifies upload scope, collisions and stale connections"]
async fn native_ssh_host_group_uploads_to_reviewed_connections() {
    let state = tempfile::tempdir().unwrap();
    let mut peers = Vec::new();
    let mut profiles = serde_json::Map::new();
    for name in ["Fleet East", "Fleet West", "Fleet Stale", "Witness"] {
        let (Source::Ssh(ssh), directory, opens, server) =
            fixture_connections(false, if name == "Fleet Stale" { 2 } else { 1 }).await
        else {
            unreachable!()
        };
        fs::create_dir(directory.path().join("incoming")).unwrap();
        if name == "Fleet West" {
            fs::write(
                directory.path().join("incoming/report.bin"),
                b"keep this file",
            )
            .unwrap();
            fs::write(
                directory.path().join("incoming/fleet-source"),
                b"keep the batch collision",
            )
            .unwrap();
        }
        if name == "Fleet Stale" {
            fs::write(directory.path().join("exit-on-eot"), []).unwrap();
        }
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        peers.push((name, directory, opens, server));
    }
    let bytes: Vec<u8> = (0..97_000).map(|offset| (offset % 251) as u8).collect();
    let picker = state.path().join("picker");
    fs::create_dir(&picker).unwrap();
    fs::write(picker.join("fleet-source"), &bytes).unwrap();
    fs::write(picker.join("second-source"), b"next upload").unwrap();
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles": profiles})).unwrap(),
    )
    .unwrap();
    drive_gui("ssh-host-group-upload-ui.ps1", state.path());
    for (name, root, opens, server) in peers {
        let path = root.path().join("incoming/report.bin");
        match name {
            "Fleet East" => assert_eq!(fs::read(path).unwrap(), bytes),
            "Fleet West" => assert_eq!(fs::read(path).unwrap(), b"keep this file"),
            _ => assert!(
                !path.exists(),
                "{name} was outside the confirmed live scope"
            ),
        }
        let first = root.path().join("incoming/fleet-source");
        let second = root.path().join("incoming/second-source");
        match name {
            "Fleet East" => {
                assert_eq!(fs::read(first).unwrap(), bytes);
                assert_eq!(fs::read(second).unwrap(), b"next upload");
            }
            "Fleet West" => {
                assert_eq!(fs::read(first).unwrap(), b"keep the batch collision");
                assert_eq!(fs::read(second).unwrap(), b"next upload");
            }
            _ => assert!(
                !first.exists() && !second.exists(),
                "{name} received a batch file"
            ),
        }
        // One shell plus SFTP on the approved hosts, two shells on the
        // restarted host, and only a shell on the filtered-out witness.
        assert_eq!(
            opens.load(Ordering::Relaxed),
            if name == "Witness" { 1 } else { 2 },
            "{name}"
        );
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives the native SSH group download review, generation checks and cancellation"]
async fn native_ssh_host_group_downloads_to_distinct_files() {
    let state = tempfile::tempdir().unwrap();
    let picker = state.path().join("picker/collected");
    fs::create_dir_all(&picker).unwrap();
    fs::write(picker.join("snapshot.bin"), b"existing").unwrap();
    let east: Vec<u8> = (0..84_000).map(|offset| (offset % 251) as u8).collect();
    let west: Vec<u8> = (0..42_000)
        .map(|offset| (250 - offset % 251) as u8)
        .collect();
    let mut profiles = serde_json::Map::new();
    let mut peers = Vec::new();
    for (name, bytes, channels) in [
        ("Fetch East", Some(east.as_slice()), 2),
        ("Fetch West", Some(west.as_slice()), 2),
        ("Fetch Missing", None, 2),
        ("Fetch Stale", Some(east.as_slice()), 2),
        ("Witness", Some(west.as_slice()), 1),
    ] {
        let (Source::Ssh(ssh), root, opens, server) =
            fixture_connections(false, if name == "Fetch Stale" { 2 } else { 1 }).await
        else {
            unreachable!()
        };
        let reports = root.path().join("reports");
        fs::create_dir(&reports).unwrap();
        if let Some(bytes) = bytes {
            fs::write(reports.join("snapshot.bin"), bytes).unwrap();
        }
        if name == "Fetch West" {
            let archive = root.path().join("archive");
            fs::create_dir(&archive).unwrap();
            fs::write(archive.join("snapshot.bin"), b"archived west").unwrap();
        }
        if name == "Fetch East" {
            fs::write(reports.join("cancel.bin"), &east).unwrap();
            fs::write(root.path().join("transfer-trace"), []).unwrap();
            fs::write(
                state.path().join("east-root"),
                root.path().to_str().unwrap(),
            )
            .unwrap();
        }
        if name == "Fetch Stale" {
            fs::write(root.path().join("exit-on-eot"), []).unwrap();
        }
        profiles.insert(name.into(), serde_json::to_value(ssh).unwrap());
        peers.push((name, root, opens, server, channels));
    }
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles": profiles})).unwrap(),
    )
    .unwrap();
    drive_gui("ssh-host-group-download-ui.ps1", state.path());

    let mut received = Vec::new();
    let mut directories = 0;
    for entry in fs::read_dir(&picker).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            directories += 1;
            assert!(
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("ssh-group-download-")
            );
            for file in fs::read_dir(entry.path()).unwrap() {
                let file = file.unwrap();
                assert!(
                    file.file_name()
                        .to_string_lossy()
                        .ends_with("-snapshot.bin")
                );
                received.push(fs::read(file.path()).unwrap());
            }
        }
    }
    received.sort_by_key(Vec::len);
    assert_eq!(
        received,
        [
            b"archived west".to_vec(),
            west.clone(),
            west,
            east.clone(),
            east
        ]
    );
    assert_eq!(directories, 3);
    assert_eq!(fs::read(picker.join("snapshot.bin")).unwrap(), b"existing");
    for (name, _root, opens, server, channels) in peers {
        assert_eq!(opens.load(Ordering::Relaxed), channels, "{name}");
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "checks actual tab and window titles while native Settings, SSH and PowerShell change the active title"]
async fn native_application_titles_follow_the_focused_shell_and_preserve_a_manual_name() {
    let input = concat!(
        "\x1b]2;background-build\x07",
        "\x1b[22;0t\x1b]2;editor: 雪.txt\x07",
        "\x1b[23;0t",
        "\x1b]2;unfocused-watch\x07",
        "\x1b]2;renamed-build\x07",
        "\x1b]2;\x07",
        "ssh-title-controls-ok"
    );
    run_palette_fixture(
        "application-tab-titles-ui.ps1",
        input.as_bytes(),
        Some("\x1b]2;ssh-seed\x07"),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "names one of two identical SSH splits, navigates by name and reopens its workspace"]
async fn native_pane_names_identify_one_ssh_split_and_survive_workspace_restore() {
    let (Source::Ssh(ssh), remote, shells, server) = fixture_connections(false, 4).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    fs::write(
        remote.path().join("copy-history.vt"),
        b"\x1b]2;shared-application\x07",
    )
    .unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();

    drive_mixed_gui("pane-display-name-ui.ps1", &ssh, state.path());

    assert_eq!(
        fs::read(trace).unwrap(),
        b"named-input/\x1b]2;changed-by-shell\x07restored-input/"
    );
    assert_eq!(
        shells.load(Ordering::Relaxed),
        4,
        "original pair and restored pair only"
    );
    server.abort();
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
#[ignore = "records real SSH output and resizes, then compares timed replay pixels and frame timing"]
async fn native_timed_replay_preserves_ssh_frames_geometry_and_pixels() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let gate = remote.path().join("hold-shell-output");
    fs::write(&gate, []).unwrap();
    fs::write(
        remote.path().join("deferred-output.vt"),
        b"\x1b[?25l\x1b[2J\x1b[H\x1b]2;timed-first\x07FIRST-SSH-FRAME",
    )
    .unwrap();
    fs::write(state.path().join("output-gate"), gate.to_str().unwrap()).unwrap();
    drive_mixed_gui("timed-replay-ui.ps1", &ssh, state.path());
    let live = image::open(state.path().join("live.png"))
        .unwrap()
        .to_rgba8();
    let replay = image::open(state.path().join("replayed.png"))
        .unwrap()
        .to_rgba8();
    assert!(
        live.pixels().any(|p| p.0 == [255, 0, 0, 255]),
        "missing red Kitty image"
    );
    assert_eq!(live.dimensions(), replay.dimensions());
    assert!(
        live.as_raw() == replay.as_raw(),
        "timed replay changed rendered pixels"
    );
    assert_eq!(
        opens.load(Ordering::Relaxed),
        1,
        "replay opened an SSH channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "inspects live SSH modes and geometry from the CLI while PowerShell keeps input focus"]
async fn native_inspect_reads_background_ssh_without_disturbing_powershell() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let wire = remote.path().join("keyboard-input.bin");
    fs::write(&wire, []).unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(state.path().join("ssh-wire"), wire.to_str().unwrap()).unwrap();
    drive_mixed_gui("inspect-pane-ui.ps1", &ssh, state.path());
    assert_eq!(
        opens.load(Ordering::Relaxed),
        1,
        "inspect opened an SSH channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "duplicates a named, zoomed SSH and PowerShell split and verifies two independent sets of sessions"]
async fn native_duplicate_tab_layout_starts_independent_ssh_and_powershell() {
    check_mixed_tab_layout(
        "duplicate-tab-layout-ui.ps1",
        b"source-before/duplicate-background/duplicate-focused/source-after/",
        2,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reopens a named, closed tab layout with new SSH and PowerShell sessions beside a live tab"]
async fn native_reopen_closed_tab_layout_restores_position_focus_and_input_lock() {
    check_mixed_tab_layout(
        "reopen-tab-layout-ui.ps1",
        b"source-before/reopened-after-unlock/",
        2,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "restores a single PowerShell pane beside a live SSH shell after closing the split"]
async fn native_reopen_closed_pane_keeps_the_neighboring_ssh_session() {
    check_mixed_tab_layout(
        "reopen-pane-ui.ps1",
        b"before-local-close/after-local-reopen/",
        1,
    )
    .await;
}

async fn check_mixed_tab_layout(script: &str, expected_input: &[u8], ssh_logins: usize) {
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
    drive_mixed_gui(script, &ssh, state.path());
    assert_eq!(accepted.load(Ordering::Relaxed), ssh_logins, "SSH logins");
    assert_eq!(
        channels.load(Ordering::Relaxed),
        ssh_logins,
        "SSH shell channels"
    );
    assert_eq!(
        fs::read(trace).unwrap(),
        expected_input,
        "SSH input seen by the loopback server"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "balances three live SSH and PowerShell columns and verifies the actual terminal grids"]
async fn native_balance_panes_equalizes_live_columns_without_restarting_either_shell() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, channels, server) =
        fixture_counting_accepts(false, 2, accepted.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let trace = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    drive_mixed_gui("balance-panes-ui.ps1", &ssh, state.path());
    assert_eq!(fs::read(trace).unwrap(), b"balance-ssh-live");
    assert_eq!(accepted.load(Ordering::Relaxed), 1);
    assert_eq!(
        channels.load(Ordering::Relaxed),
        1,
        "balancing opened a new SSH channel"
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
    expected.extend_from_slice(b"\x04\x1b]9;4;1;77\x07\x1b]9;4;0\x07");
    for report in [
        "7501;state=working:progress=24",
        "7501;state=blocked:kind=permission:msg=Q29udGludWU/",
        "7501;state=done",
        "133;A",
        "9;4;1;99",
        "7501;state=clear",
    ] {
        expected.extend(format!("\x1b]{report}\x07").bytes());
    }
    expected.extend_from_slice(b"ssh-alive");
    assert_eq!(fs::read(trace).unwrap(), expected);
    assert_eq!(
        opens.load(Ordering::Relaxed),
        3,
        "two shells, one reconnect"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "exports SSH scrollback and a pane image with the native Save dialog beside PowerShell"]
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
    let before_picker = image::open(directory.join("reference.png"))
        .unwrap()
        .into_rgba8();
    let saved_image = image::open(directory.join("captured.png"))
        .unwrap()
        .into_rgba8();
    assert!(
        before_picker == saved_image,
        "the saved image must match the grid captured before the file picker opened"
    );
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
#[ignore = "previews read-only remote text through the native drawer in a live SSH and PowerShell split"]
async fn native_sftp_text_preview_follows_selection_and_keeps_both_shells_live() {
    let (Source::Ssh(ssh), remote, channels, server) = fixture_server(false).await else {
        unreachable!()
    };
    let parent = remote.path().join("shell directory");
    let child = parent.join("nested");
    fs::create_dir_all(&child).unwrap();
    let name = "build 雪.txt";
    let outer = "first 雪\r\n\tsecond café\r\n\r\nlast line";
    let inner = "different directory\r\ninner 雪";
    let binary = b"private header\0raw body";
    fs::write(parent.join(name), outer.as_bytes()).unwrap();
    fs::write(child.join(name), inner.as_bytes()).unwrap();
    fs::write(child.join("binary.dat"), binary).unwrap();
    let keys = remote.path().join("keyboard-input.bin");
    fs::write(&keys, []).unwrap();
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("sftp-text-preview-ui.ps1", &ssh, state.path());
    assert_eq!(fs::read(parent.join(name)).unwrap(), outer.as_bytes());
    assert_eq!(fs::read(child.join(name)).unwrap(), inner.as_bytes());
    assert_eq!(fs::read(child.join("binary.dat")).unwrap(), binary);
    assert_eq!(fs::read(keys).unwrap(), b"after-preview");
    assert_eq!(
        channels.load(Ordering::Relaxed),
        2,
        "one shell and one SFTP channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "downloads and cancels remote directory trees through the Windows folder picker"]
async fn native_sftp_download_folder_preserves_tree_and_existing_destination() {
    let (Source::Ssh(ssh), remote, channels, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let picker = state.path().join("picker");
    let received = picker.join("receive here");
    let conflict = picker.join("already exists");
    let batch_received = picker.join("batch received");
    let batch_conflict = picker.join("batch conflict");
    let batch_cancelled = picker.join("batch cancelled");
    let batch_background = picker.join("batch background");
    let batch_background_conflict = picker.join("batch background conflict");
    fs::create_dir_all(&received).unwrap();
    fs::create_dir_all(conflict.join("snapshot")).unwrap();
    fs::create_dir_all(&batch_received).unwrap();
    fs::create_dir_all(&batch_conflict).unwrap();
    fs::create_dir_all(&batch_cancelled).unwrap();
    fs::create_dir_all(&batch_background).unwrap();
    fs::create_dir_all(&batch_background_conflict).unwrap();
    fs::write(batch_background_conflict.join("verified.bin"), b"user data").unwrap();
    fs::write(batch_conflict.join("verified.bin"), b"user data").unwrap();
    fs::write(received.join("neighbor.bin"), b"untouched sibling").unwrap();
    fs::write(conflict.join("snapshot/.manifest"), b"local manifest").unwrap();

    let parent = remote.path().join("shell directory");
    let source = parent.join("snapshot");
    fs::create_dir_all(source.join("data 雪/empty")).unwrap();
    let data = [0, 255, 10, 13, 128, 42, 0];
    fs::write(source.join("data 雪/trace 例.bin"), data).unwrap();
    fs::write(source.join(".manifest"), b"remote manifest\n").unwrap();
    let verified = b"verify the downloaded bytes\x00\xff";
    fs::write(parent.join("verified.bin"), verified).unwrap();
    fs::write(parent.join("z-later.bin"), b"later item").unwrap();
    let verified_time = set_fixture_modified(&parent.join("verified.bin"), 1_000_000_010);
    let snapshot_file_time =
        set_fixture_modified(&source.join("data 雪/trace 例.bin"), 1_000_000_008);
    let snapshot_root_time = set_fixture_modified(&source, 1_000_000_004);
    {
        use sha2::{Digest, Sha256};
        fs::write(
            state.path().join("verified.sha256"),
            Sha256::digest(verified)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        )
        .unwrap();
    }
    let interrupted = parent.join("interrupted/nested");
    fs::create_dir_all(&interrupted).unwrap();
    fs::write(
        interrupted.join("payload.bin"),
        vec![42; 3 * w11term::sftp::CHUNK],
    )
    .unwrap();
    for marker in ["transfer-trace", "hold-transfer-read", "keyboard-input.bin"] {
        fs::write(remote.path().join(marker), []).unwrap();
    }
    fs::write(
        state.path().join("fixture-root"),
        remote.path().to_str().unwrap(),
    )
    .unwrap();

    drive_mixed_gui("download-sftp-folder-ui.ps1", &ssh, state.path());
    assert_eq!(
        fs::read(state.path().join("accepted.bin")).unwrap(),
        verified
    );
    assert_eq!(
        fs::metadata(state.path().join("accepted.bin"))
            .unwrap()
            .modified()
            .unwrap(),
        verified_time
    );
    assert!(!state.path().join("rejected.bin").exists());
    assert_eq!(
        fs::read(batch_received.join("verified.bin")).unwrap(),
        verified
    );
    for folder in [&batch_received, &batch_conflict] {
        assert_eq!(
            fs::read(folder.join("snapshot/data 雪/trace 例.bin")).unwrap(),
            data
        );
    }
    assert_eq!(
        fs::read(batch_conflict.join("verified.bin")).unwrap(),
        b"user data"
    );
    assert!(!batch_conflict.join("z-later.bin").exists());
    assert_eq!(fs::read_dir(batch_cancelled).unwrap().count(), 0);
    for folder in [&batch_background, &batch_background_conflict] {
        assert_eq!(
            fs::read(folder.join("interrupted/nested/payload.bin")).unwrap(),
            vec![42; 3 * w11term::sftp::CHUNK]
        );
    }
    assert_eq!(
        fs::read(batch_background.join("verified.bin")).unwrap(),
        verified
    );
    assert_eq!(
        fs::read(batch_background_conflict.join("verified.bin")).unwrap(),
        b"user data"
    );

    let downloaded = received.join("snapshot");
    assert_eq!(
        fs::metadata(&downloaded).unwrap().modified().unwrap(),
        snapshot_root_time
    );
    assert_eq!(
        fs::metadata(downloaded.join("data 雪/trace 例.bin"))
            .unwrap()
            .modified()
            .unwrap(),
        snapshot_file_time
    );
    assert_eq!(
        fs::metadata(batch_received.join("verified.bin"))
            .unwrap()
            .modified()
            .unwrap(),
        verified_time
    );
    assert_eq!(
        fs::read(downloaded.join(".manifest")).unwrap(),
        b"remote manifest\n"
    );
    assert_eq!(
        fs::read(downloaded.join("data 雪/trace 例.bin")).unwrap(),
        data
    );
    assert_eq!(
        fs::read_dir(downloaded.join("data 雪/empty"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(fs::read_dir(downloaded.join("data 雪")).unwrap().count(), 2);
    assert_eq!(fs::read_dir(&downloaded).unwrap().count(), 2);
    assert_eq!(
        fs::read(received.join("neighbor.bin")).unwrap(),
        b"untouched sibling"
    );
    assert_eq!(
        fs::read_dir(&received).unwrap().count(),
        2,
        "interrupted transfer left a tree or staging directory"
    );
    assert_eq!(
        fs::read(conflict.join("snapshot/.manifest")).unwrap(),
        b"local manifest"
    );
    assert_eq!(
        fs::read_dir(conflict.join("snapshot")).unwrap().count(),
        1,
        "existing directory was merged"
    );
    assert_eq!(fs::read_dir(&conflict).unwrap().count(), 1);
    assert_eq!(
        fs::read(remote.path().join("keyboard-input.bin")).unwrap(),
        b"folder-download-live"
    );
    assert_eq!(
        channels.load(Ordering::Relaxed),
        2,
        "one SSH shell and one SFTP channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "monitors and cancels simultaneous SFTP transfers across two native SSH panes"]
async fn native_sftp_transfers_tab_routes_cancellation_and_drawer_focus() {
    let (Source::Ssh(alpha_ssh), alpha, alpha_channels, alpha_server) = fixture_server(false).await
    else {
        unreachable!()
    };
    let (Source::Ssh(beta_ssh), beta, beta_channels, beta_server) = fixture_server(false).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let picker = state.path().join("picker");
    let alpha_target = picker.join("alpha");
    let beta_target = picker.join("beta");
    fs::create_dir_all(&alpha_target).unwrap();
    fs::create_dir_all(&beta_target).unwrap();
    let alpha_source = alpha.path().join("shell directory");
    let beta_source = beta.path().join("shell directory/beta folder");
    fs::create_dir_all(&alpha_source).unwrap();
    fs::create_dir_all(&beta_source).unwrap();
    let size = w11term::sftp::CHUNK * 3 + 7;
    let bytes = vec![2; size];
    fs::write(alpha_source.join("bulk-alpha"), vec![1; size]).unwrap();
    fs::write(alpha_source.join("queued-alpha"), b"queued").unwrap();
    fs::write(beta_source.join("payload"), &bytes).unwrap();
    for (name, root) in [("alpha-root", alpha.path()), ("beta-root", beta.path())] {
        fs::write(state.path().join(name), root.to_str().unwrap()).unwrap();
        for marker in ["transfer-trace", "hold-transfer-read"] {
            fs::write(root.join(marker), []).unwrap();
        }
    }
    fs::write(
        state.path().join("profiles.json"),
        serde_json::to_vec(&json!({"profiles":{"first-ssh":alpha_ssh,"second-ssh":beta_ssh}}))
            .unwrap(),
    )
    .unwrap();
    drive_gui("sftp-transfers-tab-ui.ps1", state.path());
    assert_eq!(fs::read_dir(alpha_target).unwrap().count(), 0);
    assert_eq!(
        fs::read(beta_target.join("beta folder/payload")).unwrap(),
        bytes
    );
    let alpha_trace = fs::read_to_string(alpha.path().join("transfer-trace")).unwrap();
    assert!(alpha_trace.contains("OPEN /shell directory/bulk-alpha"));
    assert!(!alpha_trace.contains("OPEN /shell directory/queued-alpha"));
    assert_eq!(alpha_channels.load(Ordering::Relaxed), 2);
    assert_eq!(beta_channels.load(Ordering::Relaxed), 2);
    alpha_server.abort();
    beta_server.abort();
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
    fs::write(picker.join("atomic-note"), b"new atomic one").unwrap();
    fs::write(parent.join("atomic-note"), b"old atomic one").unwrap();
    fs::write(remote.path().join("posix-rename-version"), "1").unwrap();
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
        5,
        "partial download left a temporary file"
    );
    let trace = fs::read_to_string(remote.path().join("transfer-trace")).unwrap();
    assert_eq!(
        trace.lines().take(12).collect::<Vec<_>>(),
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
        fs::read(parent.join("atomic-note")).unwrap(),
        b"new atomic one"
    );
    let atomic_trace: Vec<_> = trace.lines().skip(12).collect();
    assert!(
        atomic_trace
            .first()
            .is_some_and(|line| line.starts_with("OPEN /shell directory/.w11term-upload-")),
        "{atomic_trace:?}"
    );
    assert!(
        atomic_trace
            .iter()
            .any(|line| line.starts_with("POSIX-RENAME ")
                && line.ends_with("/shell directory/atomic-note")),
        "{atomic_trace:?}"
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
    fs::write(child.join("nested-content.txt"), vec![b'f'; 70_000]).unwrap();
    let fillers = 38;
    for index in 0..fillers {
        fs::write(parent.join(format!("build-{index:02}.log")), []).unwrap();
    }
    set_fixture_modified(&parent.join("build-37.log"), 2_114_380_800);
    fs::write(parent.join("zz-[Ångström].txt"), []).unwrap();
    fs::write(parent.join("zz-Ångström.txt"), []).unwrap();
    let keyboard = remote.path().join("keyboard-input.bin");
    let wire = remote.path().join("listing-trace");
    fs::write(&keyboard, []).unwrap();
    fs::write(&wire, []).unwrap();
    fs::write(remote.path().join("delete-trace.jsonl"), []).unwrap();

    let state = tempfile::tempdir().unwrap();
    fs::write(
        state.path().join("fixture-root"),
        remote.path().to_str().unwrap(),
    )
    .unwrap();
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
    // Each listing returns one entry per READDIR plus EOF. Filtering reads no
    // directory pages. Each recursive search reads the root and child once.
    // Each download of a marked folder lists its subtree once. Selecting a
    // descendant as well does not start a separate operation for that entry.
    assert_eq!(
        listings,
        [
            ("/shell directory", fillers + 4),
            ("/shell directory", fillers + 5),
            ("/shell directory/nested", 2),
            ("/shell directory", fillers + 5),
            ("/shell directory", fillers + 5),
            ("/shell directory/nested", 2),
            ("/shell directory", fillers + 5), // Exclusion skips opening the nested directory.
            ("/shell directory", fillers + 5), // File size alone can select files from subfolders.
            ("/shell directory/nested", 2),
            ("/shell directory", fillers + 5), // A date can be the only search filter.
            ("/shell directory/nested", 2),
            ("/shell directory", fillers + 5), // One scan for the paginated sort fixture.
            ("/shell directory/nested", 2),
            ("/shell directory/nested", 2), // Enter another folder before using the saved search.
            ("/shell directory", fillers + 5), // A saved search reruns from its original root.
            ("/shell directory/nested", 2),
            ("/shell directory", fillers + 5),
            ("/shell directory/nested", 2),
            ("/shell directory/nested", 2),
            ("/shell directory/nested", 2),
            ("/shell directory", fillers + 6), // Progress includes the completed root.
            ("/shell directory/nested", 0),    // Refresh cancelled before reading the subfolder.
            ("/shell directory", fillers + 6), // The refresh sees a newly added file.
            ("/shell directory/nested", 2),
            ("/shell directory", fillers + 6), // Rename refreshes the same search.
            ("/shell directory/nested", 2),
            ("/shell directory", fillers + 5), // Delete refreshes after acknowledgement.
            ("/shell directory/nested", 2),
            ("/shell directory/nested", 1), // Cancelled during the first page.
            ("/shell directory/nested", 2),
        ]
    );
    assert_eq!(
        super::remove::trace(remote.path()),
        [["REMOVE", "/shell directory/nested-renamed.txt"]]
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
#[ignore = "moves and pins a mixed SSH/PowerShell tab with keyboard shortcuts in a real window"]
async fn native_keyboard_tab_reorder_and_pin_preserve_focus_and_live_sessions() {
    run_palette_fixture(
        "keyboard-tab-reorder-ui.ps1",
        b"before;after-right;after-select;while-pinned;last-pane",
        None,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "exercises bulk tab closing with pinned SSH, native PowerShell and Settings"]
async fn native_bulk_close_tabs_keeps_the_pinned_ssh_and_selected_shell() {
    run_palette_fixture("bulk-close-tabs-ui.ps1", b"before/after/", None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "exercises OSC 52 preferences across visible and background native panes"]
async fn native_osc52_scope_admits_only_the_selected_foreground_pane() {
    let (Source::Ssh(ssh), _remote, opens, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("osc52-focus-ui.ps1", &ssh, state.path());
    assert_eq!(opens.load(Ordering::Relaxed), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens an SSH URI from the native Quick Connect form and from the launch CLI"]
async fn native_quick_connect_verifies_the_host_without_saving_a_profile() {
    let (Source::Ssh(ssh), _remote, opens, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("quick-connect-ui.ps1", &ssh, state.path());
    assert_eq!(opens.load(Ordering::Relaxed), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "saves an unsaved SSH pane as a reusable profile without replacing its connection"]
async fn native_saving_an_ssh_profile_keeps_the_original_live_pane() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), _remote, opens, server) =
        fixture_counting_accepts(false, 3, accepted.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("save-live-ssh-profile-ui.ps1", &ssh, state.path());
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "original and fresh profile"
    );
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        2,
        "one login per live pane"
    );
    server.abort();
}

#[test]
#[ignore = "selects SSH key paths through the native file dialog in Quick Connect and a profile"]
fn native_ssh_identity_picker_preserves_draft_and_saves_the_selected_path() {
    let state = tempfile::tempdir().unwrap();
    fs::write(
        state.path().join("profiles.json"),
        br#"{"profiles":{"fixture":{"host":"example.test","user":"test","auth":"identity","identity":"missing-key-folder/original-key"}}}"#,
    )
    .unwrap();
    drive_gui("ssh-identity-picker-ui.ps1", state.path());
}

#[test]
#[ignore = "discovers test WSL registrations and opens them as native ConPTY panes"]
fn native_wsl_profiles_launch_from_the_menu_and_restore_a_workspace() {
    let state = tempfile::tempdir().unwrap();
    drive_gui("wsl-profiles-ui.ps1", state.path());
}

#[test]
#[ignore = "inserts Explorer drop payloads into PowerShell and named WSL panes through ConPTY"]
fn native_shell_path_drops_target_the_right_pane_without_submitting() {
    let state = tempfile::tempdir().unwrap();
    drive_gui("drop-shell-paths-ui.ps1", state.path());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "edits profile colors and live overrides through WinUI, including a saved workspace"]
async fn native_profile_tab_colors_follow_focused_panes_and_new_launches() {
    let (Source::Ssh(ssh), _remote, opens, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    drive_mixed_gui("profile-tab-colors-ui.ps1", &ssh, state.path());
    assert_eq!(opens.load(Ordering::Relaxed), 2);
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
    assert_eq!(
        fs::read(keyboard).unwrap(),
        b"canvas-typedsearch-canvas-typed"
    );
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
    let expected = w11term::workspace::SshStartup::new(
        &ssh,
        "/srv/crew's project".into(),
        "tail -f build.log".into(),
    )
    .unwrap()
    .exec_request();
    assert_eq!(fs::read_to_string(trace).unwrap(), expected);
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
#[ignore = "opens new PTYs from an SFTP bookmark and a recursive search result through the native drawer"]
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
        4,
        "three PTYs and one SFTP subsystem"
    );
    let users = fs::read_to_string(remote.path().join("authenticated-users")).unwrap();
    assert_eq!(users.lines().collect::<Vec<_>>(), ["test"]);
    let exec = fs::read_to_string(remote.path().join("workspace-exec.bin")).unwrap();
    let expected: String = ["/Pilot's Work", "/Pilot's Work/deeper"]
        .into_iter()
        .map(|path| {
            w11term::workspace::SshStartup::new(&ssh, path.into(), String::new())
                .unwrap()
                .exec_request()
        })
        .collect();
    assert_eq!(exec, expected);
    let trace = fs::read_to_string(remote.path().join("pty-trace")).unwrap();
    let ptys: Vec<_> = trace.lines().collect();
    assert_eq!(ptys.len(), 3);
    assert_eq!(
        ptys.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    assert_eq!(fs::read_dir(&inputs).unwrap().count(), 3);
    assert_eq!(
        fs::read_to_string(inputs.join(ptys[0])).unwrap(),
        "parent-live"
    );
    assert_eq!(
        fs::read_to_string(inputs.join(ptys[1])).unwrap(),
        "child-live"
    );
    assert_eq!(
        fs::read_to_string(inputs.join(ptys[2])).unwrap(),
        "search-child-live"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens local PowerShell tabs and splits from real directory reports beside a live SSH shell"]
async fn native_powershell_directory_actions_inherit_cwd_and_preserve_the_mixed_window() {
    run_palette_fixture(
        "split-powershell-directory-ui.ps1",
        b"directory-split-ssh-live",
        None,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens tabs and splits from verified OSC 7 paths on real SSH channels"]
async fn native_ssh_directory_actions_open_on_the_original_transport() {
    for destination in ["right", "below", "tab"] {
        check_ssh_directory_action(destination, false).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens a real SSH tab using paired iTerm2 host and literal directory reports"]
async fn native_iterm2_directory_opens_a_tab_on_the_original_transport() {
    check_ssh_directory_action("tab", true).await;
}

async fn check_ssh_directory_action(destination: &str, iterm2: bool) {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, opens, server) =
        fixture_counting_accepts(false, 2, accepted.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    fs::write(state.path().join("destination"), destination).unwrap();
    if iterm2 {
        fs::write(state.path().join("iterm2"), []).unwrap();
    }
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
    let expected_dir = if iterm2 {
        "/srv/Pilot's 100%2F;#雪"
    } else {
        "/srv/Pilot's Work"
    };
    let expected = w11term::workspace::SshStartup::new(&ssh, expected_dir.into(), String::new())
        .unwrap()
        .exec_request();
    assert_eq!(fs::read_to_string(exec).unwrap(), expected);
    let mut streams: Vec<_> = fs::read_dir(inputs)
        .unwrap()
        .map(|f| fs::read_to_string(f.unwrap().path()).unwrap())
        .collect();
    streams.sort();
    assert_eq!(
        streams,
        [
            "child-live",
            if iterm2 {
                "report-currentnested-hostnested-dirother-loginlogin-hostliteral-dirparent-live"
            } else {
                "report-currentreport-foreignreport-current-againparent-live"
            }
        ]
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "edits UTF-8 text from the SFTP listing and search, and preserves conflicting drafts"]
async fn native_sftp_editor_saves_explicitly_and_recovers_a_conflicting_draft() {
    let (Source::Ssh(ssh), remote, channels, server) = fixture_server(false).await else {
        unreachable!()
    };
    let folder = remote.path().join("shell directory");
    fs::create_dir(&folder).unwrap();
    let source = folder.join("pilot 雪.ini");
    let original = "\u{feff}pilot=one\r\nspeed=slow\nlabel=雪";
    fs::write(&source, original.as_bytes()).unwrap();
    let generated = folder.join("generated");
    fs::create_dir(&generated).unwrap();
    fs::write(generated.join("pilot 雪.ini"), original.as_bytes()).unwrap();
    fs::write(remote.path().join("posix-rename-version"), "1").unwrap();
    let keys = remote.path().join("keyboard-input.bin");
    fs::write(&keys, []).unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(
        state.path().join("fixture-folder"),
        folder.to_str().unwrap(),
    )
    .unwrap();
    drive_mixed_gui("sftp-edit-text-ui.ps1", &ssh, state.path());
    assert_eq!(
        fs::read(&source).unwrap(),
        "\u{feff}pilot=one\r\nspeed=peer\nlabel=雪".as_bytes()
    );
    assert_eq!(
        fs::read(folder.join("draft 雪.ini")).unwrap(),
        "\u{feff}pilot=two\r\nspeed=fast\nlabel=雪".as_bytes()
    );
    assert_eq!(
        fs::read(generated.join("pilot 雪.ini")).unwrap(),
        "\u{feff}pilot=one\r\nspeed=fast\nlabel=雪".as_bytes()
    );
    assert_eq!(fs::read_dir(&generated).unwrap().count(), 1);
    assert_eq!(
        fs::read_dir(&folder).unwrap().count(),
        3,
        "editor left staging files behind"
    );
    assert_eq!(fs::read(keys).unwrap(), b"after-editor");
    assert_eq!(
        channels.load(Ordering::Relaxed),
        2,
        "one SSH shell and one SFTP channel"
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
