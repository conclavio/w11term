use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "reads fresh metadata through the SFTP properties dialog without following links or opening files"]
async fn native_sftp_properties_uses_fresh_metadata_and_keeps_the_selected_entry() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, opens, server) =
        fixture_counting_accepts(false, 1, accepted.clone()).await
    else {
        unreachable!()
    };
    let root = remote.path();
    let parent = root.join("shell directory");
    fs::create_dir_all(parent.join("hangar")).unwrap();
    fs::write(parent.join("pilot 雪.bin"), b"a").unwrap();
    fs::write(parent.join("sparse-meta"), b"no attrs").unwrap();
    directory_junction(&parent, "telemetry-link", "hangar");
    fs::write(root.join("properties-link-target"), "../archive/航/雪").unwrap();
    fs::write(root.join("properties-omit-attrs"), "sparse-meta").unwrap();
    fs::write(root.join("transfer-trace"), []).unwrap();
    fs::write(root.join("keyboard-input.bin"), []).unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(state.path().join("fixture-root"), root.to_str().unwrap()).unwrap();
    drive_mixed_gui("sftp-properties-ui.ps1", &ssh, state.path());
    let requests = fs::read_to_string(root.join("properties-trace")).unwrap();
    assert_eq!(
        requests.lines().collect::<Vec<_>>(),
        [
            "LSTAT /shell directory/pilot 雪.bin",
            "LSTAT /shell directory/pilot 雪.bin",
            "LSTAT /shell directory/hangar",
            "LSTAT /shell directory/telemetry-link",
            "READLINK /shell directory/telemetry-link",
            "LSTAT /shell directory/sparse-meta",
        ]
    );
    assert!(fs::read(root.join("transfer-trace")).unwrap().is_empty());
    assert_eq!(
        fs::read(root.join("keyboard-input.bin")).unwrap(),
        b"ssh-properties-still-live"
    );
    assert_eq!(accepted.load(Ordering::Relaxed), 1);
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "expected one SSH shell and one SFTP subsystem"
    );
    server.abort();
}
