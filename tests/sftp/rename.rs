use super::guarded_job as run;
use super::*;
use w11term::sftp::Operation;

fn rename(from: &str, to: &str) -> Operation {
    Operation::RenameEntry {
        from: from.into(),
        to: to.into(),
        search_kind: None,
    }
}

fn move_entry(from: &str, directory: &str, kind: w11term::sftp::EntryKind) -> Operation {
    Operation::MoveEntry {
        from: from.into(),
        directory: directory.into(),
        kind: Some(kind),
    }
}

fn trace(root: &std::path::Path) -> Vec<[String; 2]> {
    mutation_trace(root, "rename-trace.jsonl")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destination_picker_resolves_one_folder_and_filters_nonfolders() {
    let (pane, _, remote, opens, server) = fixture(false).await;
    let root = remote.path();
    let parent = root.join("canonical-parent");
    fs::create_dir_all(parent.join("z folder")).unwrap();
    fs::create_dir(parent.join("Å archive")).unwrap();
    fs::write(parent.join("file"), b"keep").unwrap();
    directory_junction(&parent, "shortcut", "z folder");
    fs::write(root.join("omit-listed-mode"), "Å archive").unwrap();
    fs::write(root.join("rename-trace.jsonl"), []).unwrap();
    let generation = pane.frames.load().connection_generation;
    let operation = || Operation::BrowseMoveFolders {
        path: "alias".into(),
    };
    let channels = opens.load(Ordering::Relaxed);
    for missing in [None, Some(generation + 1)] {
        assert!(run(&pane, missing, operation()).await.is_err());
        assert_eq!(opens.load(Ordering::Relaxed), channels);
    }
    assert_eq!(
        run(&pane, Some(generation), operation()).await.unwrap(),
        json!({"directory":"/canonical-parent","folders":["z folder","Å archive"]})
    );
    assert_eq!(fs::read(parent.join("file")).unwrap(), b"keep");
    assert!(trace(root).is_empty());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_rename_rechecks_the_source_kind_and_parent() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let generation = Some(pane.frames.load().connection_generation);
    fs::write(root.join("rename-trace.jsonl"), []).unwrap();
    fs::create_dir(root.join("folder")).unwrap();
    fs::write(root.join("folder/source"), b"preserved").unwrap();
    let rename = |to: &str, kind| Operation::RenameEntry {
        from: "/folder/source".into(),
        to: to.into(),
        search_kind: Some(kind),
    };
    let wrong_type = run(
        &pane,
        generation,
        rename("/folder/new", w11term::sftp::EntryKind::Directory),
    )
    .await
    .unwrap_err();
    assert!(wrong_type.contains("entry type changed"), "{wrong_type}");
    let wrong_folder = run(
        &pane,
        generation,
        rename("/elsewhere", w11term::sftp::EntryKind::File),
    )
    .await
    .unwrap_err();
    assert!(
        wrong_folder.contains("containing folder changed"),
        "{wrong_folder}"
    );
    assert!(trace(root).is_empty());
    run(
        &pane,
        generation,
        rename("/folder/new", w11term::sftp::EntryKind::File),
    )
    .await
    .unwrap();
    assert_eq!(fs::read(root.join("folder/new")).unwrap(), b"preserved");
    assert!(!root.join("folder/source").exists());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drawer_rename_checks_generation_and_preserves_colliding_entries() {
    assert!(
        serde_json::from_value::<Operation>(
            json!({"op":"rename-entry","from":"/source","to":"/new"})
        )
        .is_err()
    );
    let (pane, _, remote, opens, server) = fixture(false).await;
    let root = remote.path();
    let generation = pane.frames.load().connection_generation;
    fs::write(root.join("rename-trace.jsonl"), []).unwrap();
    fs::write(root.join("source"), b"original").unwrap();
    fs::write(root.join("existing"), b"keep").unwrap();

    let channels = opens.load(Ordering::Relaxed);
    for expected in [None, Some(generation + 1)] {
        assert!(
            run(&pane, expected, rename("/source", "/renamed"))
                .await
                .unwrap_err()
                .contains("not started")
        );
        assert_eq!(opens.load(Ordering::Relaxed), channels);
    }
    let approved = Some(generation);
    assert!(
        run(&pane, approved, rename("/source", "/existing"))
            .await
            .unwrap_err()
            .contains("already exists")
    );
    assert_eq!(fs::read(root.join("existing")).unwrap(), b"keep");
    fs::write(root.join("deny-rename-check"), []).unwrap();
    let error = run(&pane, approved, rename("/source", "/denied"))
        .await
        .unwrap_err();
    assert!(
        error.contains("Rename was not started") && error.contains("check the destination"),
        "{error}"
    );
    fs::remove_file(root.join("deny-rename-check")).unwrap();
    assert!(trace(root).is_empty());

    // A second creator wins *after* LSTAT. The v3 server refuses the RENAME.
    fs::write(root.join("race-next-rename"), []).unwrap();
    let error = run(&pane, approved, rename("/source", "/racing"))
        .await
        .unwrap_err();
    assert!(!error.contains("unknown"), "{error}");
    assert_eq!(fs::read(root.join("racing")).unwrap(), b"racing entry");
    assert_eq!(fs::read(root.join("source")).unwrap(), b"original");
    run(&pane, approved, rename("/source", "/renamed"))
        .await
        .unwrap();
    assert_eq!(fs::read(root.join("renamed")).unwrap(), b"original");
    assert!(!root.join("source").exists());
    assert_eq!(
        trace(root),
        [["/source", "/racing"], ["/source", "/renamed"]]
    );
    drop(pane);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drawer_rename_names_the_link_itself_and_refuses_a_dangling_destination() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let approved = Some(pane.frames.load().connection_generation);
    fs::write(root.join("rename-trace.jsonl"), []).unwrap();
    fs::create_dir(root.join("target")).unwrap();
    directory_junction(root, "link", "target");
    run(&pane, approved, rename("/link", "/new-link"))
        .await
        .unwrap();
    assert!(
        fs::symlink_metadata(root.join("new-link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(root.join("target").is_dir());
    fs::remove_dir(root.join("target")).unwrap();
    let lstat = run(
        &pane,
        approved,
        Operation::Lstat {
            path: "/new-link".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        lstat["attrs"]["permissions"].as_u64().unwrap() & 0o170000,
        0o120000
    );
    assert!(
        run(
            &pane,
            approved,
            Operation::Stat {
                path: "/new-link".into()
            }
        )
        .await
        .is_err()
    );
    fs::create_dir(root.join("source-dir")).unwrap();
    fs::write(root.join("source-dir/child"), b"unchanged").unwrap();
    assert!(
        run(&pane, approved, rename("/source-dir", "/new-link"))
            .await
            .unwrap_err()
            .contains("already exists")
    );
    run(&pane, approved, rename("/source-dir", "/renamed-dir"))
        .await
        .unwrap();
    assert_eq!(
        fs::read(root.join("renamed-dir/child")).unwrap(),
        b"unchanged"
    );
    assert_eq!(
        trace(root),
        [["/link", "/new-link"], ["/source-dir", "/renamed-dir"]]
    );
    drop(pane);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drawer_move_checks_both_folders_and_preserves_the_entry_name() {
    use w11term::sftp::EntryKind::{Directory, File, Symlink};
    let (pane, _, remote, opens, server) = fixture(false).await;
    let root = remote.path();
    let generation = pane.frames.load().connection_generation;
    let approved = Some(generation);
    let channels = opens.load(Ordering::Relaxed);
    fs::write(root.join("rename-trace.jsonl"), []).unwrap();
    fs::create_dir_all(root.join("from/tree/child")).unwrap();
    fs::create_dir(root.join("to folder")).unwrap();
    fs::write(root.join("from/entry"), b"original").unwrap();
    fs::write(root.join("to folder/entry"), b"collision").unwrap();
    directory_junction(&root.join("from"), "link", "tree");

    let move_file = |directory| move_entry("/from/entry", directory, File);
    for expected in [None, Some(generation + 1)] {
        let error = run(&pane, expected, move_file("/to folder"))
            .await
            .unwrap_err();
        assert!(error.contains("not started"), "{error}");
        assert_eq!(opens.load(Ordering::Relaxed), channels);
    }
    for (operation, expected) in [
        (move_file("/from"), "different destination folder"),
        (move_file("/from/entry"), "not a folder"),
        (move_file("/absent"), "check the destination folder"),
        (move_file("/to folder"), "already exists"),
        (
            move_entry("/from/entry", "/to folder", Directory),
            "entry type changed",
        ),
        (
            move_entry("/from/tree", "/from/tree/child", Directory),
            "descendants",
        ),
    ] {
        let error = run(&pane, approved, operation).await.unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
    assert!(trace(root).is_empty());
    assert_eq!(fs::read(root.join("from/entry")).unwrap(), b"original");
    assert_eq!(
        fs::read(root.join("to folder/entry")).unwrap(),
        b"collision"
    );
    fs::remove_file(root.join("to folder/entry")).unwrap();

    fs::write(root.join("race-next-rename"), []).unwrap();
    let error = run(&pane, approved, move_file("/to folder"))
        .await
        .unwrap_err();
    assert!(!error.contains("unknown"), "{error}");
    assert_eq!(fs::read(root.join("from/entry")).unwrap(), b"original");
    assert_eq!(
        fs::read(root.join("to folder/entry")).unwrap(),
        b"racing entry"
    );
    fs::remove_file(root.join("to folder/entry")).unwrap();
    let moved = run(&pane, approved, move_file("/to folder")).await.unwrap();
    assert_eq!(moved["directory"], "/to folder");
    assert!(!root.join("from/entry").exists());
    assert_eq!(fs::read(root.join("to folder/entry")).unwrap(), b"original");

    run(
        &pane,
        approved,
        move_entry("/from/link", "/to folder", Symlink),
    )
    .await
    .unwrap();
    assert!(
        fs::symlink_metadata(root.join("to folder/link"))
            .unwrap()
            .is_symlink()
    );
    assert!(root.join("from/tree/child").is_dir());
    run(
        &pane,
        approved,
        move_entry("/from/tree", "/to folder", Directory),
    )
    .await
    .unwrap();
    assert!(root.join("to folder/tree/child").is_dir());
    assert!(!root.join("from/tree").exists());
    assert_eq!(
        trace(root),
        [
            ["/from/entry", "/to folder/entry"],
            ["/from/entry", "/to folder/entry"],
            ["/from/link", "/to folder/link"],
            ["/from/tree", "/to folder/tree"],
        ]
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drawer_rename_connection_loss_distinguishes_preflight_from_unacknowledged_mutation() {
    for (moving, dispatched) in [(false, false), (false, true), (true, false), (true, true)] {
        let (Source::Ssh(mut ssh), remote, _, server) = fixture_connections(false, 2).await else {
            unreachable!()
        };
        let root = remote.path();
        ssh.player.reconnect = true;
        fs::write(root.join("rename-trace.jsonl"), []).unwrap();
        fs::write(root.join("source"), b"original").unwrap();
        let (operation, target, verb) = if moving {
            fs::create_dir(root.join("dest")).unwrap();
            (
                move_entry("/source", "/dest", w11term::sftp::EntryKind::File),
                "dest/source",
                "Move",
            )
        } else {
            (rename("/source", "/target"), "target", "Rename")
        };
        fs::write(
            root.join(if dispatched {
                "disconnect-after-rename"
            } else {
                "disconnect-on-lstat"
            }),
            [],
        )
        .unwrap();
        let pane = Session::start(Source::Ssh(ssh));
        let mut operation = Some(operation);
        for generation in [1, 2] {
            tokio::time::timeout(Duration::from_secs(8), async {
                while pane.frames.load().connection_generation != generation {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            if generation == 1 {
                let error = run(&pane, Some(1), operation.take().unwrap())
                    .await
                    .unwrap_err();
                if dispatched {
                    assert!(
                        error.contains(&format!("{verb} outcome unknown")),
                        "{error}"
                    );
                } else {
                    assert!(
                        error.contains(&format!("{verb} was not started"))
                            && !error.contains("unknown"),
                        "{error}"
                    );
                }
            }
        }
        assert_eq!(trace(root).len(), usize::from(dispatched));
        assert_eq!(
            fs::read(root.join(if dispatched { target } else { "source" })).unwrap(),
            b"original"
        );
        assert!(
            !root
                .join(if dispatched { "source" } else { target })
                .exists()
        );
        drop(pane);
        server.abort();
    }
}
