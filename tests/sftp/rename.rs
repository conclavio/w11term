use super::*;
use w11term::sftp::{Job, Operation};

async fn run(
    pane: &Session,
    generation: Option<u64>,
    operation: Operation,
) -> Result<Value, String> {
    let (reply, response) = tokio::sync::oneshot::channel();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    pane.input
        .send(Command::Sftp(Job {
            expected_generation: generation,
            operation,
            deadline,
            reply,
            progress: None,
        }))
        .unwrap();
    tokio::time::timeout_at(deadline + Duration::from_secs(1), response)
        .await
        .unwrap()
        .unwrap()
}

fn rename(from: &str, to: &str) -> Operation {
    Operation::RenameEntry {
        from: from.into(),
        to: to.into(),
    }
}

fn trace(root: &std::path::Path) -> Vec<[String; 2]> {
    fs::read_to_string(root.join("rename-trace.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
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
    // A directory junction is an actual name-surrogate reparse point and
    // requires neither Developer Mode nor the symbolic-link privilege.
    let made = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/j", "link", "target"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
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
async fn drawer_rename_connection_loss_distinguishes_preflight_from_unacknowledged_mutation() {
    for dispatched in [false, true] {
        let (Source::Ssh(mut ssh), remote, _, server) = fixture_connections(false, 2).await else {
            unreachable!()
        };
        let root = remote.path();
        ssh.player.reconnect = true;
        fs::write(root.join("rename-trace.jsonl"), []).unwrap();
        fs::write(root.join("source"), b"original").unwrap();
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
        for generation in [1, 2] {
            tokio::time::timeout(Duration::from_secs(8), async {
                while pane.frames.load().connection_generation != generation {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            if generation == 1 {
                let error = run(&pane, Some(1), rename("/source", "/target"))
                    .await
                    .unwrap_err();
                if dispatched {
                    assert!(error.contains("Rename outcome unknown"), "{error}");
                } else {
                    assert!(
                        error.contains("Rename was not started") && !error.contains("unknown"),
                        "{error}"
                    );
                }
            }
        }
        assert_eq!(trace(root).len(), usize::from(dispatched));
        assert_eq!(
            fs::read(root.join(if dispatched { "target" } else { "source" })).unwrap(),
            b"original"
        );
        assert!(
            !root
                .join(if dispatched { "source" } else { "target" })
                .exists()
        );
        drop(pane);
        server.abort();
    }
}
