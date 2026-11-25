use super::*;
use std::sync::Arc;
use w11term::sftp::{
    FILE_VERIFICATION_LIMIT, FolderChange, FolderComparison, FolderContents, FolderInventory,
    FolderOrigin, Operation, VerifiedFolderFile,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_contents_counts_every_file_within_the_scan_budget_without_reading_data() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let folder = root.join("measure here");
    fs::create_dir_all(folder.join("nested")).unwrap();
    for number in 0..205 {
        fs::write(folder.join(format!("file-{number:03}")), b"x").unwrap();
    }
    fs::write(folder.join("nested/extra"), b"final").unwrap();
    directory_junction(&folder, "link", "nested");
    let trace = root.join("transfer-trace");
    fs::write(&trace, []).unwrap();
    let generation = pane.frames.load().connection_generation;
    let measure = || Operation::MeasureFolder {
        path: "/measure here".into(),
    };
    assert!(guarded_job(&pane, None, measure()).await.is_err());
    let reply = guarded_job(&pane, Some(generation), measure())
        .await
        .unwrap();
    let counts: FolderContents = serde_json::from_value(reply).unwrap();
    assert_eq!(counts.files, 206);
    assert_eq!(counts.logical_bytes, "210");
    assert_eq!(counts.scanned_directories, 2);
    assert_eq!(counts.links, 1);
    assert_eq!(counts.files_without_size, 0);
    assert!(!counts.truncated && !counts.cancelled);

    fs::write(root.join("tree-unknown-size"), []).unwrap();
    let reply = guarded_job(&pane, Some(generation), measure())
        .await
        .unwrap();
    let counts: FolderContents = serde_json::from_value(reply).unwrap();
    assert_eq!(counts.files, 206);
    assert_eq!(counts.files_without_size, 206);
    assert_eq!(counts.logical_bytes, "0");
    assert!(
        fs::read(trace).unwrap().is_empty(),
        "measurement read file data"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_inventory_compares_nested_metadata_without_opening_files_or_links() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let archive = root.join("archive");
    let live = root.join("live");
    for folder in [&archive, &live] {
        fs::create_dir_all(folder.join("nested")).unwrap();
        fs::write(folder.join("unknown.txt"), []).unwrap();
        directory_junction(folder, "link", "nested");
    }
    fs::write(archive.join("nested/data"), b"old").unwrap();
    fs::write(live.join("nested/data"), b"new data").unwrap();
    fs::write(archive.join("discarded"), []).unwrap();
    fs::write(live.join("added"), []).unwrap();
    fs::create_dir(archive.join("replaced")).unwrap();
    fs::write(live.join("replaced"), []).unwrap();
    fs::write(root.join("omit-listed-mtime"), "unknown.txt").unwrap();
    fs::write(root.join("omit-listed-mode"), "link").unwrap();
    let trace = root.join("transfer-trace");
    fs::write(&trace, []).unwrap();
    let generation = pane.frames.load().connection_generation;
    let capture = || Operation::InventoryFolder {
        path: "/archive".into(),
        reference: None,
    };
    assert!(guarded_job(&pane, None, capture()).await.is_err());
    let captured: FolderInventory = serde_json::from_value(
        guarded_job(&pane, Some(generation), capture())
            .await
            .unwrap(),
    )
    .unwrap();
    fs::write(archive.join("nested/data"), b"new data").unwrap();
    let captured = Arc::new(captured);
    let comparison = || Operation::InventoryFolder {
        path: "/live".into(),
        reference: Some(captured.clone()),
    };
    assert!(
        guarded_job(&pane, Some(generation + 1), comparison())
            .await
            .is_err()
    );
    let report: FolderComparison = serde_json::from_value(
        guarded_job(&pane, Some(generation), comparison())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        (&*report.reference_root, &*report.current_root),
        ("/archive", "/live")
    );
    assert_eq!(report.reference_only, 1);
    assert_eq!(report.current_only, 1);
    assert_eq!(report.metadata_changed, 1);
    assert_eq!(report.types_changed, 1);
    assert_eq!(report.unknown, 2);
    assert_eq!(report.matching, 1);
    assert_eq!(report.differences.len(), 6);
    let changed = report
        .differences
        .iter()
        .find(|item| item.path == "nested/data")
        .unwrap();
    assert_eq!(changed.change, FolderChange::Metadata);
    assert_eq!(changed.reference.unwrap().size, Some(3));
    assert_eq!(changed.current.unwrap().size, Some(8));
    assert!(
        report
            .differences
            .iter()
            .all(|item| !item.path.starts_with("link/"))
    );
    assert!(
        fs::read(trace).unwrap().is_empty(),
        "comparison opened a file"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_inventory_refuses_entries_it_cannot_classify() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    fs::create_dir(root.join("tree")).unwrap();
    fs::write(root.join("tree-test-names"), "unavailable").unwrap();
    let generation = pane.frames.load().connection_generation;
    let error = guarded_job(
        &pane,
        Some(generation),
        Operation::InventoryFolder {
            path: "/tree".into(),
            reference: None,
        },
    )
    .await
    .unwrap_err();
    assert!(
        error.contains("did not report the type of unavailable"),
        "{error}"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reveal_compared_file_requires_the_original_folder_and_entry_type() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    fs::create_dir_all(root.join("tree/data")).unwrap();
    let file = root.join("tree/data/test.txt");
    fs::write(&file, b"baseline").unwrap();
    directory_junction(&root.join("tree"), "shortcut", "data");
    let trace = root.join("transfer-trace");
    fs::write(&trace, []).unwrap();
    let generation = pane.frames.load().connection_generation;
    let generation = Some(generation);
    let inventory: FolderInventory = serde_json::from_value(
        guarded_job(
            &pane,
            generation,
            Operation::InventoryFolder {
                path: "/tree".into(),
                reference: None,
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let reveal = |relative: &str| Operation::RevealComparedEntry {
        path: format!("/tree/{relative}"),
        expected: inventory.entries[relative],
    };
    let selected = guarded_job(&pane, generation, reveal("data/test.txt"))
        .await
        .unwrap();
    assert_eq!(selected["directory"], "/tree/data");
    assert_eq!(selected["selected"], "test.txt");
    let selected = guarded_job(&pane, generation, reveal("shortcut"))
        .await
        .unwrap();
    assert_eq!(selected["directory"], "/tree");
    assert_eq!(selected["selected"], "shortcut");
    assert!(
        guarded_job(&pane, None, reveal("data/test.txt"))
            .await
            .is_err()
    );

    fs::write(root.join("tree-redirect"), []).unwrap();
    let error = guarded_job(&pane, generation, reveal("data/test.txt"))
        .await
        .unwrap_err();
    assert!(
        error.contains("containing folder changed") || error.contains("source folder changed"),
        "{error}"
    );
    fs::remove_file(root.join("tree-redirect")).unwrap();
    fs::remove_file(&file).unwrap();
    let error = guarded_job(&pane, generation, reveal("data/test.txt"))
        .await
        .unwrap_err();
    assert!(error.contains("no longer available"), "{error}");
    fs::create_dir(&file).unwrap();
    let error = guarded_job(&pane, generation, reveal("data/test.txt"))
        .await
        .unwrap_err();
    assert!(error.contains("type changed"), "{error}");
    assert!(
        fs::read(trace).unwrap().is_empty(),
        "revealing an entry opened file data"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_compared_files_hashes_both_origins_and_refuses_changed_ancestors() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let root = remote.path();
    for folder in [
        root.join("tree/data"),
        root.join("backup/data"),
        local.path().join("data"),
    ] {
        fs::create_dir_all(folder).unwrap();
    }
    fs::write(root.join("tree/data/file.txt"), b"ready").unwrap();
    fs::write(root.join("backup/data/file.txt"), b"other").unwrap();
    fs::write(local.path().join("data/file.txt"), b"ready").unwrap();
    let generation = Some(pane.frames.load().connection_generation);
    let verify = |origin, reference: &str, relative: &str| Operation::VerifyComparedFile {
        reference_root: reference.into(),
        reference_origin: origin,
        current_root: "/tree".into(),
        relative: relative.into(),
    };
    let remote_reply: VerifiedFolderFile = serde_json::from_value(
        guarded_job(
            &pane,
            generation,
            verify(FolderOrigin::Remote, "/backup", "data/file.txt"),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_ne!(remote_reply.reference_sha256, remote_reply.current_sha256);
    let windows = local.path().to_str().unwrap();
    let windows_reply: VerifiedFolderFile = serde_json::from_value(
        guarded_job(
            &pane,
            generation,
            verify(FolderOrigin::Windows, windows, "data/file.txt"),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(windows_reply.reference_sha256, windows_reply.current_sha256);
    assert_eq!(windows_reply.current_sha256, remote_reply.current_sha256);

    for path in ["../data/file.txt", "/data/file.txt", "data\\file.txt"] {
        assert!(
            guarded_job(
                &pane,
                generation,
                verify(FolderOrigin::Remote, "/backup", path)
            )
            .await
            .is_err()
        );
    }
    directory_junction(local.path(), "link", "data");
    let error = guarded_job(
        &pane,
        generation,
        verify(FolderOrigin::Windows, windows, "link/file.txt"),
    )
    .await
    .unwrap_err();
    assert!(error.contains("link or a non-folder ancestor"), "{error}");
    fs::write(root.join("tree-redirect"), []).unwrap();
    let error = guarded_job(
        &pane,
        generation,
        verify(FolderOrigin::Remote, "/backup", "data/file.txt"),
    )
    .await
    .unwrap_err();
    assert!(error.contains("source folder changed"), "{error}");
    fs::remove_file(root.join("tree-redirect")).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(root.join("tree/data/file.txt"))
        .unwrap()
        .set_len(FILE_VERIFICATION_LIMIT + 1)
        .unwrap();
    let error = guarded_job(
        &pane,
        generation,
        verify(FolderOrigin::Remote, "/backup", "data/file.txt"),
    )
    .await
    .unwrap_err();
    assert!(error.contains("SHA-256 byte limit"), "{error}");
    server.abort();
}
