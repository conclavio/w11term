use super::*;
use crate::upload_folder::{assert_uploaded_tree, seed_source};
use w11term::sftp::CHUNK;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "uploads and cancels recursive transfers from a native picker alongside a live local shell"]
async fn native_sftp_upload_folder_preserves_tree_collisions_and_both_shells() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, channels, server) =
        fixture_counting_accepts(false, 2, accepted.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let picker = state.path().join("picker");
    fs::create_dir(&picker).unwrap();
    seed_source(&picker);
    let interrupted = picker.join("interrupted/nested");
    fs::create_dir_all(&interrupted).unwrap();
    let payload: Vec<u8> = (0..4 * CHUNK).map(|n| (n % 251) as u8).collect();
    fs::write(interrupted.join("payload.bin"), &payload).unwrap();
    let dest = remote.path().join("shell directory");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("readme.txt"), b"unrelated entry").unwrap();
    for marker in [
        "transfer-trace",
        "hold-transfer-write",
        "keyboard-input.bin",
    ] {
        fs::write(remote.path().join(marker), []).unwrap();
    }
    fs::write(
        state.path().join("fixture-root"),
        remote.path().to_str().unwrap(),
    )
    .unwrap();

    drive_mixed_gui("upload-sftp-folder-ui.ps1", &ssh, state.path());

    assert_uploaded_tree(&dest.join("release 雪"));
    assert_eq!(
        fs::read(dest.join("interrupted/nested/payload.bin")).unwrap(),
        &payload[..2 * CHUNK]
    );
    assert_eq!(
        fs::read(dest.join("readme.txt")).unwrap(),
        b"unrelated entry"
    );
    assert_eq!(fs::read_dir(dest).unwrap().count(), 3);
    let wire = fs::read_to_string(remote.path().join("transfer-trace")).unwrap();
    assert_eq!(
        wire.lines().filter(|l| l.starts_with("OPEN ")).count(),
        3,
        "collision reopened a file: {wire}"
    );
    assert_eq!(
        fs::read(remote.path().join("keyboard-input.bin")).unwrap(),
        b"folder-upload-live"
    );
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        1,
        "unnecessary SSH connection"
    );
    assert_eq!(
        channels.load(Ordering::Relaxed),
        2,
        "one shell and one shared SFTP channel"
    );
    server.abort();
}
