use super::*;
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::sync::oneshot;
use w11term::sftp::{CHUNK, Job, Operation, TransferCancel};

fn start(
    pane: &Session,
    operation: Operation,
    cancel: &TransferCancel,
) -> oneshot::Receiver<Result<Value, String>> {
    let (reply, result) = oneshot::channel();
    pane.input
        .send(Command::Sftp(Job {
            expected_generation: Some(pane.frames.load().connection_generation),
            operation,
            deadline: tokio::time::Instant::now() + Duration::from_secs(10),
            reply,
            progress: None,
            cancel: Some(cancel.clone()),
        }))
        .unwrap();
    result
}

fn download(local: &Path) -> Operation {
    Operation::Get {
        local: local.into(),
        remote: "/source".into(),
        overwrite: true,
        max_bytes: None,
        expected_sha256: None,
        resume: false,
        preserve_mtime: false,
    }
}

fn verified_resume(local: &Path, digest: String) -> Operation {
    Operation::Get {
        local: local.into(),
        remote: "/source".into(),
        overwrite: true,
        max_bytes: None,
        expected_sha256: Some(digest),
        resume: true,
        preserve_mtime: true,
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumable_download_uses_the_checkpoint_after_a_new_ssh_connection() {
    let (source, remote, _, server) = fixture_connections(false, 2).await;
    let log = remote.path().join("transfer-trace");
    let disconnect = remote.path().join("disconnect-tree-read");
    fs::write(&log, []).unwrap();
    fs::write(&disconnect, []).unwrap();
    let data: Vec<u8> = (0..CHUNK * 3 + 11)
        .map(|index| (index % 241) as u8)
        .collect();
    fs::write(remote.path().join("source"), &data).unwrap();
    let modified = set_fixture_modified(&remote.path().join("source"), 1_100_000_000);
    let parent = tempfile::tempdir().unwrap();
    let local = parent.path().join("saved.bin");
    fs::write(&local, b"existing").unwrap();
    let digest = sha256(&data);

    let pane = Session::start(source.clone());
    let connected = |pane: &Session| pane.frames.load().connection_generation > 0;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !connected(&pane) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let error = guarded_job(&pane, None, verified_resume(&local, digest.clone()))
        .await
        .unwrap_err();
    assert!(error.contains("Resumable download interrupted"), "{error}");
    assert_eq!(fs::read(&local).unwrap(), b"existing");
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 2);
    drop(pane);
    fs::remove_file(disconnect).unwrap();
    fs::write(&log, []).unwrap();

    let pane = Session::start(source);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !connected(&pane) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let result = guarded_job(&pane, None, verified_resume(&local, digest.clone()))
        .await
        .unwrap();
    assert_eq!(result["sha256"], digest);
    assert_eq!(result["resumed_from"], CHUNK);
    assert_eq!(result["bytes_transferred"], data.len());
    assert_eq!(fs::read(&local).unwrap(), data);
    assert_eq!(fs::metadata(&local).unwrap().modified().unwrap(), modified);
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
    let log = fs::read_to_string(log).unwrap();
    assert_eq!(
        log.lines().find(|line| line.starts_with("READ ")),
        Some(format!("READ {CHUNK}").as_str())
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumable_download_verifies_a_retained_prefix_and_discards_a_mismatch() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let log = remote.path().join("transfer-trace");
    let hold = remote.path().join("hold-transfer-read");
    fs::write(&log, []).unwrap();
    fs::write(&hold, []).unwrap();
    let data = vec![54; CHUNK * 2 + 1];
    fs::write(remote.path().join("source"), &data).unwrap();
    let parent = tempfile::tempdir().unwrap();
    let local = parent.path().join("saved.bin");
    fs::write(&local, b"existing").unwrap();
    let digest = sha256(&data);
    let cancel = TransferCancel::default();
    let first = start(&pane, verified_resume(&local, digest.clone()), &cancel);
    wait_for_packet(&log, &format!("READ {CHUNK}")).await;
    cancel.cancel();
    fs::remove_file(hold).unwrap();
    let result = first.await.unwrap().unwrap();
    assert_eq!(result["cancelled"], true);
    assert_eq!(result["bytes_transferred"], CHUNK);
    let partial = PathBuf::from(result["partial_file"].as_str().unwrap());
    assert_eq!(fs::read(&partial).unwrap(), &data[..CHUNK]);
    fs::write(&partial, vec![0; CHUNK]).unwrap();

    let error = guarded_job(&pane, None, verified_resume(&local, digest.clone()))
        .await
        .unwrap_err();
    assert!(error.contains("SHA-256 mismatch"), "{error}");
    assert_eq!(fs::read(&local).unwrap(), b"existing");
    assert!(!partial.exists());
    let result = guarded_job(&pane, None, verified_resume(&local, digest))
        .await
        .unwrap();
    assert_eq!(result["resumed_from"], 0);
    assert_eq!(fs::read(&local).unwrap(), data);
    server.abort();
}

fn upload(local: &Path) -> Operation {
    Operation::Put {
        local: local.into(),
        remote: "/destination".into(),
        overwrite: true,
        atomic: false,
        preserve_times: false,
    }
}

fn atomic_upload(local: &Path, remote: &str, overwrite: bool) -> Operation {
    Operation::Put {
        local: local.into(),
        remote: remote.into(),
        overwrite,
        atomic: true,
        preserve_times: false,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn atomic_upload_cancellation_preserves_original_and_success_publishes_only_complete_file() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    fs::write(root.join("posix-rename-version"), "1").unwrap();
    let log = root.join("transfer-trace");
    let gate = root.join("hold-transfer-write");
    fs::write(&log, []).unwrap();
    fs::write(&gate, []).unwrap();
    let destination = root.join("destination");
    fs::write(&destination, b"still live").unwrap();
    let local = tempfile::tempdir().unwrap();
    let source = local.path().join("source");
    let bytes = vec![69; CHUNK * 3 + 37];
    fs::write(&source, &bytes).unwrap();
    let cancel = TransferCancel::default();
    let upload = start(&pane, atomic_upload(&source, "/destination", true), &cancel);
    wait_for_packet(&log, &format!("WRITE {CHUNK}")).await;
    assert_eq!(fs::read(&destination).unwrap(), b"still live");
    cancel.cancel();
    fs::remove_file(gate).unwrap();
    let result = upload.await.unwrap().unwrap();
    assert_eq!(result["cancelled"], true);
    assert_eq!(result["remote_may_be_partial"], false);
    assert_eq!(fs::read(&destination).unwrap(), b"still live");
    assert!(fs::read_dir(root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".w11term-upload-")
    }));
    assert!(!fs::read_to_string(&log).unwrap().contains("POSIX-RENAME"));

    let result = guarded_job(&pane, None, atomic_upload(&source, "/destination", true))
        .await
        .unwrap();
    assert_eq!(result["atomic"], true);
    assert_eq!(fs::read(destination).unwrap(), bytes);
    assert!(fs::read_to_string(log).unwrap().contains("POSIX-RENAME"));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn atomic_upload_without_extension_creates_new_names_and_rejects_replacement() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let source = local.path().join("source");
    fs::write(&source, b"new data").unwrap();
    let destination = remote.path().join("destination");
    assert_eq!(
        guarded_job(&pane, None, atomic_upload(&source, "destination", false))
            .await
            .unwrap()["atomic"],
        true
    );
    assert_eq!(fs::read(&destination).unwrap(), b"new data");
    fs::write(&source, b"must not replace").unwrap();
    for overwrite in [false, true] {
        assert!(
            guarded_job(
                &pane,
                None,
                atomic_upload(&source, "/destination", overwrite)
            )
            .await
            .is_err()
        );
        assert_eq!(fs::read(&destination).unwrap(), b"new data");
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_download_timeout_removes_staging_and_publication_refuses_a_racing_destination() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let tree = remote.path().join("bundle");
    fs::create_dir(&tree).unwrap();
    fs::write(tree.join("data.bin"), vec![42; CHUNK * 3]).unwrap();
    let log = remote.path().join("transfer-trace");
    let hold = remote.path().join("hold-transfer-read");
    let parent = tempfile::tempdir().unwrap();
    let local = parent.path().join("saved bundle");

    for expire in [true, false] {
        fs::write(&log, []).unwrap();
        fs::write(&hold, []).unwrap();
        let (reply, result) = oneshot::channel();
        pane.input
            .send(Command::Sftp(Job {
                expected_generation: Some(pane.frames.load().connection_generation),
                operation: Operation::GetDir {
                    preserve_mtime: false,
                    remote: "/bundle".into(),
                    local: local.clone(),
                },
                deadline: tokio::time::Instant::now()
                    + Duration::from_secs(if expire { 3 } else { 15 }),
                reply,
                progress: None,
                cancel: None,
            }))
            .unwrap();
        wait_for_packet(&log, &format!("READ {CHUNK}")).await;
        assert!(
            !local.exists(),
            "the final name appeared before the last read"
        );
        if expire {
            let failure = tokio::time::timeout(Duration::from_secs(6), result)
                .await
                .unwrap()
                .unwrap();
            assert!(
                failure.is_err(),
                "expired folder download unexpectedly succeeded"
            );
            fs::remove_file(&hold).unwrap();
            assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
        } else {
            fs::create_dir(&local).unwrap();
            fs::write(local.join("mine.txt"), b"existing").unwrap();
            fs::remove_file(&hold).unwrap();
            assert!(
                result.await.unwrap().is_err(),
                "download replaced a concurrent destination"
            );
            assert_eq!(fs::read(local.join("mine.txt")).unwrap(), b"existing");
            assert_eq!(fs::read_dir(&local).unwrap().count(), 1);
            assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 1);
        }
    }
    server.abort();
}

pub(super) async fn wait_for_packet(log: &Path, packet: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fs::read_to_string(log)
            .unwrap()
            .lines()
            .any(|line| line == packet)
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("waiting for {packet}: {}", fs::read_to_string(log).unwrap()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_before_admission_never_opens_a_channel_or_changes_either_file() {
    let (pane, _, remote, opens, server) = fixture(false).await;
    let shell_channels = opens.load(Ordering::Relaxed);
    let local = tempfile::tempdir().unwrap();
    let path = local.path().join("saved");
    fs::write(&path, b"keep local").unwrap();
    let destination = remote.path().join("destination");
    fs::write(&destination, b"keep remote").unwrap();
    let cancel = TransferCancel::default();
    cancel.cancel();
    cancel.cancel();
    for operation in [
        upload(&path),
        download(&path),
        Operation::PutDir {
            local: local.path().into(),
            remote: "/new tree".into(),
            preserve_times: false,
        },
    ] {
        let reply = start(&pane, operation, &cancel).await.unwrap().unwrap();
        assert_eq!(reply["cancelled"], true);
        assert_eq!(reply["bytes_transferred"], 0);
        assert_eq!(reply["remote_may_be_partial"], false);
    }
    assert_eq!(opens.load(Ordering::Relaxed), shell_channels);
    assert!(!remote.path().join("new tree").exists());
    assert_eq!(fs::read(path).unwrap(), b"keep local");
    assert_eq!(fs::read(destination).unwrap(), b"keep remote");
    assert_eq!(fs::read_dir(local.path()).unwrap().count(), 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_inflight_packets_closes_the_handle_and_preserves_unrelated_jobs() {
    let (pane, _, remote, opens, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let path = local.path().join("saved");
    let data: Vec<u8> = (0..CHUNK * 4).map(|i| i as u8).collect();
    fs::write(remote.path().join("source"), &data).unwrap();
    fs::write(&path, b"old local file").unwrap();
    let cli_handle = guarded_job(
        &pane,
        None,
        Operation::Open {
            path: "/source".into(),
            flags: vec!["read".into()],
            attrs: Default::default(),
        },
    )
    .await
    .unwrap()["handle"]
        .as_u64()
        .unwrap();
    let shared_channels = opens.load(Ordering::Relaxed);
    let log = remote.path().join("transfer-trace");
    fs::write(&log, []).unwrap();
    let gate = remote.path().join("hold-transfer-read");
    fs::write(&gate, []).unwrap();
    let active = TransferCancel::default();
    let result = start(&pane, download(&path), &active);
    wait_for_packet(&log, &format!("READ {CHUNK}")).await;
    let queued = TransferCancel::default();
    let later = start(&pane, upload(&path), &queued);
    queued.cancel(); // queued behind the held read
    active.cancel();
    fs::remove_file(gate).unwrap();
    let reply = result.await.unwrap().unwrap();
    assert_eq!(reply["cancelled"], true);
    assert_eq!(reply["bytes_transferred"], CHUNK);
    assert_eq!(reply["remote_may_be_partial"], false);
    assert_eq!(
        later.await.unwrap().unwrap()["remote_may_be_partial"],
        false
    );
    assert_eq!(fs::read(&path).unwrap(), b"old local file");
    assert_eq!(fs::read_dir(local.path()).unwrap().count(), 1);
    assert!(!remote.path().join("destination").exists());
    assert_eq!(
        fs::read_to_string(&log)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["OPEN /source", "READ 0", &format!("READ {CHUNK}"), "CLOSE"]
    );

    fs::write(&log, []).unwrap();
    fs::write(&path, &data).unwrap();
    let gate = remote.path().join("hold-transfer-write");
    fs::write(&gate, []).unwrap();
    let active = TransferCancel::default();
    let result = start(&pane, upload(&path), &active);
    wait_for_packet(&log, &format!("WRITE {CHUNK}")).await;
    active.cancel();
    active.cancel();
    fs::remove_file(gate).unwrap();
    let reply = result.await.unwrap().unwrap();
    assert_eq!(reply["cancelled"], true);
    assert_eq!(reply["remote"], "/destination");
    assert_eq!(reply["bytes_transferred"], CHUNK * 2);
    assert_eq!(reply["remote_may_be_partial"], true);
    assert_eq!(
        fs::read(remote.path().join("destination")).unwrap(),
        &data[..CHUNK * 2]
    );
    assert_eq!(
        fs::read_to_string(&log)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "OPEN /destination",
            "WRITE 0",
            &format!("WRITE {CHUNK}"),
            "CLOSE"
        ]
    );
    assert_eq!(
        guarded_job(&pane, None, Operation::Fstat { handle: cli_handle })
            .await
            .unwrap()["attrs"]["size"],
        data.len()
    );
    guarded_job(&pane, None, Operation::Close { handle: cli_handle })
        .await
        .unwrap();
    assert_eq!(opens.load(Ordering::Relaxed), shared_channels);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_at_close_prevents_local_overwrite_and_preserves_completed_upload_status() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let path = local.path().join("destination");
    fs::write(&path, b"old").unwrap();
    fs::write(remote.path().join("source"), b"new").unwrap();
    let log = remote.path().join("transfer-trace");
    let gate = remote.path().join("hold-transfer-close");
    for (op, should_cancel) in [(download(&path), true), (upload(&path), false)] {
        fs::write(&log, []).unwrap();
        fs::write(&gate, []).unwrap();
        let cancel = TransferCancel::default();
        let result = start(&pane, op, &cancel);
        wait_for_packet(&log, "CLOSE").await;
        cancel.cancel();
        fs::remove_file(&gate).unwrap();
        let reply = result.await.unwrap().unwrap();
        assert_eq!(reply["cancelled"].as_bool().unwrap_or(false), should_cancel);
        assert_eq!(reply["bytes_transferred"], 3);
    }
    assert_eq!(fs::read(&path).unwrap(), b"old");
    assert_eq!(fs::read(remote.path().join("destination")).unwrap(), b"old");
    assert_eq!(fs::read_dir(local.path()).unwrap().count(), 1);
    let last = TransferCancel::default();
    assert_eq!(
        start(&pane, download(&path), &last).await.unwrap().unwrap()["bytes_transferred"],
        3
    );
    last.cancel();
    assert_eq!(fs::read(path).unwrap(), b"new");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_during_cancelled_write_reports_only_acknowledged_upload_bytes() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let path = local.path().join("source");
    fs::write(&path, vec![1u8; CHUNK * 3]).unwrap();
    let log = remote.path().join("transfer-trace");
    let gate = remote.path().join("hold-transfer-write");
    fs::write(&log, []).unwrap();
    fs::write(&gate, []).unwrap();
    fs::write(remote.path().join("disconnect-next-write"), []).unwrap();
    let cancel = TransferCancel::default();
    let result = start(&pane, upload(&path), &cancel);
    wait_for_packet(&log, &format!("WRITE {CHUNK}")).await;
    cancel.cancel();
    fs::remove_file(gate).unwrap();
    let error = result.await.unwrap().unwrap_err();
    assert!(
        error.contains(&format!("{CHUNK} confirmed bytes")),
        "{error}"
    );
    assert!(
        error.contains("/destination") && error.contains("may contain a partial file"),
        "{error}"
    );
    assert_eq!(
        fs::metadata(remote.path().join("destination"))
            .unwrap()
            .len(),
        CHUNK as u64
    );
    server.abort();
}
