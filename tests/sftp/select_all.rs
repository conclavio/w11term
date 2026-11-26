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
    let gate = remote.path().join("hold-shell-output");
    fs::write(&gate, []).unwrap();
    fs::write(state.path().join("output-gate"), gate.to_str().unwrap()).unwrap();
    drive_mixed_gui("select-all-ui.ps1", &ssh, state.path());
    let files = fs::read_dir(input_dir)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(files.len(), 1, "only the fixture shell can receive input");
    let mut expected = b"\x01!".to_vec();
    expected.extend(fs::read(state.path().join("alternate.vt")).unwrap());
    expected.extend_from_slice(b"\x1b[?1049l");
    assert_eq!(
        fs::read(files[0].path()).unwrap(),
        expected,
        "a selection or keyboard command leaked into SSH"
    );
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    server.abort();
}
