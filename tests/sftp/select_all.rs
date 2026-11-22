use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "selects all retained SSH text across soft wraps and separately copies the alternate screen"]
async fn native_select_all_copies_only_the_focused_screen_and_preserves_shell_input() {
    let (Source::Ssh(mut ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    ssh.scrollback_lines = "12".parse().unwrap();
    let state = tempfile::tempdir().unwrap();
    let input_dir = remote.path().join("split-inputs");
    fs::create_dir(&input_dir).unwrap();
    fs::write(state.path().join("input-dir"), input_dir.to_str().unwrap()).unwrap();
    let primary = {
        let mut lines = vec![
            "FIRST-RETAINED-SSH".to_owned(),
            format!("wrap/{}雪🌍é/tail", "x".repeat(57)),
        ];
        lines.extend((1..=20).map(|n| format!("row {n:02}")));
        lines.push("LAST-RETAINED-SSH".to_owned());
        lines.join("\r\n")
    };
    // At 64x12 the wrapped line uses two rows. These last 24 physical rows
    // exactly fill the visible screen and the configured 12 rows of history.
    let history = format!("{}{}", "EVICTED-SSH\r\n".repeat(48), primary);
    let gate = remote.path().join("hold-shell-output");
    fs::write(&gate, []).unwrap();
    fs::write(remote.path().join("deferred-output.vt"), history).unwrap();
    fs::write(state.path().join("output-gate"), gate.to_str().unwrap()).unwrap();
    fs::write(state.path().join("expected-primary.txt"), primary).unwrap();
    let alternate = (1..=12)
        .map(|n| format!("alternate {n:02} 雪"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let enter = format!("\x1b[?1049h\x1b[H{alternate}");
    fs::write(state.path().join("alternate.vt"), &enter).unwrap();
    fs::write(state.path().join("expected-alternate.txt"), &alternate).unwrap();
    drive_mixed_gui("select-all-ui.ps1", &ssh, state.path());
    let files = fs::read_dir(input_dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(files.len(), 1, "only the fixture shell can receive input");
    let mut expected = b"\x01!".to_vec();
    expected.extend_from_slice(enter.as_bytes());
    expected.extend_from_slice(b"\x1b[?1049l");
    assert_eq!(
        fs::read(files[0].path()).unwrap(),
        expected,
        "a selection or keyboard command leaked into SSH"
    );
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    server.abort();
}
