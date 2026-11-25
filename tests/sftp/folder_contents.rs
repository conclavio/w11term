use super::*;
use w11term::sftp::{FolderContents, Operation};

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
