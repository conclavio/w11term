use super::*;
use w11term::sftp::{Job, Operation, TransferCancel};

fn checksum(path: &str, max_bytes: Option<u64>) -> Operation {
    Operation::Sha256 {
        path: path.into(),
        max_bytes,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_sha256_hashes_exact_bytes_and_enforces_known_or_unknown_sizes() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let trace = root.join("transfer-trace");
    let data: Vec<u8> = (0..70_000).map(|index| (index % 251) as u8).collect();
    let digest = "9dc177c2fde29dea8e7c29f7ddf147b7c449c99d049c62f3aac0a5933ecf76a3";
    fs::write(root.join("file with spaces"), &data).unwrap();
    fs::write(root.join("empty"), []).unwrap();
    fs::create_dir(root.join("folder")).unwrap();
    directory_junction(root, "linked", "folder");
    fs::write(&trace, []).unwrap();

    for path in ["/folder", "/linked", "/file with spaces/"] {
        assert!(
            guarded_job(&pane, None, checksum(path, None))
                .await
                .is_err()
        );
    }
    let error = guarded_job(&pane, None, checksum("/file with spaces", Some(5)))
        .await
        .unwrap_err();
    assert!(error.contains("SHA-256 byte limit"), "{error}");
    assert!(fs::read(&trace).unwrap().is_empty());

    fs::write(root.join("tree-unknown-size"), []).unwrap();
    fs::write(root.join("no-fstat"), []).unwrap();
    let error = guarded_job(&pane, None, checksum("/file with spaces", Some(5)))
        .await
        .unwrap_err();
    assert!(error.contains("SHA-256 byte limit"), "{error}");
    assert_eq!(
        fs::read_to_string(&trace).unwrap().lines().last(),
        Some("CLOSE")
    );
    let reply = guarded_job(&pane, None, checksum("/file with spaces", Some(70_000)))
        .await
        .unwrap();
    assert_eq!(reply, json!({"sha256": digest, "bytes": 70_000}));
    let reply = guarded_job(&pane, None, checksum("/empty", Some(0)))
        .await
        .unwrap();
    assert_eq!(
        reply["sha256"],
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(reply["bytes"], 0);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_sha256_refuses_changed_files_and_closes_cancelled_reads() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let trace = root.join("transfer-trace");
    let gate = root.join("hold-transfer-read");
    let input = root.join("changing");
    for cancel_read in [false, true] {
        fs::write(&input, vec![0; 70_000]).unwrap();
        fs::write(&trace, []).unwrap();
        fs::write(&gate, []).unwrap();
        let cancel = TransferCancel::default();
        let (reply, receiver) = tokio::sync::oneshot::channel();
        pane.input
            .send(Command::Sftp(Job {
                expected_generation: Some(pane.frames.load().connection_generation),
                operation: checksum("/changing", None),
                deadline: tokio::time::Instant::now() + Duration::from_secs(10),
                reply,
                progress: None,
                cancel: Some(cancel.clone()),
            }))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fs::read_to_string(&trace)
                .unwrap_or_default()
                .contains("READ 32768")
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("blocked second SFTP read");
        let expected = if cancel_read {
            cancel.cancel();
            "cancelled"
        } else {
            fs::OpenOptions::new()
                .write(true)
                .open(&input)
                .unwrap()
                .set_len(70_001)
                .unwrap();
            "changed during SHA-256"
        };
        fs::remove_file(&gate).unwrap();
        let error = receiver.await.unwrap().unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert_eq!(
            fs::read_to_string(&trace).unwrap().lines().last(),
            Some("CLOSE")
        );
    }
    let reply = guarded_job(&pane, None, checksum("/changing", None))
        .await
        .unwrap();
    assert_eq!(reply["bytes"], 70_000);
    server.abort();
}
