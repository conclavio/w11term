use super::*;
use sha2::{Digest, Sha256};
use w11term::sftp::{Operation, UtilityArchitecture, UtilityBuild, UtilityBundle, UtilityTool};

impl Files {
    pub(super) fn utility_fixture_link(&self, path: &str) -> Option<String> {
        let file = fs::File::open(self.path(path)).ok()?;
        let mut text = String::new();
        file.take(4096).read_to_string(&mut text).ok()?;
        text.strip_prefix("fixture-tool-symlink\n")?
            .strip_suffix('\n')
            .map(str::to_owned)
    }
}

fn probe_response(root: &std::path::Path, arch: &str, home: &str, path: &str) {
    fs::write(
        root.join("utility-probe-response"),
        format!("SSH banner\nw11term-tools-v1\nLinux\n{arch}\n{home}\n{path}\nglibc 2.36\n"),
    )
    .unwrap();
}

// Minimal ELF programs for Linux x86-64 and AArch64 that exit with status 0.
// The optional interpreter segment lets a fixture exercise missing-loader diagnostics.
fn executable(architecture: UtilityArchitecture, interpreter: Option<&str>) -> Vec<u8> {
    let mut bytes = vec![0u8; 256];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    let (machine, code): (u16, &[u8]) = match architecture {
        UtilityArchitecture::X86_64 => (62, &[0xb8, 60, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05]),
        UtilityArchitecture::Aarch64 => (
            183,
            &[0xa8, 0x0b, 0x80, 0xd2, 0, 0, 0x80, 0xd2, 0x01, 0, 0, 0xd4],
        ),
    };
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&0x0040_00e0u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&(1 + u16::from(interpreter.is_some())).to_le_bytes());
    bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
    bytes[68..72].copy_from_slice(&5u32.to_le_bytes());
    bytes[80..88].copy_from_slice(&0x0040_0000u64.to_le_bytes());
    bytes[96..104].copy_from_slice(&256u64.to_le_bytes());
    bytes[104..112].copy_from_slice(&256u64.to_le_bytes());
    bytes[112..120].copy_from_slice(&4096u64.to_le_bytes());
    if let Some(loader) = interpreter {
        bytes[120..124].copy_from_slice(&3u32.to_le_bytes());
        bytes[124..128].copy_from_slice(&4u32.to_le_bytes());
        bytes[128..136].copy_from_slice(&176u64.to_le_bytes());
        bytes[152..160].copy_from_slice(&(loader.len() as u64 + 1).to_le_bytes());
        bytes[160..168].copy_from_slice(&(loader.len() as u64 + 1).to_le_bytes());
        bytes[176..176 + loader.len()].copy_from_slice(loader.as_bytes());
    }
    bytes[224..224 + code.len()].copy_from_slice(code);
    bytes
}

fn build(
    directory: &std::path::Path,
    filename: &str,
    architecture: UtilityArchitecture,
    content: Vec<u8>,
) -> UtilityBuild {
    let path = directory.join(filename);
    fs::write(&path, &content).unwrap();
    UtilityBuild {
        architecture,
        path,
        sha256: Sha256::digest(&content)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        release: None,
    }
}

fn preview(builds: Vec<UtilityBuild>) -> Operation {
    Operation::PreviewToolBundle {
        bundle: UtilityBundle {
            version: 1,
            name: "portable".into(),
            bin_directory: ".local/bin".into(),
            tools: vec![UtilityTool {
                name: "rg".into(),
                builds,
            }],
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn utility_preview_resolves_linux_builds_and_preserves_a_user_command() {
    use UtilityArchitecture::{Aarch64, X86_64};
    let (pane, _, root, opens, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    probe_response(root.path(), "x86_64", "/", "/usr/bin:/.local/bin");
    let builds = || {
        vec![
            build(local.path(), "arm", Aarch64, executable(Aarch64, None)),
            build(
                local.path(),
                "gnu",
                X86_64,
                executable(X86_64, Some("/lib64/missing-loader")),
            ),
            build(local.path(), "musl", X86_64, executable(X86_64, None)),
        ]
    };

    let plan = guarded_job(&pane, None, preview(builds())).await.unwrap();
    assert_eq!(
        plan["host"],
        json!({"os":"Linux","architecture":"x86_64","home":"/","glibc":"2.36.0"})
    );
    assert_eq!(plan["path_status"], "present");
    assert_eq!(plan["ready"], true);
    assert_eq!(plan["tools"][0]["destination"], "/.local/bin/rg");
    assert_eq!(plan["tools"][0]["launcher"], "create");
    assert_eq!(plan["tools"][0]["selected_build"], 2);
    assert_eq!(
        plan["tools"][0]["builds"][0]["status"],
        "other-architecture"
    );
    assert_eq!(
        plan["tools"][0]["builds"][1]["status"],
        "runtime-unverified"
    );
    assert_eq!(
        plan["tools"][0]["builds"][1]["verified"]["interpreter"],
        "/lib64/missing-loader"
    );
    assert_eq!(plan["tools"][0]["builds"][2]["verified"]["bytes"], 256);
    let id = plan["bundle_sha256"].as_str().unwrap();
    assert_eq!(id.len(), 64);
    assert_eq!(
        plan["version_directory"],
        format!("/.local/share/w11term/tools/portable/versions/{id}")
    );
    assert!(!root.path().join(".local").exists());

    fs::create_dir_all(root.path().join(".local/bin")).unwrap();
    let existing = root.path().join(".local/bin/rg");
    fs::write(&existing, "my own rg").unwrap();
    let conflict = guarded_job(&pane, None, preview(builds())).await.unwrap();
    assert_eq!(conflict["tools"][0]["launcher"], "conflict");
    assert_eq!(conflict["bundle_sha256"], plan["bundle_sha256"]);
    assert_eq!(conflict["ready"], false);
    assert_eq!(fs::read_to_string(existing).unwrap(), "my own rg");
    assert!(!root.path().join(".local/share").exists());
    assert_eq!(
        opens.load(Ordering::Relaxed),
        4,
        "one shell, one reused SFTP channel, two compatibility probes"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn utility_preview_rejects_wrong_machine_hash_and_account_home() {
    use UtilityArchitecture::{Aarch64, X86_64};
    let (pane, _, root, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    probe_response(root.path(), "aarch64", "/", "/usr/bin");
    let builds = || {
        let mut changed = build(local.path(), "changed", Aarch64, executable(Aarch64, None));
        changed.sha256 = "a".repeat(64);
        vec![
            changed,
            build(
                local.path(),
                "wrong-machine",
                Aarch64,
                executable(X86_64, None),
            ),
            build(local.path(), "arm", Aarch64, executable(Aarch64, None)),
        ]
    };
    let plan = guarded_job(&pane, None, preview(builds())).await.unwrap();
    assert_eq!(plan["host"]["architecture"], "aarch64");
    assert_eq!(plan["path_status"], "missing");
    assert_eq!(plan["tools"][0]["selected_build"], 2);
    let candidates = plan["tools"][0]["builds"].as_array().unwrap();
    assert!(
        candidates[0]["reason"]
            .as_str()
            .unwrap()
            .contains("SHA-256")
    );
    assert!(
        candidates[1]["reason"]
            .as_str()
            .unwrap()
            .contains("machine")
    );

    probe_response(root.path(), "aarch64", "/different-home", "/usr/bin");
    let error = guarded_job(&pane, None, preview(builds()))
        .await
        .unwrap_err();
    assert!(error.contains("disagree on the account's home"), "{error}");
    assert!(!root.path().join(".local").exists());
    server.abort();
}

fn apply(preview: Operation, report: &Value) -> Operation {
    let Operation::PreviewToolBundle { bundle } = preview else {
        unreachable!()
    };
    Operation::ApplyToolBundle {
        bundle,
        review: report["review"].as_str().unwrap().into(),
    }
}

fn rollback(id: &str, preview: Option<&Value>) -> Operation {
    Operation::RollbackToolBundle {
        name: "portable".into(),
        bundle_sha256: id.into(),
        review: preview.map(|report| report["review"].as_str().unwrap().into()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn utility_install_updates_and_rolls_back_one_atomic_selector() {
    let (pane, _, root, _, server) = fixture(false).await;
    let remote = root.path();
    let local = tempfile::tempdir().unwrap();
    probe_response(remote, "x86_64", "/", "/usr/bin:/.local/bin");
    fs::write(remote.join("posix-rename-version"), "1").unwrap();
    fs::write(remote.join("transfer-trace"), []).unwrap();
    fs::write(remote.join("fixture-copy-data"), []).unwrap();
    let version = |byte| {
        let mut binary = executable(UtilityArchitecture::X86_64, None);
        binary[255] = byte;
        binary.resize(96 * 1024, 0);
        let operation = preview(vec![build(
            local.path(),
            "rg",
            UtilityArchitecture::X86_64,
            binary,
        )]);
        let Operation::PreviewToolBundle { mut bundle } = operation else {
            unreachable!()
        };
        bundle.tools.push(UtilityTool {
            name: "jq".into(),
            builds: vec![build(
                local.path(),
                "jq",
                UtilityArchitecture::X86_64,
                executable(UtilityArchitecture::X86_64, None),
            )],
        });
        Operation::PreviewToolBundle { bundle }
    };
    let a = guarded_job(&pane, None, version(1)).await.unwrap();
    let a_id = a["bundle_sha256"].as_str().unwrap();
    let installed = guarded_job(&pane, None, apply(version(1), &a))
        .await
        .unwrap();
    assert_eq!(installed["uploaded"], true);
    assert_eq!(installed["changed"], true);
    let store = remote.join(".local/share/w11term/tools/portable");
    let selector = || fs::read_to_string(store.join("current")).unwrap();
    let launcher = || fs::read_to_string(remote.join(".local/bin/rg")).unwrap();
    assert!(selector().ends_with(&format!("versions/{a_id}\n")));
    let original_launcher = launcher();
    assert!(original_launcher.ends_with("/portable/current/rg\n"));
    assert_eq!(
        fs::read(store.join("versions").join(a_id).join("rg")).unwrap()[255],
        1
    );
    let Operation::PreviewToolBundle { mut bundle } = version(1) else {
        unreachable!()
    };
    bundle.bin_directory = "portable-bin".into();
    let moved = guarded_job(&pane, None, Operation::PreviewToolBundle { bundle })
        .await
        .unwrap();
    assert_eq!(moved["ready"], false);
    assert_eq!(moved["bin_matches_active"], false);

    fs::write(remote.join("transfer-trace"), []).unwrap();
    let repeat = guarded_job(&pane, None, version(1)).await.unwrap();
    let unchanged = guarded_job(&pane, None, apply(version(1), &repeat))
        .await
        .unwrap();
    assert_eq!(unchanged["changed"], false);
    assert_eq!(unchanged["uploaded"], false);
    assert!(
        !fs::read_to_string(remote.join("transfer-trace"))
            .unwrap()
            .contains("WRITE")
    );

    let b = guarded_job(&pane, None, version(2)).await.unwrap();
    let error = guarded_job(&pane, None, apply(version(2), &repeat))
        .await
        .unwrap_err();
    assert!(error.contains("review is stale"), "{error}");
    fs::write(remote.join("fail-next-write"), []).unwrap();
    let failed = guarded_job(&pane, None, apply(version(2), &b))
        .await
        .unwrap_err();
    assert!(failed.contains("activation did not finish"), "{failed}");
    assert!(selector().ends_with(&format!("versions/{a_id}\n")));
    assert_eq!(fs::read_dir(store.join("versions")).unwrap().count(), 1);
    fs::remove_file(remote.join("fail-next-write")).unwrap();
    let installed = guarded_job(&pane, None, apply(version(2), &b))
        .await
        .unwrap();
    let b_id = b["bundle_sha256"].as_str().unwrap();
    assert_eq!(installed["previous_bundle_sha256"], a_id);
    assert_eq!(installed["copied_tools"], json!(["jq"]));
    assert!(
        fs::read_to_string(remote.join("transfer-trace"))
            .unwrap()
            .contains("COPY-DATA")
    );
    assert!(selector().ends_with(&format!("versions/{b_id}\n")));
    assert_eq!(launcher(), original_launcher);

    fs::remove_file(local.path().join("rg")).unwrap();
    fs::remove_file(local.path().join("jq")).unwrap();
    let original = fs::read(store.join("versions").join(a_id).join("rg")).unwrap();
    let review = guarded_job(&pane, None, rollback(a_id, None))
        .await
        .unwrap();
    let old_path = store.join("versions").join(a_id).join("rg");
    let mut changed = original.clone();
    changed[255] = 77;
    fs::write(&old_path, &changed).unwrap();
    let error = guarded_job(&pane, None, rollback(a_id, Some(&review)))
        .await
        .unwrap_err();
    assert!(error.contains("contents changed"), "{error}");
    assert!(selector().ends_with(&format!("versions/{b_id}\n")));
    fs::write(old_path, original).unwrap();
    let review = guarded_job(&pane, None, rollback(a_id, None))
        .await
        .unwrap();
    let restored = guarded_job(&pane, None, rollback(a_id, Some(&review)))
        .await
        .unwrap();
    assert_eq!(restored["previous_bundle_sha256"], b_id);
    assert_eq!(restored["uploaded"], false);
    assert!(selector().ends_with(&format!("versions/{a_id}\n")));
    assert_eq!(launcher(), original_launcher);
    let trace = fs::read_to_string(remote.join("transfer-trace")).unwrap();
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.starts_with("POSIX-RENAME"))
            .count(),
        2
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn utility_install_preserves_racing_user_command_and_checks_remote_lock() {
    let (pane, _, root, _, server) = fixture(false).await;
    let remote = root.path();
    let local = tempfile::tempdir().unwrap();
    probe_response(remote, "aarch64", "/", "/usr/bin");
    fs::write(remote.join("posix-rename-version"), "1").unwrap();
    let version = || {
        preview(vec![build(
            local.path(),
            "rg",
            UtilityArchitecture::Aarch64,
            executable(UtilityArchitecture::Aarch64, None),
        )])
    };
    let first = guarded_job(&pane, None, version()).await.unwrap();
    fs::write(remote.join("hold-utility-lock"), []).unwrap();
    let failed = guarded_job(&pane, None, apply(version(), &first))
        .await
        .unwrap_err();
    assert!(failed.contains("Could not lock"), "{failed}");
    let store = remote.join(".local/share/w11term/tools/portable");
    assert!(!store.join("current").exists());
    assert_eq!(fs::read_dir(store.join("versions")).unwrap().count(), 0);
    fs::remove_file(remote.join("hold-utility-lock")).unwrap();

    let before = guarded_job(&pane, None, version()).await.unwrap();
    fs::write(remote.join("race-tool-launcher"), []).unwrap();
    let error = guarded_job(&pane, None, apply(version(), &before))
        .await
        .unwrap_err();
    assert!(error.contains("Creating launcher"), "{error}");
    assert_eq!(
        fs::read(remote.join(".local/bin/rg")).unwrap(),
        b"my racing command"
    );
    assert!(!store.join("current").exists());

    fs::remove_file(remote.join("race-tool-launcher")).unwrap();
    fs::remove_file(remote.join(".local/bin/rg")).unwrap();
    let reviewed = guarded_job(&pane, None, version()).await.unwrap();
    let installed = guarded_job(&pane, None, apply(version(), &reviewed))
        .await
        .unwrap();
    assert_eq!(
        installed["uploaded"], false,
        "reuse the verified version staged in the earlier attempt"
    );
    assert_eq!(installed["changed"], true);
    assert!(store.join("current").exists());
    server.abort();
}
