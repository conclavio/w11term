use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "shows cached SFTP sizes, sorts the listing and previews the same file across pages"]
async fn native_sftp_sort_preserves_the_selected_file_across_pages_and_filters() {
    let (Source::Ssh(ssh), remote, opens, server) = fixture_server(false).await else {
        unreachable!()
    };
    let parent = remote.path().join("shell directory");
    fs::create_dir_all(parent.join("zz-review-archive")).unwrap();
    let create = |name: &str, content: &[u8], modified: SystemTime| {
        let mut file = fs::File::create(parent.join(name)).unwrap();
        file.write_all(content).unwrap();
        file.set_modified(modified).unwrap();
    };
    let old = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    create("aa-review.log", b"alphabetical decoy", old);
    let kilobytes = [b'k'; 1536];
    for index in 0..35 {
        let contents: &[u8] = match index {
            0 => &kilobytes,
            1 => b"",
            _ => b"batch",
        };
        create(
            &format!("batch-{index:02}.log"),
            contents,
            old + Duration::from_secs(10 + index),
        );
    }
    let tomorrow = SystemTime::now() + Duration::from_secs(86_400);
    create("yy-review.log", b"second newest", tomorrow);
    create(
        "zz-review-雪.log",
        "selected Unicode file: 雪".as_bytes(),
        tomorrow + Duration::from_secs(60),
    );
    let trace = remote.path().join("listing-trace");
    let wire = remote.path().join("keyboard-input.bin");
    fs::write(&trace, []).unwrap();
    fs::write(&wire, []).unwrap();
    let state = tempfile::tempdir().unwrap();
    fs::write(
        state.path().join("fixture-directory"),
        parent.to_str().unwrap(),
    )
    .unwrap();
    drive_mixed_gui("sort-sftp-listing-ui.ps1", &ssh, state.path());
    let listing = fs::read_to_string(trace).unwrap();
    let mut calls = listing.lines();
    assert_eq!(calls.next(), Some("OPENDIR /shell directory"));
    // One entry per READDIR: 38 files, one directory, then EOF.
    assert_eq!(calls.collect::<Vec<_>>(), vec!["READDIR"; 40]);
    assert_eq!(fs::read(wire).unwrap(), b"ssh-sort-still-live");
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "expected the original SSH shell and SFTP subsystem"
    );
    server.abort();
}
