use super::*;
use w11term::sftp::{EntryKind, Operation};

fn remove(path: &str, kind: EntryKind) -> Operation {
    Operation::RemoveEntry {
        path: path.into(),
        kind,
    }
}

pub(super) fn trace(root: &std::path::Path) -> Vec<[String; 2]> {
    mutation_trace(root, "delete-trace.jsonl")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drawer_delete_checks_connection_and_lstat_kind_before_any_mutation() {
    assert!(
        serde_json::from_value::<Operation>(json!({
            "op": "remove-entry", "path": "/keep", "kind": "file"
        }))
        .is_err()
    );
    let (pane, _, remote, channels, server) = fixture(false).await;
    let root = remote.path();
    let generation = pane.frames.load().connection_generation;
    fs::write(root.join("delete-trace.jsonl"), []).unwrap();
    fs::write(root.join("keep"), b"original").unwrap();
    let initial_channels = channels.load(Ordering::Relaxed);
    for stale in [None, Some(generation + 1)] {
        let error = guarded_job(&pane, stale, remove("/keep", EntryKind::File))
            .await
            .unwrap_err();
        assert!(error.contains("not started"), "{error}");
        assert_eq!(channels.load(Ordering::Relaxed), initial_channels);
    }
    let approved = Some(generation);
    fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join("target/child"), b"keep child").unwrap();
    directory_junction(root, "linked", "target");
    let changed = guarded_job(&pane, approved, remove("/linked", EntryKind::Directory))
        .await
        .unwrap_err();
    assert!(
        changed.contains("type changed") && changed.contains("not started"),
        "{changed}"
    );
    for path in ["/linked/", "/"] {
        let rejected = guarded_job(&pane, approved, remove(path, EntryKind::Directory))
            .await
            .unwrap_err();
        assert!(rejected.contains("not started"), "{rejected}");
    }
    fs::write(root.join("omit-lstat-permissions"), []).unwrap();
    let unknown = guarded_job(&pane, approved, remove("/keep", EntryKind::File))
        .await
        .unwrap_err();
    assert!(
        unknown.contains("unsupported type") && unknown.contains("not started"),
        "{unknown}"
    );
    fs::remove_file(root.join("omit-lstat-permissions")).unwrap();
    assert!(trace(root).is_empty());
    assert_eq!(fs::read(root.join("keep")).unwrap(), b"original");

    let refused = guarded_job(&pane, approved, remove("/target", EntryKind::Directory))
        .await
        .unwrap_err();
    assert!(!refused.contains("unknown"), "{refused}");
    guarded_job(&pane, approved, remove("/linked", EntryKind::Symlink))
        .await
        .unwrap();
    assert!(fs::symlink_metadata(root.join("linked")).is_err());
    assert_eq!(fs::read(root.join("target/child")).unwrap(), b"keep child");
    assert_eq!(trace(root), [["RMDIR", "/target"], ["REMOVE", "/linked"]]);
    drop(pane);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drawer_delete_reports_the_dispatch_boundary_and_never_replays_after_disconnect() {
    for dispatched in [false, true] {
        let (Source::Ssh(mut ssh), remote, _, server) = fixture_connections(false, 2).await else {
            unreachable!()
        };
        let root = remote.path();
        fs::write(root.join("delete-trace.jsonl"), []).unwrap();
        fs::write(root.join("item"), b"old").unwrap();
        let fault = root.join(if dispatched {
            "disconnect-after-delete"
        } else {
            "disconnect-on-lstat"
        });
        fs::write(&fault, []).unwrap();
        ssh.player.reconnect = true;
        let pane = Session::start(Source::Ssh(ssh));
        for generation in [1, 2] {
            tokio::time::timeout(Duration::from_secs(8), async {
                while pane.frames.load().connection_generation != generation {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            if generation == 1 {
                let error = guarded_job(&pane, Some(1), remove("/item", EntryKind::File))
                    .await
                    .unwrap_err();
                if dispatched {
                    assert!(error.contains("Delete outcome unknown"), "{error}");
                    assert!(!root.join("item").exists());
                } else {
                    assert!(
                        error.contains("Delete was not started") && !error.contains("unknown"),
                        "{error}"
                    );
                    assert_eq!(fs::read(root.join("item")).unwrap(), b"old");
                }
                fs::remove_file(&fault).unwrap();
                fs::write(root.join("item"), b"replacement").unwrap();
            }
        }
        // A new operation fences any request the reconnect could have queued.
        guarded_job(
            &pane,
            Some(2),
            Operation::Lstat {
                path: "/item".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(fs::read(root.join("item")).unwrap(), b"replacement");
        assert_eq!(trace(root).len(), usize::from(dispatched));
        drop(pane);
        server.abort();
    }
}
