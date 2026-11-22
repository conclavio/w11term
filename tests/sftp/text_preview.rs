use super::*;
use w11term::sftp::{CHUNK, Job, Operation, TEXT_PREVIEW_MAX_BYTES, TransferCancel};

fn preview(path: &str) -> Operation {
    Operation::PreviewText { path: path.into() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preview_uses_the_selected_connection_and_preserves_original_utf8_bytes() {
    let (pane, _, root, opens, server) = fixture(false).await;
    let gen_id = pane.frames.load().connection_generation;
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    for generation in [None, Some(gen_id + 1)] {
        assert!(
            guarded_job(&pane, generation, preview("/config 雪"))
                .await
                .is_err()
        );
    }
    assert_eq!(
        opens.load(Ordering::Relaxed),
        1,
        "invalid generation opened SFTP"
    );
    assert!(
        serde_json::from_value::<Operation>(json!({"op":"preview-text","path":"/test"})).is_err()
    );
    let original = "\u{feff}first\r\nsecond\n雪\t\x1b[31mend";
    fs::write(root.path().join("config 雪"), original).unwrap();
    fs::write(root.path().join("no-fstat"), []).unwrap();
    let trace = root.path().join("transfer-trace");
    fs::write(&trace, []).unwrap();
    let result = guarded_job(&pane, Some(gen_id), preview("/config 雪"))
        .await
        .unwrap();
    assert_eq!(
        result["text"].as_str().unwrap().as_bytes(),
        original.as_bytes()
    );
    assert_eq!(
        fs::read(root.path().join("config 雪")).unwrap(),
        original.as_bytes()
    );
    let log = fs::read_to_string(trace).unwrap();
    assert_eq!(log.lines().next(), Some("OPEN /config 雪"));
    assert_eq!(log.lines().last(), Some("CLOSE"));
    assert!(!log.contains("WRITE"));
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one shell and one SFTP channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preview_enforces_the_limit_on_actual_reads_and_closes_after_errors() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let generation = Some(pane.frames.load().connection_generation);
    let root = remote.path();
    let file = root.join("entry");
    let trace = root.join("transfer-trace");
    // This server omits both sizes and the type bits on FSTAT.
    fs::write(root.join("tree-unknown-size"), []).unwrap();
    fs::write(&file, vec![b'x'; TEXT_PREVIEW_MAX_BYTES]).unwrap();
    let max = guarded_job(&pane, generation, preview("/entry"))
        .await
        .unwrap();
    assert_eq!(max["text"].as_str().unwrap().len(), TEXT_PREVIEW_MAX_BYTES);
    for (bytes, message) in [
        (vec![b'a'; TEXT_PREVIEW_MAX_BYTES + 1], "256 KiB"),
        (b"contains\0NUL".to_vec(), "NUL"),
        (vec![0xff], "UTF-8"),
    ] {
        fs::write(&trace, []).unwrap();
        fs::write(&file, &bytes).unwrap();
        let error = guarded_job(&pane, generation, preview("/entry"))
            .await
            .unwrap_err();
        assert!(error.contains(message), "{error}");
        let log = fs::read_to_string(&trace).unwrap();
        assert!(log.contains("READ 0"));
        assert_eq!(log.lines().last(), Some("CLOSE"));
        assert!(!log.contains("WRITE"));
    }
    fs::create_dir(root.join("folder")).unwrap();
    fs::write(&trace, []).unwrap();
    let error = guarded_job(&pane, generation, preview("/folder"))
        .await
        .unwrap_err();
    assert!(error.contains("regular file"), "{error}");
    assert!(
        fs::read(&trace).unwrap().is_empty(),
        "a directory was opened"
    );
    fs::write(root.join("fstat-nonregular"), []).unwrap();
    let error = guarded_job(&pane, generation, preview("/entry"))
        .await
        .unwrap_err();
    assert!(
        error.contains("opened entry is not a regular file"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        ["OPEN /entry", "CLOSE"]
    );
    fs::write(&trace, []).unwrap();
    fs::write(root.join("omit-lstat-permissions"), []).unwrap();
    let error = guarded_job(&pane, generation, preview("/entry"))
        .await
        .unwrap_err();
    assert!(error.contains("regular file"), "{error}");
    assert!(
        fs::read(&trace).unwrap().is_empty(),
        "unconfirmed file was opened"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_text_preview_closes_the_remote_file_and_keeps_the_subsystem() {
    let (pane, _, remote, opens, server) = fixture(false).await;
    let root = remote.path();
    let generation = Some(pane.frames.load().connection_generation);
    let trace = root.join("transfer-trace");
    let gate = root.join("hold-transfer-read");
    fs::write(root.join("entry"), vec![b'x'; CHUNK * 3]).unwrap();
    fs::write(&trace, []).unwrap();
    fs::write(&gate, []).unwrap();
    let cancel = TransferCancel::default();
    let (reply, result) = tokio::sync::oneshot::channel();
    pane.input
        .send(Command::Sftp(Job {
            expected_generation: generation,
            operation: preview("/entry"),
            deadline: tokio::time::Instant::now() + Duration::from_secs(8),
            reply,
            progress: None,
            cancel: Some(cancel.clone()),
        }))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        let expected = format!("READ {CHUNK}");
        while !fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .any(|line| line == expected)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("waiting for the second remote READ");
    cancel.cancel();
    fs::remove_file(gate).unwrap();
    let error = result.await.unwrap().unwrap_err();
    assert!(error.contains("cancelled"), "{error}");
    let log = fs::read_to_string(trace).unwrap();
    assert_eq!(log.lines().last(), Some("CLOSE"));
    assert!(!log.contains("WRITE"));
    guarded_job(&pane, generation, Operation::Info)
        .await
        .unwrap();
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "cancellation replaced the subsystem"
    );
    server.abort();
}
