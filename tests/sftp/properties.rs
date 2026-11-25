use super::*;
use w11term::sftp::{Attributes, Operation, PermissionReview};

impl Files {
    pub(super) fn set_permissions(
        &mut self,
        opcode: &str,
        path: String,
        attrs: &FileAttributes,
    ) -> Result<(), StatusCode> {
        if !self.root.join("fixture-permissions").is_file() {
            return Err(StatusCode::OpUnsupported);
        }
        self.trace_properties(opcode, &path)?;
        if attrs.size.is_some()
            || attrs.uid.is_some()
            || attrs.gid.is_some()
            || attrs.atime.is_some()
            || attrs.mtime.is_some()
        {
            return Err(StatusCode::BadMessage);
        }
        let mode = attrs
            .permissions
            .filter(|&mode| mode <= 0o7777)
            .ok_or(StatusCode::BadMessage)?;
        let entry = entry_attrs(&self.path(&path))?;
        if !matches!(
            w11term::sftp::EntryKind::from_permissions(entry.permissions),
            Some(w11term::sftp::EntryKind::File | w11term::sftp::EntryKind::Directory)
        ) {
            return Err(StatusCode::PermissionDenied);
        }
        self.posix
            .as_mut()
            .ok_or(StatusCode::Failure)?
            .set_permissions(path, mode);
        Ok(())
    }

    pub(super) fn trace_properties(&self, opcode: &str, path: &str) -> Result<(), StatusCode> {
        let trace = self.root.join("properties-trace");
        if trace.is_file() {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(trace)
                .map_err(io)?;
            writeln!(file, "{opcode} {path}").map_err(io)?;
        }
        Ok(())
    }

    pub(super) fn properties_readlink(&self, id: u32, path: &str) -> Result<Name, StatusCode> {
        self.trace_properties("READLINK", path)?;
        if self.root.join("deny-readlink").is_file() {
            return Err(StatusCode::PermissionDenied);
        }
        let target = fs::read_link(self.path(path)).map_err(io)?;
        // The marker represents the literal POSIX target carried by SSH_FXP_NAME.
        // A real junction supplies the link's LSTAT type on unprivileged Windows.
        let target = fs::read_to_string(self.root.join("properties-link-target"))
            .unwrap_or_else(|_| target.to_string_lossy().into_owned());
        Ok(Name {
            id,
            files: vec![russh_sftp::protocol::File::dummy(target)],
        })
    }
}

fn properties(path: &str) -> Operation {
    Operation::EntryProperties { path: path.into() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn updating_permissions_rechecks_the_review_and_uses_the_server_mode() {
    for (capability, verb) in [("lsetstat", "LSETSTAT"), ("v3", "SETSTAT")] {
        let (pane, _, remote, _, server) = fixture(false).await;
        let root = remote.path();
        fs::write(root.join("fixture-permissions"), capability).unwrap();
        fs::write(root.join("regular"), b"keep the contents").unwrap();
        fs::create_dir(root.join("folder")).unwrap();
        directory_junction(root, "link", "folder");
        let trace = root.join("properties-trace");
        fs::write(&trace, []).unwrap();
        let generation = pane.frames.load().connection_generation;
        let snapshot = |reply: Value| {
            let attrs: Attributes = serde_json::from_value(reply["attrs"].clone()).unwrap();
            PermissionReview::from_attributes(&attrs)
        };
        let review = snapshot(
            guarded_job(&pane, Some(generation), properties("/regular"))
                .await
                .unwrap(),
        )
        .unwrap();
        let edit = |path: &str, review, mode| Operation::SetEntryPermissions {
            path: path.into(),
            review,
            mode,
        };
        for expected in [None, Some(generation + 1)] {
            assert!(
                guarded_job(&pane, expected, edit("/regular", review, 0o750))
                    .await
                    .is_err()
            );
        }
        assert!(
            guarded_job(&pane, Some(generation), edit("/regular", review, 0o100750))
                .await
                .is_err()
        );
        assert!(
            guarded_job(&pane, Some(generation), edit("/link", review, 0o750))
                .await
                .is_err()
        );
        assert!(
            guarded_job(&pane, Some(generation), edit("/folder", review, 0o750))
                .await
                .is_err()
        );
        let reply = guarded_job(&pane, Some(generation), edit("/regular", review, 0o750))
            .await
            .unwrap();
        assert_eq!(reply["attrs"]["uid"], 1701);
        assert_eq!(reply["attrs"]["gid"], 2048);
        assert_eq!(snapshot(reply).unwrap().mode(), 0o750);
        let error = guarded_job(&pane, Some(generation), edit("/regular", review, 0o777))
            .await
            .unwrap_err();
        assert!(error.contains("changed"), "{error}");
        assert_eq!(
            fs::read(root.join("regular")).unwrap(),
            b"keep the contents"
        );
        let requests = fs::read_to_string(trace).unwrap();
        assert_eq!(
            requests
                .lines()
                .filter(|line| line.contains("SETSTAT"))
                .collect::<Vec<_>>(),
            [format!("{verb} /regular")]
        );
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entry_properties_requires_the_original_ssh_connection_and_only_reads_link_metadata() {
    let (pane, _, remote, opens, server) = fixture(false).await;
    let root = remote.path();
    let generation = pane.frames.load().connection_generation;
    let initial_channels = opens.load(Ordering::Relaxed);
    let trace = root.join("properties-trace");
    let transfer = root.join("transfer-trace");
    fs::write(&trace, []).unwrap();
    fs::write(&transfer, []).unwrap();
    let file = root.join("pilot 雪");
    fs::write(&file, b"hello").unwrap();
    fs::create_dir(root.join("folder")).unwrap();
    directory_junction(root, "linked", "folder");
    fs::write(
        root.join("properties-link-target"),
        "../archived [雪]/unavailable",
    )
    .unwrap();
    for expected in [None, Some(generation + 1)] {
        let error = guarded_job(&pane, expected, properties("/pilot 雪"))
            .await
            .unwrap_err();
        assert!(error.contains("connection"), "{error}");
    }
    assert_eq!(opens.load(Ordering::Relaxed), initial_channels);
    assert!(fs::read(&trace).unwrap().is_empty());
    assert!(
        serde_json::from_value::<Operation>(json!({"op":"entry-properties", "path":"/linked"}))
            .is_err()
    );
    for (path, kind) in [
        ("/pilot 雪", 0o100000),
        ("/folder", 0o040000),
        ("/linked", 0o120000),
    ] {
        let reply = guarded_job(&pane, Some(generation), properties(path))
            .await
            .unwrap();
        let attrs = &reply["attrs"];
        assert_eq!(attrs["permissions"].as_u64().unwrap() & 0o170000, kind);
        if kind == 0o120000 {
            assert_eq!(reply["target"], "../archived [雪]/unavailable");
        } else {
            assert!(reply.get("target").is_none());
        }
        assert!(reply.get("target_error").is_none());
        if path == "/pilot 雪" {
            assert_eq!(attrs["size"], 5);
        }
    }
    fs::write(root.join("properties-omit-attrs"), "pilot 雪").unwrap();
    let reply = guarded_job(&pane, Some(generation), properties("/pilot 雪"))
        .await
        .unwrap();
    assert!(reply["attrs"].is_object());
    for attr in ["size", "permissions", "uid", "gid", "atime", "mtime"] {
        assert!(reply["attrs"][attr].is_null(), "{attr} was supplied");
    }
    assert!(reply.get("target").is_none() && reply.get("target_error").is_none());
    assert_eq!(
        fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "LSTAT /pilot 雪",
            "LSTAT /folder",
            "LSTAT /linked",
            "READLINK /linked",
            "LSTAT /pilot 雪"
        ]
    );
    assert!(fs::read(&transfer).unwrap().is_empty());
    assert_eq!(fs::read(file).unwrap(), b"hello");
    assert_eq!(opens.load(Ordering::Relaxed), initial_channels + 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entry_properties_keeps_link_attributes_when_readlink_is_denied_or_has_no_target() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let generation = Some(pane.frames.load().connection_generation);
    let trace = root.join("properties-trace");
    fs::write(&trace, []).unwrap();
    fs::write(root.join("transfer-trace"), []).unwrap();
    fs::create_dir(root.join("target")).unwrap();
    directory_junction(root, "link", "target");
    let deny = root.join("deny-readlink");
    fs::write(&deny, []).unwrap();
    let denied = guarded_job(&pane, generation, properties("/link"))
        .await
        .unwrap();
    assert!(!denied["target_error"].as_str().unwrap().is_empty());
    assert!(denied.get("target").is_none());
    let permissions = denied["attrs"]["permissions"].as_u64().unwrap();
    assert_eq!(permissions & 0o170000, 0o120000);
    fs::remove_file(deny).unwrap();
    fs::write(root.join("properties-link-target"), []).unwrap();
    let empty = guarded_job(&pane, generation, properties("/link"))
        .await
        .unwrap();
    assert_eq!(empty["attrs"]["permissions"], permissions);
    assert!(!empty["target_error"].as_str().unwrap().is_empty());
    assert!(empty.get("target").is_none());
    assert!(
        guarded_job(&pane, generation, properties("/missing"))
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(trace)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "LSTAT /link",
            "READLINK /link",
            "LSTAT /link",
            "READLINK /link",
            "LSTAT /missing"
        ]
    );
    assert!(fs::read(root.join("transfer-trace")).unwrap().is_empty());
    server.abort();
}
