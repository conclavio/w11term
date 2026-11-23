use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "uses native recording file pickers and opens replay tabs beside live SSH and PowerShell"]
async fn native_recording_dialogs_pin_the_pane_and_share_recorder_state_with_the_cli() {
    let (Source::Ssh(ssh), _remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let recordings = state.path().join("captures");
    fs::create_dir(&recordings).unwrap();
    drive_mixed_gui("recording-ui.ps1", &ssh, state.path());
    assert_eq!(
        opens.load(Ordering::Relaxed),
        1,
        "replay opened an SSH channel"
    );
    server.abort();
}
