use super::*;
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
    }
}

fn upload(local: &Path) -> Operation {
    Operation::Put {
        local: local.into(),
        remote: "/destination".into(),
        overwrite: true,
    }
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
