use super::guarded_job as run;
use super::*;
use w11term::sftp::{CopyDestination, Operation};

fn copy() -> Operation {
    Operation::CopyEntry {
        from: "/original.bin".into(),
        destination: CopyDestination::Folder {
            directory: "/archive".into(),
            is_canonical: false,
        },
    }
}

fn archive_names(root: &std::path::Path) -> Vec<String> {
    fs::read_dir(root.join("archive"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_accepts_a_literal_sibling_name_and_keeps_existing_files() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let generation = Some(pane.frames.load().connection_generation);
    fs::write(root.join("original.bin"), b"source\0bytes").unwrap();
    fs::write(root.join("existing.bin"), b"keep").unwrap();
    fs::write(root.join("rename-trace.jsonl"), []).unwrap();

    let duplicate = |name: &str| Operation::CopyEntry {
        from: "/original.bin".into(),
        destination: CopyDestination::Sibling { name: name.into() },
    };
    for name in [
        "",
        " ",
        ".",
        "..",
        "/outside",
        "../outside",
        "sub\\file.bin",
        "line\nfeed",
        "nul\0byte",
        "original.bin",
        "existing.bin",
    ] {
        let error = run(&pane, generation, duplicate(name)).await.unwrap_err();
        assert!(error.contains("Copy was not started"), "{name:?}: {error}");
    }
    assert!(mutation_trace(root, "rename-trace.jsonl").is_empty());
    let new_name = "a $literal 雪.bin";
    assert_eq!(
        run(&pane, generation, duplicate(new_name)).await.unwrap(),
        json!({"directory":"/","bytes_copied":12})
    );
    let renames = mutation_trace(root, "rename-trace.jsonl");
    assert_eq!(renames.len(), 1);
    assert!(renames[0][0].starts_with("/.w11term-copy-"));
    assert_eq!(renames[0][1], format!("/{new_name}"));
    assert_eq!(fs::read(root.join(new_name)).unwrap(), b"source\0bytes");
    assert_eq!(
        fs::read(root.join("original.bin")).unwrap(),
        b"source\0bytes"
    );
    assert_eq!(fs::read(root.join("existing.bin")).unwrap(), b"keep");
    assert!(!fs::read_dir(root).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".w11term-copy-")
    }));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_uses_the_extension_or_relays_bounded_packets() {
    for (advertised, rejects) in [(false, false), (true, false), (true, true)] {
        let (pane, _, remote, opens, server) = fixture(false).await;
        let root = remote.path();
        let bytes: Vec<u8> = (0..70_000).map(|i| (i % 251) as u8).collect();
        fs::write(root.join("original.bin"), &bytes).unwrap();
        fs::create_dir(root.join("archive")).unwrap();
        fs::write(root.join("small-reads"), []).unwrap();
        fs::write(root.join("transfer-trace"), []).unwrap();
        if advertised {
            fs::write(root.join("fixture-copy-data"), []).unwrap();
        }
        if rejects {
            fs::write(root.join("reject-copy-data"), []).unwrap();
        }
        let generation = pane.frames.load().connection_generation;
        let channels = opens.load(Ordering::Relaxed);
        for expected in [None, Some(generation + 1)] {
            assert!(
                run(&pane, expected, copy())
                    .await
                    .unwrap_err()
                    .contains("not started")
            );
            assert_eq!(opens.load(Ordering::Relaxed), channels);
        }
        let result = run(&pane, Some(generation), copy()).await.unwrap();
        assert_eq!(
            result,
            json!({"directory":"/archive","bytes_copied":bytes.len()})
        );
        assert_eq!(archive_names(root), ["original.bin"]);
        assert_eq!(fs::read(root.join("archive/original.bin")).unwrap(), bytes);
        assert_eq!(fs::read(root.join("original.bin")).unwrap(), bytes);
        let trace = fs::read_to_string(root.join("transfer-trace")).unwrap();
        assert_eq!(trace.contains("COPY-DATA"), advertised, "{trace}");
        assert_eq!(
            trace.contains("READ 512"),
            !advertised || rejects,
            "{trace}"
        );
        assert_eq!(
            trace.contains("WRITE 512"),
            !advertised || rejects,
            "{trace}"
        );
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_keeps_existing_names_and_removes_failed_staging_files() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let generation = Some(pane.frames.load().connection_generation);
    let bytes = vec![123; 70_000];
    fs::write(root.join("original.bin"), &bytes).unwrap();
    fs::create_dir(root.join("archive")).unwrap();
    fs::write(root.join("archive/original.bin"), b"existing").unwrap();
    let error = run(&pane, generation, copy()).await.unwrap_err();
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(
        fs::read(root.join("archive/original.bin")).unwrap(),
        b"existing"
    );
    fs::remove_file(root.join("archive/original.bin")).unwrap();

    fs::write(root.join("tree-early-eof"), []).unwrap();
    let error = run(&pane, generation, copy()).await.unwrap_err();
    assert!(error.contains("source shrank"), "{error}");
    assert!(archive_names(root).is_empty());
    fs::remove_file(root.join("tree-early-eof")).unwrap();

    fs::write(root.join("race-next-rename"), []).unwrap();
    assert!(run(&pane, generation, copy()).await.is_err());
    assert_eq!(archive_names(root), ["original.bin"]);
    assert_eq!(
        fs::read(root.join("archive/original.bin")).unwrap(),
        b"racing entry"
    );
    assert_eq!(fs::read(root.join("original.bin")).unwrap(), bytes);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn successful_copy_data_status_does_not_publish_a_short_copy() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    fs::write(root.join("original.bin"), b"keep every byte").unwrap();
    fs::create_dir(root.join("archive")).unwrap();
    fs::write(root.join("fixture-copy-data"), []).unwrap();
    fs::write(root.join("short-copy-data"), []).unwrap();
    let generation = Some(pane.frames.load().connection_generation);
    let error = run(&pane, generation, copy()).await.unwrap_err();
    assert!(error.contains("did not stage a complete copy"), "{error}");
    assert!(archive_names(root).is_empty());
    assert_eq!(
        fs::read(root.join("original.bin")).unwrap(),
        b"keep every byte"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copy_reports_lost_publication_acknowledgement_without_replay() {
    let (Source::Ssh(mut ssh), remote, _, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    let root = remote.path();
    ssh.player.reconnect = true;
    fs::write(root.join("original.bin"), b"keep every byte").unwrap();
    fs::create_dir(root.join("archive")).unwrap();
    fs::write(root.join("rename-trace.jsonl"), []).unwrap();
    fs::write(root.join("disconnect-after-rename"), []).unwrap();
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
            let error = run(&pane, Some(generation), copy()).await.unwrap_err();
            assert!(error.contains("publication outcome unknown"), "{error}");
        }
    }
    let renames = mutation_trace(root, "rename-trace.jsonl");
    assert_eq!(renames.len(), 1);
    assert!(renames[0][0].starts_with("/archive/.w11term-copy-"));
    assert_eq!(renames[0][1], "/archive/original.bin");
    assert_eq!(
        fs::read(root.join("archive/original.bin")).unwrap(),
        b"keep every byte"
    );
    assert_eq!(
        fs::read(root.join("original.bin")).unwrap(),
        b"keep every byte"
    );
    server.abort();
}
