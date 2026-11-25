use super::*;
use w11term::sftp::{FilenameSearch, Job, Operation, TransferCancel};

fn search(path: &str, query: &str) -> Operation {
    Operation::Find {
        path: path.into(),
        query: query.into(),
    }
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
