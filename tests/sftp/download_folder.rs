use super::*;
use std::path::Path;
use w11term::sftp::{CHUNK, Operation};

async fn fetch(pane: &Session, remote: &str, local: &Path) -> Result<Value, String> {
    guarded_job(
        pane,
        None,
        Operation::GetDir {
            remote: remote.into(),
            local: local.into(),
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_folder_preserves_tree_and_skips_links_and_special_files() {
    let (pane, _, root, _, server) = fixture(false).await;
    let tree = root.path().join("tree");
    fs::create_dir_all(tree.join("遊び/empty")).unwrap();
    fs::write(tree.join("遊び/.hidden"), b"\0\xff\x7f").unwrap();
    fs::write(tree.join("socket"), b"never read").unwrap();
    fs::create_dir(root.path().join("outside")).unwrap();
    directory_junction(root.path(), r"tree\link", "outside");
    fs::write(root.path().join("tree-unknown-size"), []).unwrap();
    let local = tempfile::tempdir().unwrap();
    let dest = local.path().join("copy");
    let reply = fetch(&pane, "/tree/", &dest).await.unwrap();
    assert_eq!(
        (
            reply["files"].as_u64(),
            reply["directories"].as_u64(),
            reply["skipped"].as_u64()
        ),
        (Some(1), Some(3), Some(2))
    );
    assert_eq!(reply["bytes_transferred"], 3);
    assert_eq!(fs::read(dest.join("遊び/.hidden")).unwrap(), b"\0\xff\x7f");
    assert!(dest.join("遊び/empty").is_dir());
    assert!(!dest.join("link").exists());
    assert!(!dest.join("socket").exists());
    assert!(
        fetch(&pane, "/tree/link/.", &local.path().join("linked"))
            .await
            .is_err()
    );
    assert_eq!(fs::read_dir(local.path()).unwrap().count(), 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_names_duplicates_short_files_and_redirects_never_publish_partial_trees() {
    let (pane, _, root, _, server) = fixture(false).await;
    let tree = root.path().join("tree");
    fs::create_dir(&tree).unwrap();
    fs::write(tree.join("data"), vec![0xab; CHUNK + 1]).unwrap();
    let local = tempfile::tempdir().unwrap();
    let dest = local.path().join("output");
    let list = root.path().join("tree-test-names");
    for (names, expected) in [
        ("data\n../escape", "Windows filename"),
        ("data\nCON.txt", "Windows filename"),
        ("data\nDATA", "colliding"),
        ("data\ndata", "colliding"),
    ] {
        fs::write(&list, names).unwrap();
        let err = fetch(&pane, "/tree", &dest).await.unwrap_err();
        assert!(err.contains(expected), "{err}");
        assert_eq!(fs::read_dir(local.path()).unwrap().count(), 0);
    }
    fs::remove_file(&list).unwrap();
    let deep = (0..33).fold(tree.clone(), |path, _| path.join("d"));
    fs::create_dir_all(&deep).unwrap();
    let err = fetch(&pane, "/tree", &dest).await.unwrap_err();
    assert!(err.contains("levels"), "{err}");
    assert_eq!(fs::read_dir(local.path()).unwrap().count(), 0);
    fs::remove_dir_all(tree.join("d")).unwrap();
    for (marker, expected) in [
        ("tree-early-eof", "ended after"),
        ("tree-redirect", "symlink"),
        ("empty-tree-page", "empty SFTP directory page"),
    ] {
        let marker = root.path().join(marker);
        fs::write(&marker, []).unwrap();
        let err = fetch(&pane, "/tree", &dest).await.unwrap_err();
        assert!(err.contains(expected), "{err}");
        assert_eq!(fs::read_dir(local.path()).unwrap().count(), 0);
        fs::remove_file(marker).unwrap();
    }
    fs::write(root.path().join("disconnect-tree-read"), []).unwrap();
    assert!(fetch(&pane, "/tree", &dest).await.is_err());
    assert_eq!(fs::read_dir(local.path()).unwrap().count(), 0);
    server.abort();
}
