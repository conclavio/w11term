use super::*;
use std::path::Path;
use tokio::sync::{oneshot, watch};
use w11term::sftp::{CHUNK, Job, Operation, TransferCancel, TransferProgress};

fn upload(local: &Path, remote: &str) -> Operation {
    Operation::PutDir {
        local: local.into(),
        remote: remote.into(),
    }
}

pub(super) fn seed_source(parent: &Path) {
    let source = parent.join("release 雪");
    fs::create_dir_all(source.join("data 例/empty")).unwrap();
    fs::write(
        source.join("data 例/trace 日本.bin"),
        [0, 1, 128, 255, 13, 10],
    )
    .unwrap();
    fs::write(source.join(".empty"), []).unwrap();
    fs::create_dir(parent.join("outside")).unwrap();
    fs::write(parent.join("outside/unselected"), b"outside tree").unwrap();
    directory_junction(parent, r"release 雪\outside link", "outside");
}

pub(super) fn assert_uploaded_tree(dest: &Path) {
    assert_eq!(
        fs::read(dest.join("data 例/trace 日本.bin")).unwrap(),
        [0, 1, 128, 255, 13, 10]
    );
    assert!(fs::read(dest.join(".empty")).unwrap().is_empty());
    assert_eq!(fs::read_dir(dest.join("data 例/empty")).unwrap().count(), 0);
    assert_eq!(fs::read_dir(dest.join("data 例")).unwrap().count(), 2);
    assert!(!dest.join("outside link").exists());
    assert_eq!(fs::read_dir(dest).unwrap().count(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_folder_preserves_bytes_empty_entries_and_skips_child_junctions() {
    let (pane, _, remote, channels, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    seed_source(local.path());
    let source = local.path().join("release 雪");
    let reply = guarded_job(&pane, None, upload(&source, "/exact destination"))
        .await
        .unwrap();
    assert_eq!(reply["files"], 2);
    assert_eq!(reply["directories"], 3);
    assert_eq!(reply["skipped"], 1);
    assert_eq!(reply["bytes_transferred"], 6);
    assert_eq!(reply["remote"], "/exact destination");
    assert_eq!(reply["resolved_remote"], "/exact destination");
    assert_uploaded_tree(&remote.path().join("exact destination"));

    let err = guarded_job(
        &pane,
        None,
        upload(&source.join("outside link"), "/refused"),
    )
    .await;
    assert!(err.is_err(), "selected junction unexpectedly uploaded");
    assert!(!remote.path().join("refused").exists());
    assert_eq!(
        channels.load(Ordering::Relaxed),
        2,
        "one SSH shell and one SFTP channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_folder_preserves_existing_destinations_and_racing_child_files() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("a.bin"), b"from local").unwrap();
    let dest = remote.path().join("existing");
    fs::create_dir(&dest).unwrap();
    fs::write(dest.join("keep"), b"before").unwrap();
    fs::write(remote.path().join("file"), b"single file").unwrap();
    for path in ["/existing", "/file"] {
        let error = guarded_job(&pane, None, upload(local.path(), path))
            .await
            .unwrap_err();
        assert!(error.contains("already exists"), "{path}: {error}");
    }
    assert_eq!(fs::read(dest.join("keep")).unwrap(), b"before");
    assert_eq!(fs::read_dir(&dest).unwrap().count(), 1);
    assert_eq!(
        fs::read(remote.path().join("file")).unwrap(),
        b"single file"
    );

    // A remote creator wins immediately before the server processes OPEN.
    fs::write(remote.path().join("race-upload-path"), "/raced/a.bin").unwrap();
    let error = guarded_job(&pane, None, upload(local.path(), "/raced"))
        .await
        .unwrap_err();
    assert!(error.contains("a.bin"), "{error}");
    assert_eq!(
        fs::read(remote.path().join("raced/a.bin")).unwrap(),
        b"racing entry"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_folder_cancellation_reports_acknowledged_bytes_and_closes_the_partial_file() {
    let (pane, _, remote, channels, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let payload: Vec<u8> = (0..4 * CHUNK).map(|n| (n % 251) as u8).collect();
    fs::write(local.path().join("large"), &payload).unwrap();
    let log = remote.path().join("transfer-trace");
    let gate = remote.path().join("hold-transfer-write");
    fs::write(&log, []).unwrap();
    fs::write(&gate, []).unwrap();
    let cancel = TransferCancel::default();
    let (progress, observed) = watch::channel(TransferProgress::default());
    let (reply, finished) = oneshot::channel();
    pane.input
        .send(Command::Sftp(Job {
            expected_generation: Some(pane.frames.load().connection_generation),
            operation: upload(local.path(), "/partial"),
            deadline: tokio::time::Instant::now() + Duration::from_secs(15),
            reply,
            progress: Some(progress),
            cancel: Some(cancel.clone()),
        }))
        .unwrap();
    cancellation::wait_for_packet(&log, &format!("WRITE {CHUNK}")).await;
    assert_eq!(observed.borrow().bytes, CHUNK as u64);
    assert_eq!(observed.borrow().files, Some(0));
    cancel.cancel();
    fs::remove_file(gate).unwrap();
    let reply = finished.await.unwrap().unwrap();
    assert_eq!(reply["cancelled"], true);
    assert_eq!(reply["remote_may_be_partial"], true);
    assert_eq!(reply["resolved_remote"], "/partial");
    assert_eq!(reply["files"], 0);
    assert_eq!(reply["directories"], 1);
    assert_eq!(reply["bytes_transferred"], 2 * CHUNK);
    assert_eq!(
        fs::read(remote.path().join("partial/large")).unwrap(),
        &payload[..2 * CHUNK]
    );
    let trace = fs::read_to_string(log).unwrap();
    assert_eq!(trace.lines().last(), Some("CLOSE"), "{trace}");
    assert_eq!(
        guarded_job(&pane, None, Operation::Info).await.unwrap()["open_handles"],
        0
    );
    assert_eq!(
        channels.load(Ordering::Relaxed),
        2,
        "cancellation replaced the SFTP channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_folder_write_error_retains_confirmed_bytes_and_identifies_the_failed_entry() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let bytes = vec![0xcd; CHUNK + 1];
    fs::write(local.path().join("broken.bin"), &bytes).unwrap();
    fs::write(remote.path().join("fail-next-write"), []).unwrap();

    let error = guarded_job(&pane, None, upload(local.path(), "/bundle"))
        .await
        .unwrap_err();
    for detail in ["/bundle/broken.bin", &CHUNK.to_string(), "partial"] {
        assert!(error.contains(detail), "missing {detail:?} in {error:?}");
    }
    assert_eq!(
        fs::read(remote.path().join("bundle/broken.bin")).unwrap(),
        &bytes[..CHUNK]
    );
    assert_eq!(fs::read(local.path().join("broken.bin")).unwrap(), bytes);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_folder_ssh_disconnect_reports_the_partial_destination_and_confirmed_counts() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    fs::write(local.path().join("flight.bin"), vec![0xa7; CHUNK * 3]).unwrap();
    let canonical_parent = remote.path().join("canonical-parent");
    fs::create_dir(&canonical_parent).unwrap();
    fs::write(remote.path().join("disconnect-next-write"), []).unwrap();
    fs::write(remote.path().join("transfer-trace"), []).unwrap();

    let error = guarded_job(&pane, None, upload(local.path(), "alias/lost transfer"))
        .await
        .expect_err("a closed SSH transport cannot complete an upload");
    for detail in [
        "/canonical-parent/lost transfer",
        &format!("{CHUNK} confirmed bytes"),
        "0 complete files",
        "partial folder",
    ] {
        assert!(error.contains(detail), "missing {detail:?} in {error:?}");
    }
    let trace = fs::read_to_string(remote.path().join("transfer-trace")).unwrap();
    assert!(
        trace
            .lines()
            .any(|line| line == "OPEN /canonical-parent/lost transfer/flight.bin"),
        "{trace}"
    );
    assert!(
        trace.lines().any(|line| line == format!("WRITE {CHUNK}")),
        "{trace}"
    );
    assert_eq!(
        fs::read(canonical_parent.join("lost transfer/flight.bin")).unwrap(),
        vec![0xa7; CHUNK]
    );
    server.abort();
}
