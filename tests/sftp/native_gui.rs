use super::*;
#[path = "../support/native_gui.rs"]
mod shared;
use shared::run_gui;

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
    assert_eq!(fs::read(remote.path().join("upload.bin")).unwrap(), bytes);
    assert_eq!(fs::read(state.path().join("download.bin")).unwrap(), bytes);
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one shell and one SFTP channel"
    );
    server.abort();
}
