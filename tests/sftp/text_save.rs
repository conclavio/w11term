use super::*;
use w11term::sftp::Operation;

// NTFS backs the bytes; the loopback SFTP server supplies Unix owners and modes.
pub(super) type PosixEntries = Arc<std::sync::Mutex<HashMap<String, (u32, u32, u32)>>>;

pub(super) struct PosixMetadata {
    attrs: PosixEntries,
    handles: HashMap<String, String>,
}

impl PosixMetadata {
    pub(super) fn new(attrs: PosixEntries) -> Self {
        Self {
            attrs,
            handles: HashMap::new(),
        }
    }

    pub(super) fn opened(
        &mut self,
        handle: &str,
        path: String,
        flags: OpenFlags,
        attrs: FileAttributes,
    ) {
        if flags.contains(OpenFlags::CREATE) {
            self.attrs.lock().unwrap().insert(
                path.clone(),
                (1000, 1000, attrs.permissions.unwrap_or(0o666)),
            );
        }
        self.handles.insert(handle.into(), path);
    }

    pub(super) fn closed(&mut self, handle: &str) {
        self.handles.remove(handle);
    }

    pub(super) fn stat(&self, path: &str, attrs: &mut FileAttributes) {
        if let Some(mode) = attrs.permissions {
            let (uid, gid, perms) = self
                .attrs
                .lock()
                .unwrap()
                .get(path)
                .copied()
                .unwrap_or((1701, 2048, 0o640));
            attrs.permissions = Some((mode & 0o170000) | (perms & 0o7777));
            attrs.uid = Some(uid);
            attrs.gid = Some(gid);
        }
    }

    pub(super) fn set_permissions(&mut self, path: String, mode: u32) {
        self.attrs
            .lock()
            .unwrap()
            .entry(path)
            .or_insert((1701, 2048, 0o640))
            .2 = mode;
    }

    pub(super) fn renamed(&mut self, from: &str, to: &str) {
        let mut moved = Vec::new();
        let mut attrs = self.attrs.lock().unwrap();
        attrs.retain(|path, attr| {
            if let Some(rest) = path.strip_prefix(from)
                && (rest.is_empty() || rest.starts_with('/'))
            {
                moved.push((format!("{to}{rest}"), *attr));
                false
            } else {
                true
            }
        });
        attrs.extend(moved);
    }

    pub(super) fn fstat(&self, handle: &str, attrs: &mut FileAttributes) -> Result<(), StatusCode> {
        self.stat(self.handles.get(handle).ok_or(StatusCode::Failure)?, attrs);
        Ok(())
    }

    pub(super) fn fsetstat(
        &mut self,
        handle: &str,
        attrs: &FileAttributes,
    ) -> Result<(), StatusCode> {
        let path = self.handles.get(handle).ok_or(StatusCode::Failure)?;
        let mut metadata = self.attrs.lock().unwrap();
        let (uid, gid, mode) = metadata.get_mut(path).ok_or(StatusCode::Failure)?;
        if let (Some(user), Some(group)) = (attrs.uid, attrs.gid) {
            *uid = user;
            *gid = group;
            *mode &= !0o6000;
        }
        if let Some(perms) = attrs.permissions {
            *mode = perms;
        }
        Ok(())
    }
}

impl Files {
    pub(super) fn posix_rename(&mut self, id: u32, payload: Vec<u8>) -> Result<Packet, StatusCode> {
        let mut bytes = payload.into();
        let (from, to): (String, String) =
            russh_sftp::de::from_bytes(&mut bytes).map_err(|_| StatusCode::BadMessage)?;
        if !bytes.is_empty() {
            return Err(StatusCode::BadMessage);
        }
        self.trace_transfer(format_args!("POSIX-RENAME {from} -> {to}"))?;
        if self.root.join("deny-editor-rename").is_file() {
            return Err(StatusCode::PermissionDenied);
        }
        fs::rename(self.path(&from), self.path(&to)).map_err(io)?;
        let posix = self.posix.as_mut().ok_or(StatusCode::OpUnsupported)?;
        posix.renamed(&from, &to);
        Ok(Packet::Status(status(id)))
    }
}

fn save(path: &str, original: Option<&str>, text: &str) -> Operation {
    Operation::SaveText {
        path: path.into(),
        original: original.map(String::from),
        text: text.into(),
    }
}

fn names(folder: &std::path::Path) -> Vec<String> {
    let mut entries: Vec<_> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    entries.sort();
    entries
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn save_replaces_the_file_through_the_extension_and_preserves_mode_and_ownership() {
    let (pane, _, remote, channels, server) = fixture(false).await;
    fs::write(remote.path().join("posix-rename-version"), "1").unwrap();
    fs::write(remote.path().join("transfer-trace"), []).unwrap();
    let folder = remote.path().join("work");
    fs::create_dir(&folder).unwrap();
    let file = folder.join("résumé 雪.txt");
    let path = "/work/résumé 雪.txt";
    let original = "original\r\n";
    let draft = "replacement 雪\r\n\n";
    fs::write(&file, original).unwrap();
    let generation = pane.frames.load().connection_generation;
    for expected in [None, Some(generation + 1)] {
        assert!(
            guarded_job(&pane, expected, save(path, Some(original), draft))
                .await
                .is_err()
        );
    }
    assert_eq!(channels.load(Ordering::Relaxed), 1);
    let approved = Some(generation);
    for marker in ["deny-editor-fsetstat", "deny-editor-rename"] {
        fs::write(remote.path().join(marker), []).unwrap();
        assert!(
            guarded_job(&pane, approved, save(path, Some(original), draft))
                .await
                .is_err()
        );
        assert_eq!(fs::read(&file).unwrap(), original.as_bytes());
        assert_eq!(names(&folder), ["résumé 雪.txt"]);
        fs::remove_file(remote.path().join(marker)).unwrap();
    }
    guarded_job(&pane, approved, save(path, Some(original), draft))
        .await
        .unwrap();
    assert_eq!(fs::read(file).unwrap(), draft.as_bytes());
    assert_eq!(names(&folder), ["résumé 雪.txt"]);
    let stat = guarded_job(&pane, approved, Operation::Lstat { path: path.into() })
        .await
        .unwrap();
    assert_eq!(stat["attrs"]["permissions"], 0o100640u32);
    assert_eq!(stat["attrs"]["uid"], 1701);
    assert_eq!(stat["attrs"]["gid"], 2048);
    let trace = fs::read_to_string(remote.path().join("transfer-trace")).unwrap();
    assert_eq!(
        trace
            .lines()
            .filter(|l| l.starts_with("POSIX-RENAME ") && l.ends_with(path))
            .count(),
        2
    );
    assert_eq!(channels.load(Ordering::Relaxed), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn save_rechecks_same_length_contents_after_uploading_the_draft() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    fs::write(root.join("posix-rename-version"), "1").unwrap();
    let wire = root.join("transfer-trace");
    fs::write(&wire, []).unwrap();
    let gate = root.join("hold-transfer-write");
    fs::write(&gate, []).unwrap();
    let folder = root.join("work");
    fs::create_dir(&folder).unwrap();
    let file = folder.join("config");
    fs::write(&file, "first").unwrap();
    let saved = guarded_job(
        &pane,
        Some(pane.frames.load().connection_generation),
        save(
            "/work/config",
            Some("first"),
            &"x".repeat(3 * w11term::sftp::CHUNK),
        ),
    );
    let concurrent_writer = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fs::read_to_string(&wire)
                .unwrap_or_default()
                .lines()
                .any(|l| l.starts_with("WRITE ") && l != "WRITE 0")
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("second packet of staging upload");
        assert_eq!(fs::read(&file).unwrap(), b"first");
        fs::write(&file, "other").unwrap();
        fs::remove_file(gate).unwrap();
    };
    let (result, ()) = tokio::join!(saved, concurrent_writer);
    let error = result.unwrap_err();
    assert!(error.contains("changed"), "{error}");
    assert_eq!(fs::read(file).unwrap(), b"other");
    assert_eq!(names(&folder), ["config"]);
    assert!(!fs::read_to_string(wire).unwrap().contains("POSIX-RENAME"));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v3_save_as_publishes_a_new_file_without_disturbing_existing_names() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    let folder = root.join("work");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("existing"), b"keep").unwrap();
    let approved = Some(pane.frames.load().connection_generation);
    let error = guarded_job(
        &pane,
        approved,
        save("/work/existing", Some("keep"), "edit"),
    )
    .await
    .unwrap_err();
    assert!(error.to_ascii_lowercase().contains("save as"), "{error}");
    assert!(
        guarded_job(&pane, approved, save("/work/existing", None, "wrong"))
            .await
            .is_err()
    );
    fs::write(root.join("race-next-rename"), []).unwrap();
    assert!(
        guarded_job(&pane, approved, save("/work/raced", None, "wrong"))
            .await
            .is_err()
    );
    assert_eq!(fs::read(folder.join("raced")).unwrap(), b"racing entry");
    assert_eq!(fs::read(folder.join("existing")).unwrap(), b"keep");
    guarded_job(&pane, approved, save("/work/new", None, "new 雪\r\n"))
        .await
        .unwrap();
    assert_eq!(
        fs::read(folder.join("new")).unwrap(),
        "new 雪\r\n".as_bytes()
    );
    assert_eq!(names(&folder), ["existing", "new", "raced"]);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn save_as_losing_publication_acknowledgement_is_reported_and_never_replayed() {
    let (Source::Ssh(mut ssh), remote, _, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    let root = remote.path();
    fs::write(root.join("disconnect-after-rename"), []).unwrap();
    fs::write(root.join("rename-trace.jsonl"), []).unwrap();
    let folder = root.join("work");
    fs::create_dir(&folder).unwrap();
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
            let error = guarded_job(&pane, Some(1), save("/work/new", None, "published"))
                .await
                .unwrap_err();
            assert!(
                error.contains("outcome unknown")
                    && error.contains("refresh and check the destination"),
                "{error}"
            );
        }
    }
    // Complete a fresh request on the new SSH connection before counting packets.
    guarded_job(
        &pane,
        Some(2),
        Operation::Lstat {
            path: "/work/new".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(fs::read(folder.join("new")).unwrap(), b"published");
    assert_eq!(names(&folder), ["new"]);
    let renames = mutation_trace(root, "rename-trace.jsonl");
    assert_eq!(renames.len(), 1);
    assert!(renames[0][0].starts_with("/work/.w11term-edit-"));
    assert_eq!(renames[0][1], "/work/new");
    server.abort();
}
