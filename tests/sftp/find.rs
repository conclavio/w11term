use super::*;
use w11term::sftp::{
    EntryKind, FilenameMatch, FilenameSearch, Job, Operation, SearchDownload, TransferCancel,
};

fn search(path: &str, query: &str) -> Operation {
    Operation::Find {
        path: path.into(),
        query: query.into(),
        regex: false,
        match_case: false,
        match_path: false,
        kind: None,
        exclude_dir_regex: String::new(),
    }
}

fn hit(parent: &str, name: &str, kind: EntryKind) -> FilenameMatch {
    FilenameMatch {
        parent: parent.into(),
        name: name.into(),
        kind: Some(kind),
        mtime: None,
        size: None,
    }
}

#[test]
fn search_download_rejects_unsafe_components_and_collisions_between_folders() {
    let selected = hit("/tree/Logs", "shared.txt", EntryKind::File);
    for bad in [
        hit("/trees/Logs", "other.txt", EntryKind::File),
        hit("/tree/../Logs", "other.txt", EntryKind::File),
        hit("/tree/Logs", "NUL.txt", EntryKind::File),
        hit("/tree/logs", "different.txt", EntryKind::File),
        hit("/tree/Logs", "SHARED.TXT", EntryKind::File),
        hit("/tree/Logs", "other.txt", EntryKind::Symlink),
    ] {
        assert!(
            SearchDownload::new("/tree", [&selected, &bad]).is_err(),
            "{bad:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filename_search_returns_reported_metadata_and_keeps_zero_distinct_from_unknown() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let tree = root.join("tree");
    fs::create_dir_all(tree.join("folder.log")).unwrap();
    fs::write(tree.join("big.log"), b"12345").unwrap();
    fs::write(tree.join("empty.log"), []).unwrap();
    set_fixture_modified(&tree.join("big.log"), 1_690_000_000);
    set_fixture_modified(&tree.join("empty.log"), 1_690_000_001);
    fs::write(root.join("omit-listed-mode"), "big.log").unwrap();

    let found: FilenameSearch = serde_json::from_value(
        guarded_job(&pane, None, search("/tree", ".log"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        found
            .matches
            .iter()
            .map(|hit| (hit.name.as_str(), hit.size))
            .collect::<Vec<_>>(),
        [
            ("big.log", Some(5)),
            ("empty.log", Some(0)),
            ("folder.log", None)
        ]
    );
    assert_eq!(found.matches[0].mtime, Some(1_690_000_000));
    assert_eq!(found.matches[1].mtime, Some(1_690_000_001));

    fs::write(root.join("tree-unknown-size"), []).unwrap();
    let missing = guarded_job(&pane, None, search("/tree", "big.log"))
        .await
        .unwrap();
    assert!(missing["matches"][0].get("size").is_none());
    assert_eq!(missing["matches"][0]["mtime"], 1_690_000_000);
    let legacy: FilenameMatch = serde_json::from_value(json!({
        "parent": "/tree", "name": "old.log", "kind": "file"
    }))
    .unwrap();
    assert_eq!((legacy.mtime, legacy.size), (None, None));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn marked_search_download_preserves_paths_and_publishes_only_complete_folders() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path().join("tree");
    fs::create_dir_all(root.join("primary")).unwrap();
    fs::create_dir_all(root.join("backup")).unwrap();
    fs::write(root.join("primary/report.txt"), b"primary").unwrap();
    fs::write(root.join("primary/ignore.txt"), b"unmarked").unwrap();
    fs::write(root.join("backup/report.txt"), b"backup").unwrap();
    fs::write(root.join("backup/keep.txt"), b"inside the marked folder").unwrap();
    let local = tempfile::tempdir().unwrap();
    let destination = local.path().join("Reports");
    let selection = [
        hit("/tree/backup", "report.txt", EntryKind::File),
        hit("/tree", "backup", EntryKind::Directory),
        hit("/tree/primary", "report.txt", EntryKind::File),
    ];
    let op = |local: PathBuf, hits: &[FilenameMatch]| Operation::GetSearch {
        selection: SearchDownload::new("/tree", hits).unwrap(),
        local,
        preserve_mtime: false,
    };
    let generation = Some(pane.frames.load().connection_generation);
    let reply = guarded_job(&pane, generation, op(destination.clone(), &selection))
        .await
        .unwrap();
    assert_eq!(reply["files"], 3);
    assert_eq!(
        fs::read(destination.join("primary/report.txt")).unwrap(),
        b"primary"
    );
    assert_eq!(
        fs::read(destination.join("backup/report.txt")).unwrap(),
        b"backup"
    );
    assert_eq!(
        fs::read(destination.join("backup/keep.txt")).unwrap(),
        b"inside the marked folder"
    );
    assert!(!destination.join("primary/ignore.txt").exists());
    let rejected = guarded_job(&pane, generation, op(destination.clone(), &selection))
        .await
        .unwrap_err();
    assert!(
        rejected.contains("local destination already exists"),
        "{rejected}"
    );

    let interrupted = local.path().join("Missing");
    let mut missing = selection.to_vec();
    missing.push(hit("/tree/primary", "zz-vanished.txt", EntryKind::File));
    let rejected = guarded_job(&pane, generation, op(interrupted.clone(), &missing))
        .await
        .unwrap_err();
    assert!(rejected.contains("No such file"), "{rejected}");
    assert!(!interrupted.exists());
    assert_eq!(fs::read_dir(local.path()).unwrap().count(), 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filename_search_uses_regex_and_case_options_and_rejects_bad_patterns_before_scanning() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let folder = remote.path().join("logs");
    fs::create_dir(&folder).unwrap();
    let trace = remote.path().join("listing-trace");
    fs::write(&trace, []).unwrap();
    for name in [
        "Report-01.log",
        "report-02.log",
        "Report-old.log",
        "Report-03.txt",
    ] {
        fs::write(folder.join(name), []).unwrap();
    }
    let options = |query: &str, regex, match_case| Operation::Find {
        path: "/logs".into(),
        query: query.into(),
        regex,
        match_case,
        match_path: false,
        kind: None,
        exclude_dir_regex: String::new(),
    };
    let error = guarded_job(&pane, None, options("(", true, false))
        .await
        .unwrap_err();
    assert!(error.contains("Invalid filename regex"), "{error}");
    assert!(fs::read(&trace).unwrap().is_empty());
    for (query, regex, match_case, names) in [
        (
            r"^Report-[0-9]+\.log$",
            true,
            false,
            &["Report-01.log", "report-02.log"][..],
        ),
        (r"^Report-[0-9]+\.log$", true, true, &["Report-01.log"][..]),
        ("report-", false, true, &["report-02.log"][..]),
    ] {
        let result: FilenameSearch = serde_json::from_value(
            guarded_job(&pane, None, options(query, regex, match_case))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            result
                .matches
                .iter()
                .map(|hit| hit.name.as_str())
                .collect::<Vec<_>>(),
            names
        );
    }
    let legacy = serde_json::json!({"op":"find","path":"/logs","query":"report-"});
    let parsed: Operation = serde_json::from_value(legacy.clone()).unwrap();
    assert!(matches!(
        parsed,
        Operation::Find {
            regex: false,
            match_case: false,
            match_path: false,
            kind: None,
            ..
        }
    ));
    assert_eq!(serde_json::to_value(parsed).unwrap(), legacy);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filename_type_limits_results_without_skipping_subdirectories() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let top = remote.path().join("tree");
    fs::create_dir_all(top.join("branch/nested")).unwrap();
    for n in 0..205 {
        fs::write(top.join(format!("{n:03}.log")), []).unwrap();
    }
    fs::write(top.join("branch/nested/report.log"), []).unwrap();
    directory_junction(&top, "linked", top.join("branch").to_str().unwrap());
    for (query, regex, kind, expected) in [
        ("", false, EntryKind::Directory, &["branch", "nested"][..]),
        ("^report\\.log$", true, EntryKind::File, &["report.log"][..]),
        ("", false, EntryKind::Symlink, &["linked"][..]),
    ] {
        let result: FilenameSearch = serde_json::from_value(
            guarded_job(
                &pane,
                None,
                Operation::Find {
                    path: "/tree".into(),
                    query: query.into(),
                    regex,
                    match_case: false,
                    match_path: false,
                    kind: Some(kind),
                    exclude_dir_regex: String::new(),
                },
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert!(!result.truncated);
        assert_eq!(result.scanned_directories, 3);
        assert!(result.scanned_entries > 200);
        assert!(result.matches.iter().all(|hit| hit.kind == Some(kind)));
        assert_eq!(
            result
                .matches
                .iter()
                .map(|hit| hit.name.as_str())
                .collect::<Vec<_>>(),
            expected
        );
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relative_path_matching_uses_the_canonical_search_root_and_full_components() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    for folder in [
        "tree/Raw Logs/雪",
        "tree/other/雪",
        "tree/previous Raw Logs/雪",
    ] {
        fs::create_dir_all(root.join(folder)).unwrap();
        fs::write(root.join(folder).join("report.txt"), b"fixture").unwrap();
    }
    fs::write(root.join("tree/root.txt"), []).unwrap();
    fs::create_dir_all(root.join("canonical-parent/sub")).unwrap();
    fs::write(root.join("canonical-parent/sub/report.txt"), []).unwrap();
    for (root, query, regex, match_case, match_path, expected) in [
        (
            "/tree",
            "^Raw Logs/雪/report\\.txt$",
            true,
            true,
            true,
            &["/tree/Raw Logs/雪"][..],
        ),
        (
            "/tree",
            "raw logs/雪/report",
            false,
            false,
            true,
            &["/tree/Raw Logs/雪", "/tree/previous Raw Logs/雪"][..],
        ),
        ("/tree", "raw logs/雪/report", false, true, true, &[][..]),
        ("/tree", "^/Raw Logs/", true, true, true, &[][..]),
        ("/tree", "Raw Logs/雪/report", false, true, false, &[][..]),
        (
            "/tree",
            "^report\\.txt$",
            true,
            true,
            false,
            &[
                "/tree/Raw Logs/雪",
                "/tree/other/雪",
                "/tree/previous Raw Logs/雪",
            ][..],
        ),
        ("/tree", "^root\\.txt$", true, true, true, &["/tree"][..]),
        (
            "alias",
            r"^sub/report\.txt$",
            true,
            true,
            true,
            &["/canonical-parent/sub"][..],
        ),
        (
            "/",
            "^tree/Raw Logs/雪/report\\.txt$",
            true,
            true,
            true,
            &["/tree/Raw Logs/雪"][..],
        ),
    ] {
        let found: FilenameSearch = serde_json::from_value(
            guarded_job(
                &pane,
                None,
                Operation::Find {
                    path: root.into(),
                    query: query.into(),
                    regex,
                    match_case,
                    match_path,
                    kind: Some(EntryKind::File),
                    exclude_dir_regex: String::new(),
                },
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            found
                .matches
                .iter()
                .map(|hit| hit.parent.as_str())
                .collect::<Vec<_>>(),
            expected,
            "{root} {query}"
        );
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn excluded_folders_are_neither_results_nor_opened_and_exclusions_respect_case() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    for path in ["tree/.GIT", "tree/src/node_modules", "tree/vendor"] {
        fs::create_dir_all(root.join(path)).unwrap();
    }
    for path in [
        "tree/.GIT/ignored.txt",
        "tree/src/node_modules/ignored.txt",
        "tree/src/keep.txt",
        "tree/vendor/node_modules",
    ] {
        fs::write(root.join(path), []).unwrap();
    }
    let trace = root.join("listing-trace");
    fs::write(&trace, []).unwrap();
    let query = |pattern: &str, match_case| Operation::Find {
        path: "/tree".into(),
        query: String::new(),
        regex: false,
        match_case,
        match_path: false,
        kind: None,
        exclude_dir_regex: pattern.into(),
    };
    let invalid = guarded_job(&pane, None, query("[", false))
        .await
        .unwrap_err();
    assert!(
        invalid.contains("Invalid folder exclusion regex"),
        "{invalid}"
    );
    assert!(fs::read(&trace).unwrap().is_empty());

    let found: FilenameSearch = serde_json::from_value(
        guarded_job(&pane, None, query(r"(^|/)(\.git|node_modules)$", false))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        found
            .matches
            .iter()
            .map(|hit| (hit.parent.as_str(), hit.name.as_str()))
            .collect::<Vec<_>>(),
        [
            ("/tree", "src"),
            ("/tree", "vendor"),
            ("/tree/src", "keep.txt"),
            ("/tree/vendor", "node_modules")
        ]
    );
    assert_eq!(found.excluded_directories, 2);
    assert_eq!(found.scanned_directories, 3);
    assert_eq!(found.skipped_directories, 0);
    assert!(!found.truncated);
    #[cfg(feature = "ui-smoke")]
    {
        let wire = fs::read_to_string(&trace).unwrap();
        let opened: std::collections::BTreeSet<_> = wire
            .lines()
            .filter_map(|request| request.strip_prefix("OPENDIR "))
            .collect();
        assert_eq!(
            opened,
            std::collections::BTreeSet::from(["/tree", "/tree/src", "/tree/vendor"])
        );
    }
    let sensitive: FilenameSearch = serde_json::from_value(
        guarded_job(&pane, None, query(r"(^|/)(\.git|node_modules)$", true))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(sensitive.excluded_directories, 1);
    assert!(
        sensitive
            .matches
            .iter()
            .any(|hit| hit.parent == "/tree/.GIT" && hit.name == "ignored.txt")
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn excluded_folders_leave_directory_budget_for_the_rest_of_the_search() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let tree = remote.path().join("tree");
    fs::create_dir_all(tree.join("keep")).unwrap();
    fs::write(tree.join("keep/report.txt"), []).unwrap();
    for index in 0..130 {
        fs::create_dir(tree.join(format!("cache-{index:03}"))).unwrap();
    }
    let found: FilenameSearch = serde_json::from_value(
        guarded_job(
            &pane,
            None,
            Operation::Find {
                path: "/tree".into(),
                query: r"^keep/report\.txt$".into(),
                regex: true,
                match_case: false,
                match_path: true,
                kind: Some(EntryKind::File),
                exclude_dir_regex: r"^cache-[0-9]+$".into(),
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(found.excluded_directories, 130);
    assert_eq!(found.scanned_directories, 2);
    assert!(!found.truncated);
    assert_eq!(found.matches.len(), 1);
    assert_eq!(found.matches[0].parent, "/tree/keep");
    assert_eq!(found.matches[0].name, "report.txt");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recursive_filename_search_finds_literal_unicode_and_does_not_traverse_links() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let parent = root.join("tree");
    let outside = root.join("outside");
    fs::create_dir_all(parent.join("nested")).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(parent.join("nested/[Ångström].txt"), []).unwrap();
    fs::write(parent.join("nested/Ångström.txt"), []).unwrap();
    fs::write(outside.join("[ångström]-outside.txt"), []).unwrap();
    directory_junction(&parent, "linked", outside.to_str().unwrap());
    fs::write(root.join("omit-listed-mode"), "nested").unwrap();
    let result: FilenameSearch = serde_json::from_value(
        guarded_job(&pane, None, search("/tree", "[ångSTRÖM]"))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result.root, "/tree");
    assert_eq!(result.scanned_directories, 2);
    assert!(!result.truncated);
    assert_eq!(result.matches.len(), 1);
    assert_eq!(result.matches[0].parent, "/tree/nested");
    assert_eq!(result.matches[0].name, "[Ångström].txt");
    assert_eq!(result.matches[0].kind, Some(EntryKind::File));
    for (name, kind) in [
        ("nested", EntryKind::Directory),
        ("linked", EntryKind::Symlink),
    ] {
        let result: FilenameSearch = serde_json::from_value(
            guarded_job(&pane, None, search("/tree", name))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].kind, Some(kind));
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recursive_filename_search_caps_matches_and_closes_its_handle() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let folder = remote.path().join("tree");
    fs::create_dir(&folder).unwrap();
    fs::write(remote.path().join("listing-trace"), []).unwrap();
    fs::write(remote.path().join("trace-directory-close"), []).unwrap();
    for n in 0..225 {
        fs::write(folder.join(format!("entry-{n}.log")), []).unwrap();
    }
    let found = guarded_job(&pane, None, search("/tree", ".log"))
        .await
        .unwrap();
    assert_eq!(found["matches"].as_array().unwrap().len(), 200);
    assert_eq!(found["truncated"], true);
    let trace = fs::read_to_string(remote.path().join("listing-trace")).unwrap();
    assert_eq!(trace.lines().last(), Some("CLOSE DIR"));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recursive_filename_search_cancels_an_in_flight_directory_open() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    fs::create_dir_all(root.join("tree/child")).unwrap();
    fs::write(root.join("tree/child/inside.log"), []).unwrap();
    fs::write(root.join("listing-trace"), []).unwrap();
    let hold = root.join("hold-opendir");
    fs::write(&hold, "/tree/child").unwrap();
    let cancel = TransferCancel::default();
    let (reply, response) = tokio::sync::oneshot::channel();
    pane.input
        .send(Command::Sftp(Job {
            expected_generation: Some(pane.frames.load().connection_generation),
            operation: search("/tree", ".log"),
            deadline: tokio::time::Instant::now() + Duration::from_secs(10),
            reply,
            progress: None,
            cancel: Some(cancel.clone()),
        }))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.join("opendir-paused").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the child directory open");
    cancel.cancel();
    fs::remove_file(hold).unwrap();
    let result = response.await.unwrap().unwrap();
    assert_eq!(result["cancelled"], true);
    assert!(result["matches"].as_array().unwrap().is_empty());
    assert_eq!(
        guarded_job(
            &pane,
            None,
            Operation::Ls {
                path: "/tree".into()
            }
        )
        .await
        .unwrap()["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    server.abort();
}
