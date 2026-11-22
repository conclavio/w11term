use super::*;
use crate::upload_folder::{assert_uploaded_tree, seed_source};
use w11term::sftp::CHUNK;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drops mixed Windows storage items onto the SFTP drawer over an established SSH login"]
async fn native_sftp_mixed_drops_keep_order_stop_at_collisions_and_preserve_both_shells() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), remote, channels, server) =
        fixture_counting_accepts(false, 2, accepted.clone()).await
    else {
        unreachable!()
    };
    let state = tempfile::tempdir().unwrap();
    let source = state.path().join("sources");
    fs::create_dir(&source).unwrap();
    seed_source(&source);
    for (name, bytes) in [
        ("z-lead.bin", &[0u8, 255, 1][..]),
        ("a-tail.bin", &[31, 0, 254]),
        ("replace.bin", b"new"),
        ("before-cancel", b"first"),
        ("after-collision", b"not reached"),
        ("after-cancel", b"not reached"),
        ("after-stale", b"not reached"),
        ("before-invalid", b"not admitted"),
    ] {
        fs::write(source.join(name), bytes).unwrap();
    }
    let payload: Vec<u8> = (0..4 * CHUNK).map(|n| (n % 251) as u8).collect();
    fs::create_dir_all(source.join("interrupted/nested")).unwrap();
    fs::write(source.join("interrupted/nested/payload.bin"), &payload).unwrap();
    fs::write(source.join("before-stale.bin"), &payload).unwrap();
    fs::create_dir(source.join("queued folder")).unwrap();
    fs::write(source.join("queued folder/original"), b"original selection").unwrap();
    let dest = remote.path().join("shell directory");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("readme.txt"), b"keep").unwrap();
    fs::write(dest.join("replace.bin"), b"old remote file").unwrap();
    for marker in ["transfer-trace", "keyboard-input.bin"] {
        fs::write(remote.path().join(marker), []).unwrap();
    }
    fs::write(
        state.path().join("fixture-root"),
        remote.path().to_str().unwrap(),
    )
    .unwrap();

    drive_mixed_gui("mixed-sftp-drop-ui.ps1", &ssh, state.path());

    assert_uploaded_tree(&dest.join("release 雪"));
    assert_eq!(fs::read(dest.join("z-lead.bin")).unwrap(), [0, 255, 1]);
    assert_eq!(fs::read(dest.join("a-tail.bin")).unwrap(), [31, 0, 254]);
    assert_eq!(fs::read(dest.join("replace.bin")).unwrap(), b"new");
    assert_eq!(fs::read(dest.join("readme.txt")).unwrap(), b"keep");
    assert_eq!(fs::read(dest.join("before-cancel")).unwrap(), b"first");
    assert_eq!(
        fs::read(dest.join("interrupted/nested/payload.bin")).unwrap(),
        &payload[..2 * CHUNK]
    );
    assert_eq!(fs::read(dest.join("before-stale.bin")).unwrap(), payload);
    for name in [
        "before-invalid",
        "outside link",
        "after-collision",
        "after-cancel",
        "queued folder",
        "after-stale",
    ] {
        assert!(
            !dest.join(name).exists(),
            "unexpected queued destination {name}"
        );
    }
    let wire = fs::read_to_string(remote.path().join("transfer-trace")).unwrap();
    let opened: Vec<_> = wire
        .lines()
        .filter_map(|line| line.strip_prefix("OPEN /shell directory/"))
        .collect();
    assert_eq!(opened.first(), Some(&"z-lead.bin"), "{wire}");
    assert!(
        opened[1..3]
            .iter()
            .all(|name| name.starts_with("release 雪/")),
        "{wire}"
    );
    assert_eq!(opened.get(3), Some(&"a-tail.bin"), "{wire}");
    // The collided folder was never reopened or merged with the newly added local entry.
    assert_eq!(
        opened
            .iter()
            .filter(|name| name.starts_with("release 雪/"))
            .count(),
        2,
        "{wire}"
    );
    assert_eq!(
        fs::read(remote.path().join("keyboard-input.bin")).unwrap(),
        b"mixed-drop-live"
    );
    assert_eq!(
        channels.load(Ordering::Relaxed),
        2,
        "one shell, one SFTP subsystem"
    );
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        1,
        "new SSH login during drop"
    );
    server.abort();
}
