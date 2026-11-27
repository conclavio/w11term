use super::*;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use w11term::sftp::{
    InstalledToolPath, Operation, UtilityArchitecture, UtilityBuild, UtilityBundle,
    UtilityDownload, UtilityTool,
};
use w11term::{
    options::{ToolPath, ToolSync},
    sftp::DotfileShell,
};

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

pub(super) fn tool_digest_report(
    root: &std::path::Path,
    args: &str,
) -> anyhow::Result<Option<String>> {
    let mode = fs::read_to_string(root.join("utility-digest-mode")).unwrap_or_default();
    if mode.is_empty() || mode == "refused" {
        return Ok(None);
    }
    if mode == "malformed" {
        return Ok(Some("w11term-tools-sha256-v1\npartial result\n".into()));
    }
    let unquote = |token: &str| -> String {
        token
            .strip_prefix('\'')
            .unwrap()
            .strip_suffix('\'')
            .unwrap()
            .to_owned()
    };
    // This fixture uses a home and tool names without any embedded spaces or quotes.
    let mut args = args.split_ascii_whitespace();
    let directory = unquote(args.next().unwrap());
    assert!(directory.starts_with("/.local/share/w11term/tools/"));
    let directory = root.join(directory.trim_start_matches('/'));
    let mut report = String::from("fixture banner\nw11term-tools-sha256-v1\n");
    while let Some(file) = args.next() {
        let file = unquote(file);
        let limit: u64 = args.next().unwrap().parse()?;
        let mut bytes = Vec::new();
        fs::File::open(directory.join(&file))?
            .take(limit)
            .read_to_end(&mut bytes)?;
        if mode == "different-manifest" && file == ".w11term-manifest.json" {
            writeln!(report, "{:064x}", 0)?;
        } else {
            writeln!(report, "{}", tool_sha256(&bytes))?;
        }
    }
    Ok(Some(report))
}

// Minimal ELF programs for Linux x86-64 and AArch64 that exit with status 0.
// The optional interpreter segment lets a fixture exercise missing-loader diagnostics.
pub(super) fn executable(architecture: UtilityArchitecture, interpreter: Option<&str>) -> Vec<u8> {
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

fn sftp_opened_tool(trace: &std::path::Path) -> bool {
    fs::read_to_string(trace)
        .unwrap()
        .lines()
        .any(|line| line.starts_with("OPEN ") && line.ends_with("/rg"))
}

pub(super) fn tool_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
        sha256: tool_sha256(&content),
        release: None,
        download: None,
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
async fn local_tool_bundle_pins_both_architectures_for_reviewed_installation() {
    use UtilityArchitecture::{Aarch64, X86_64};
    use w11term::sftp::{LocalUtilityBundle, LocalUtilityExecutable};

    let local = tempfile::tempdir().unwrap();
    let x64 = local.path().join("operator-x64");
    let arm = local.path().join("operator-arm");
    let bogus = local.path().join("not-an-elf.exe");
    fs::write(&x64, executable(X86_64, None)).unwrap();
    fs::write(&arm, executable(Aarch64, None)).unwrap();
    fs::write(&bogus, b"MZ invalid").unwrap();
    assert!(LocalUtilityExecutable::inspect(&bogus).is_err());
    let x64_build = LocalUtilityExecutable::inspect(&x64).unwrap();
    let arm_build = LocalUtilityExecutable::inspect(&arm).unwrap();
    assert_eq!(x64_build.architecture, X86_64);
    assert_eq!(arm_build.architecture, Aarch64);
    let mut bundle = LocalUtilityBundle::default();
    assert!(bundle.add("-invalid", x64_build.clone()).is_err());
    bundle.add("operator", x64_build.clone()).unwrap();
    assert!(bundle.add("operator", x64_build).is_err());
    bundle.add("operator", arm_build).unwrap();
    let manifest = local.path().join("tools.json");
    bundle.save(&manifest, "ops", ".local/bin").unwrap();
    let saved = fs::read(&manifest).unwrap();
    assert!(bundle.save(&manifest, "other", ".local/bin").is_err());
    assert_eq!(fs::read(&manifest).unwrap(), saved);
    let document: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(document["tools"][0]["builds"][0]["path"], "operator-x64");

    let (pane, _, remote, _, server) = fixture(false).await;
    probe_response(remote.path(), "aarch64", "/", "/usr/bin:/.local/bin");
    fs::write(remote.path().join("posix-rename-version"), "1").unwrap();
    let operation = || Operation::PreviewToolBundle {
        bundle: UtilityBundle::load(&manifest).unwrap(),
    };
    let mut changed = executable(Aarch64, None);
    changed[255] = 1;
    fs::write(&arm, changed).unwrap();
    let blocked = guarded_job(&pane, None, operation()).await.unwrap();
    assert_eq!(blocked["ready"], false);
    assert!(blocked.to_string().contains("SHA-256 does not match"));
    let original = executable(Aarch64, None);
    fs::write(&arm, &original).unwrap();
    let plan = guarded_job(&pane, None, operation()).await.unwrap();
    assert_eq!(plan["ready"], true);
    let installed = guarded_job(&pane, None, apply(operation(), &plan))
        .await
        .unwrap();
    let digest = installed["bundle_sha256"].as_str().unwrap();
    assert_eq!(
        fs::read(remote.path().join(format!(
            ".local/share/w11term/tools/ops/versions/{digest}/operator"
        )))
        .unwrap(),
        original
    );
    server.abort();
}

#[test]
fn bundle_revision_preserves_pins_downloads_and_build_order() {
    use UtilityArchitecture::{Aarch64, X86_64};
    use w11term::sftp::{LocalUtilityBundle, LocalUtilityExecutable};

    let local = tempfile::tempdir().unwrap();
    let original = local.path().join("original.json");
    let previous = build(local.path(), "previous", X86_64, executable(X86_64, None));
    let arm = build(local.path(), "arm", Aarch64, executable(Aarch64, None));
    let download = json!({
        "architecture": "x86_64",
        "release": "https://example.com/releases/v1",
        "sha256": "a".repeat(64),
        "download": {
            "format": "tar-gz",
            "url": "https://example.com/releases/operator.tar.gz",
            "member": "operator",
            "archive_sha256": "b".repeat(64)
        }
    });
    fs::write(
        &original,
        serde_json::to_vec(&json!({
            "version": 1,
            "name": "team",
            "bin_directory": "bin",
            "tools": [
                {"name": "operator", "builds": [
                    {"architecture": "x86_64", "path": "previous", "sha256": previous.sha256},
                    download.clone(),
                    {"architecture": "aarch64", "path": "arm", "sha256": arm.sha256}
                ]}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let saved_original = fs::read(&original).unwrap();
    let source = UtilityBundle::load(&original).unwrap();
    let mut editor = LocalUtilityBundle::from_tools(source.tools);
    assert!(
        editor
            .replace(
                "operator",
                0,
                LocalUtilityExecutable::inspect(&arm.path).unwrap()
            )
            .is_err()
    );
    assert_eq!(editor.tools()[0].builds[0].sha256, previous.sha256);
    let mut updated_bytes = executable(X86_64, None);
    updated_bytes[255] = 1;
    let updated = build(local.path(), "updated", X86_64, updated_bytes);
    editor
        .replace(
            "operator",
            0,
            LocalUtilityExecutable::inspect(&updated.path).unwrap(),
        )
        .unwrap();
    assert!(
        editor
            .save(&original, &source.name, &source.bin_directory)
            .is_err()
    );
    assert_eq!(fs::read(&original).unwrap(), saved_original);

    let folder = local.path().join("elsewhere");
    fs::create_dir(&folder).unwrap();
    let revision = folder.join("revised.json");
    editor
        .save(&revision, &source.name, &source.bin_directory)
        .unwrap();
    let revised = UtilityBundle::load(&revision).unwrap();
    let builds = &revised.tools[0].builds;
    assert_eq!(revised.name, source.name);
    assert_eq!(revised.bin_directory, source.bin_directory);
    assert_eq!(builds[0].sha256, updated.sha256);
    assert_eq!(builds[0].path, updated.path);
    assert_eq!(serde_json::to_value(&builds[1]).unwrap(), download);
    assert_eq!(builds[2].path, arm.path);
    assert_eq!(builds[2].sha256, arm.sha256);
    editor.remove("operator", 1);
    assert_eq!(editor.tools()[0].builds.len(), 2);
    assert_eq!(editor.tools()[0].builds[1].sha256, arm.sha256);
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
async fn removing_managed_tools_keeps_exact_pins_and_can_be_rolled_back() {
    let (pane, _, root, _, server) = fixture(false).await;
    let remote = root.path();
    probe_response(remote, "x86_64", "/", "/usr/bin:/.local/bin");
    fs::write(remote.join("posix-rename-version"), "1").unwrap();
    let local = tempfile::tempdir().unwrap();
    let arch = UtilityArchitecture::X86_64;
    let rg = executable(arch, None);
    let mut mdd = rg.clone();
    mdd.push(1);
    let bundle = UtilityBundle {
        version: 1,
        name: "portable".into(),
        bin_directory: ".local/bin".into(),
        tools: vec![
            UtilityTool {
                name: "mdd".into(),
                builds: vec![build(local.path(), "mdd", arch, mdd.clone())],
            },
            UtilityTool {
                name: "rg".into(),
                builds: vec![build(local.path(), "rg", arch, rg.clone())],
            },
        ],
    };
    let preview = || Operation::PreviewToolBundle {
        bundle: bundle.clone(),
    };
    let plan = guarded_job(&pane, None, preview()).await.unwrap();
    let original = guarded_job(&pane, None, apply(preview(), &plan))
        .await
        .unwrap();
    let original_id = original["bundle_sha256"].as_str().unwrap();
    let store = remote.join(".local/share/w11term/tools/portable");
    let launcher = remote.join(".local/bin/rg");
    let original_launcher = fs::read(&launcher).unwrap();
    let original_selector = fs::read(store.join("current")).unwrap();
    let remove = |tool: &str, preview: Option<&Value>| Operation::RemoveFromToolBundle {
        name: "portable".into(),
        tool: tool.into(),
        review: preview.map(|report| report["review"].as_str().unwrap().into()),
    };
    fs::remove_file(&bundle.tools[0].builds[0].path).unwrap();
    fs::remove_file(&bundle.tools[1].builds[0].path).unwrap();

    let plan = guarded_job(&pane, None, remove("rg", None)).await.unwrap();
    assert_eq!(plan["ready"], true);
    assert_eq!(plan["current"]["version"], original_id);
    assert_eq!(plan["removed"]["name"], "rg");
    assert_eq!(plan["launcher_status"], "managed");
    assert_eq!(plan["kept"][0]["sha256"], tool_sha256(&mdd));
    assert_eq!(fs::read(store.join("current")).unwrap(), original_selector);

    fs::write(&launcher, "operator-owned rg").unwrap();
    let stale = guarded_job(&pane, None, remove("rg", Some(&plan)))
        .await
        .unwrap_err();
    assert!(stale.contains("review is stale"), "{stale}");
    assert_eq!(fs::read(store.join("current")).unwrap(), original_selector);
    let plan = guarded_job(&pane, None, remove("rg", None)).await.unwrap();
    assert_eq!(plan["launcher_status"], "conflict");
    let removed = guarded_job(&pane, None, remove("rg", Some(&plan)))
        .await
        .unwrap();
    let smaller = removed["bundle_sha256"].as_str().unwrap();
    assert_eq!(removed["automatic_sync_paused"], true);
    assert_eq!(removed["copied_tools"], json!(["mdd"]));
    assert_eq!(fs::read_to_string(&launcher).unwrap(), "operator-owned rg");
    assert_eq!(
        fs::read(store.join(format!("versions/{smaller}/mdd"))).unwrap(),
        mdd
    );
    assert!(!store.join(format!("versions/{smaller}/rg")).exists());
    assert_eq!(
        fs::read(store.join(format!("versions/{original_id}/rg"))).unwrap(),
        rg
    );
    let installed = guarded_job(&pane, None, Operation::InstalledTools)
        .await
        .unwrap();
    assert_eq!(installed["tools"].as_array().unwrap().len(), 1);
    assert_eq!(installed["tools"][0]["name"], "mdd");

    fs::write(&launcher, original_launcher).unwrap();
    let last = guarded_job(&pane, None, remove("mdd", None)).await.unwrap();
    assert_eq!(last["kept"], json!([]));
    let empty = guarded_job(&pane, None, remove("mdd", Some(&last)))
        .await
        .unwrap();
    let empty_id = empty["bundle_sha256"].as_str().unwrap();
    assert_eq!(empty["automatic_sync_paused"], true);
    let installed = guarded_job(&pane, None, Operation::InstalledTools)
        .await
        .unwrap();
    assert_eq!(installed["tools"], json!([]));
    let versions = guarded_job(&pane, None, Operation::ToolBundleVersions)
        .await
        .unwrap();
    assert_eq!(versions["versions"][0]["bundle_sha256"], empty_id);
    assert_eq!(versions["versions"][0]["held"], true);
    let snapshots = local.path().join("snapshots");
    let save = Operation::SaveInstalledToolBundle {
        name: "portable".into(),
        bundle_sha256: empty_id.into(),
        directory: snapshots.clone(),
    };
    let error = guarded_job(&pane, pane.ssh_connection_generation(), save)
        .await
        .unwrap_err();
    assert!(error.contains("no SSH tools to save"), "{error}");
    assert!(!snapshots.exists());

    let restore = guarded_job(&pane, None, rollback(original_id, None))
        .await
        .unwrap();
    assert_eq!(restore["ready"], true);
    let restored = guarded_job(&pane, None, rollback(original_id, Some(&restore)))
        .await
        .unwrap();
    assert_eq!(restored["bundle_sha256"], original_id);
    let installed = guarded_job(&pane, None, Operation::InstalledTools)
        .await
        .unwrap();
    assert_eq!(installed["tools"].as_array().unwrap().len(), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_tool_bundle_saves_exact_pins_for_profile_reuse() {
    let (pane, _, root, _, server) = fixture(false).await;
    let remote = root.path();
    probe_response(remote, "x86_64", "/", "/usr/bin");
    fs::write(remote.join("posix-rename-version"), "1").unwrap();
    let local = tempfile::tempdir().unwrap();
    let destination = local.path().join("snapshots");
    let mut bundle = UtilityBundle {
        version: 1,
        name: "portable".into(),
        bin_directory: ".local/custom-bin".into(),
        tools: Vec::new(),
    };
    let arch = UtilityArchitecture::X86_64;
    for (index, name) in ["mdd", "rg"].into_iter().enumerate() {
        let mut bytes = executable(arch, None);
        bytes.push(index as u8);
        bundle.tools.push(UtilityTool {
            name: name.into(),
            builds: vec![build(local.path(), name, arch, bytes)],
        });
    }
    let preview = || Operation::PreviewToolBundle {
        bundle: bundle.clone(),
    };
    let review = guarded_job(&pane, None, preview()).await.unwrap();
    let installed = guarded_job(&pane, None, apply(preview(), &review))
        .await
        .unwrap();
    let id = installed["bundle_sha256"].as_str().unwrap();
    let store = remote.join(".local/share/w11term/tools/portable");
    let selector = fs::read(store.join("current")).unwrap();
    let selected_file = store.join(format!("versions/{id}/rg"));
    let original = fs::read(&selected_file).unwrap();
    let save = |id: &str| Operation::SaveInstalledToolBundle {
        name: bundle.name.clone(),
        bundle_sha256: id.into(),
        directory: destination.clone(),
    };
    let generation = pane.ssh_connection_generation().unwrap();
    let error = guarded_job(&pane, None, save(id)).await.unwrap_err();
    assert!(error.contains("original SSH connection"), "{error}");
    let error = guarded_job(&pane, Some(generation), save(&"0".repeat(64)))
        .await
        .unwrap_err();
    assert!(error.contains("active tool bundle changed"), "{error}");
    assert!(!destination.exists());

    let mut corrupted = original.clone();
    *corrupted.last_mut().unwrap() ^= 1;
    fs::write(&selected_file, corrupted).unwrap();
    let error = guarded_job(&pane, Some(generation), save(id))
        .await
        .unwrap_err();
    assert!(error.contains("SHA-256 mismatch"), "{error}");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
    fs::write(&selected_file, original).unwrap();

    let saved = guarded_job(&pane, Some(generation), save(id))
        .await
        .unwrap();
    assert_eq!(saved["home"], "/");
    let manifest = std::path::Path::new(saved["manifest"].as_str().unwrap());
    assert!(manifest.starts_with(&destination));
    let exported = UtilityBundle::load(manifest).unwrap();
    assert_eq!(exported.name, bundle.name);
    assert_eq!(exported.bin_directory, bundle.bin_directory);
    assert_eq!(exported.tools.len(), bundle.tools.len());
    for (saved_tool, original_tool) in exported.tools.iter().zip(bundle.tools) {
        assert_eq!(saved_tool.name, original_tool.name);
        let [saved_build] = saved_tool.builds.as_slice() else {
            panic!("expected the single installed architecture");
        };
        let original_build = &original_tool.builds[0];
        assert_eq!(saved_build.architecture, arch);
        assert_eq!(saved_build.sha256, original_build.sha256);
        assert!(saved_build.path.starts_with(manifest.parent().unwrap()));
        assert_eq!(
            fs::read(&saved_build.path).unwrap(),
            fs::read(&original_build.path).unwrap()
        );
        fs::remove_file(&original_build.path).unwrap();
    }
    let restored = guarded_job(
        &pane,
        None,
        Operation::PreviewToolBundle { bundle: exported },
    )
    .await
    .unwrap();
    assert_eq!(restored["bundle_sha256"], id);
    assert_eq!(restored["ready"], true);
    assert_eq!(fs::read(store.join("current")).unwrap(), selector);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn utility_install_uses_a_verified_download_cache_without_network() {
    const CHILD: &str = "W11TERM_UTILITY_CACHE_TEST";
    let Some(state) = std::env::var_os(CHILD).map(std::path::PathBuf::from) else {
        let state = tempfile::tempdir().unwrap();
        // The other fixtures run in parallel. Set the cache override only in this child.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "utilities::utility_install_uses_a_verified_download_cache_without_network",
            ])
            .env(CHILD, state.path())
            .env("W11TERM_DATA_DIR", state.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    let (pane, _, root, _, server) = fixture(false).await;
    probe_response(root.path(), "x86_64", "/", "/usr/bin:/.local/bin");
    fs::write(root.path().join("posix-rename-version"), "1").unwrap();
    let bytes = executable(UtilityArchitecture::X86_64, None);
    let mut candidate = build(
        &state,
        "fixture-elf",
        UtilityArchitecture::X86_64,
        bytes.clone(),
    );
    fs::remove_file(std::mem::take(&mut candidate.path)).unwrap();
    candidate.download = Some(UtilityDownload::Raw {
        url: "https://example.invalid/never-fetched-on-connect".into(),
    });
    let cache = state.join("tools").join("cache").join("v1");
    fs::create_dir_all(&cache).unwrap();
    let cached = cache.join(&candidate.sha256);
    let operation = preview(vec![candidate]);
    let request = serde_json::to_value(&operation).unwrap();
    let preview_request = || serde_json::from_value(request.clone()).unwrap();

    let missing = guarded_job(&pane, None, preview_request()).await.unwrap();
    assert_eq!(missing["ready"], false);
    assert!(
        missing["tools"][0]["builds"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("fetch-tool-bundle")
    );
    fs::write(&cached, &bytes).unwrap();
    let plan = guarded_job(&pane, None, preview_request()).await.unwrap();
    assert_eq!(plan["ready"], true);
    assert_eq!(plan["tools"][0]["builds"][0]["path"], json!(cached));
    let installed = guarded_job(&pane, None, apply(operation, &plan))
        .await
        .unwrap();
    assert_eq!(installed["uploaded"], true);
    let release = plan["bundle_sha256"].as_str().unwrap();
    let installed_path = root
        .path()
        .join(".local/share/w11term/tools/portable/versions")
        .join(release)
        .join("rg");
    assert_eq!(fs::read(installed_path).unwrap(), bytes);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_tool_verification_uses_server_hashes_with_sftp_fallback() {
    let (pane, _, root, _, server) = fixture(false).await;
    let remote = root.path();
    let local = tempfile::tempdir().unwrap();
    probe_response(remote, "x86_64", "/", "/usr/bin:/.local/bin");
    fs::write(remote.join("posix-rename-version"), "1").unwrap();
    let trace = remote.join("transfer-trace");
    let mode = remote.join("utility-digest-mode");
    let mut bytes = executable(UtilityArchitecture::X86_64, None);
    bytes.resize(128 * 1024, 0);
    let operation = || {
        preview(vec![build(
            local.path(),
            "rg",
            UtilityArchitecture::X86_64,
            bytes.clone(),
        )])
    };
    let preview = guarded_job(&pane, None, operation()).await.unwrap();
    guarded_job(&pane, None, apply(operation(), &preview))
        .await
        .unwrap();
    let reviewed = guarded_job(&pane, None, operation()).await.unwrap();
    let installed_path = remote
        .join(
            reviewed["version_directory"]
                .as_str()
                .unwrap()
                .trim_start_matches('/'),
        )
        .join("rg");
    for (behavior, expects_read) in [
        ("valid", false),
        ("refused", true),
        ("malformed", true),
        ("different-manifest", true),
    ] {
        fs::write(&mode, behavior).unwrap();
        fs::write(&trace, []).unwrap();
        let checked = guarded_job(&pane, None, apply(operation(), &reviewed))
            .await
            .unwrap();
        assert_eq!(checked["uploaded"], false);
        assert_eq!(checked["changed"], false);
        assert_eq!(sftp_opened_tool(&trace), expects_read, "{behavior}");
    }

    fs::write(&mode, "valid").unwrap();
    fs::write(&trace, []).unwrap();
    let mut changed = bytes.clone();
    changed[255] = 17;
    fs::write(&installed_path, changed).unwrap();
    let error = guarded_job(&pane, None, apply(operation(), &reviewed))
        .await
        .unwrap_err();
    assert!(
        error.contains("Managed executable contents changed"),
        "{error}"
    );
    assert!(!sftp_opened_tool(&trace));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_tool_path_requires_a_review_of_the_active_release_and_startup_files() {
    let (pane, _, root, _, server) = fixture(false).await;
    let remote = root.path();
    let local = tempfile::tempdir().unwrap();
    probe_response(remote, "x86_64", "/", "/usr/bin");
    fs::write(remote.join("posix-rename-version"), "1").unwrap();
    let launcher_dir = "operator/bin";
    let rc = remote.join(".bashrc");
    fs::write(&rc, "# existing settings\n").unwrap();
    let Operation::PreviewToolBundle { mut bundle } = preview(vec![build(
        local.path(),
        "rg",
        UtilityArchitecture::X86_64,
        executable(UtilityArchitecture::X86_64, None),
    )]) else {
        unreachable!()
    };
    bundle.bin_directory = launcher_dir.into();
    let operation = || Operation::PreviewToolBundle {
        bundle: bundle.clone(),
    };
    let reviewed = guarded_job(&pane, None, operation()).await.unwrap();
    let installed = guarded_job(&pane, None, apply(operation(), &reviewed))
        .await
        .unwrap();
    assert_eq!(installed["path_status"], "missing");
    let id = installed["bundle_sha256"].as_str().unwrap();
    let path_change =
        |shell, zdotdir: Option<&str>, remove, review| Operation::ReviewInstalledToolPath {
            installed: InstalledToolPath {
                bundle: bundle.name.clone(),
                bundle_sha256: id.to_owned(),
                path: ToolPath {
                    shell,
                    zdotdir: zdotdir.map(str::to_owned),
                },
                remove,
            },
            review,
        };
    let setup = |shell, review| path_change(shell, None, false, review);
    let bash = DotfileShell::Bash;
    let generation = pane.ssh_connection_generation().unwrap();
    let error = guarded_job(&pane, None, setup(bash, None))
        .await
        .unwrap_err();
    assert!(error.contains("original SSH connection"), "{error}");

    let plan = guarded_job(&pane, Some(generation), setup(bash, None))
        .await
        .unwrap();
    assert_eq!(plan["bundle_sha256"], id);
    assert_eq!(plan["bin_directory"], launcher_dir);
    assert_eq!(plan["ready"], true);
    assert_eq!(fs::read_to_string(&rc).unwrap(), "# existing settings\n");
    assert!(!remote.join(".profile").exists());
    let token = || Some(plan["review"].as_str().unwrap().to_owned());
    let wrong_shell = guarded_job(&pane, Some(generation), setup(DotfileShell::Zsh, token()))
        .await
        .unwrap_err();
    assert!(wrong_shell.contains("review is stale"), "{wrong_shell}");
    assert!(!remote.join(".zshrc").exists());

    let selector = remote.join(".local/share/w11term/tools/portable/current");
    let active = fs::read_to_string(&selector).unwrap();
    fs::write(&selector, active.replace(id, &"f".repeat(64))).unwrap();
    let changed = guarded_job(&pane, Some(generation), setup(bash, token()))
        .await
        .unwrap_err();
    assert!(
        changed.contains("installed tool bundle changed"),
        "{changed}"
    );
    fs::write(&selector, &active).unwrap();

    let launcher = remote.join(launcher_dir).join("rg");
    let link = fs::read_to_string(&launcher).unwrap();
    fs::write(&launcher, "operator-owned launcher").unwrap();
    let conflict = guarded_job(&pane, Some(generation), setup(bash, token()))
        .await
        .unwrap_err();
    assert!(conflict.contains("managed launcher changed"), "{conflict}");
    assert_eq!(
        fs::read_to_string(&launcher).unwrap(),
        "operator-owned launcher"
    );
    fs::write(&launcher, link).unwrap();

    fs::write(&rc, "# edited after preview\n").unwrap();
    let stale = guarded_job(&pane, Some(generation), setup(bash, token()))
        .await
        .unwrap_err();
    assert!(stale.contains("review is stale"), "{stale}");
    assert!(!remote.join(".profile").exists());
    let fresh = guarded_job(&pane, Some(generation), setup(bash, None))
        .await
        .unwrap();
    let applied = guarded_job(
        &pane,
        Some(generation),
        setup(bash, Some(fresh["review"].as_str().unwrap().to_owned())),
    )
    .await
    .unwrap();
    assert_eq!(applied["changed"], true);
    let content = fs::read_to_string(&rc).unwrap();
    assert!(content.starts_with("# edited after preview\n"));
    assert!(content.contains("PATH=\"${PATH:+$PATH:}$HOME/operator/bin\""));
    assert!(
        fs::read_to_string(remote.join(".profile"))
            .unwrap()
            .contains("$HOME/operator/bin")
    );
    assert_eq!(fs::read_to_string(&selector).unwrap(), active);
    let current = guarded_job(&pane, Some(generation), setup(bash, None))
        .await
        .unwrap();
    assert_eq!(current["ready"], false);
    assert!(current["review"].is_null());
    assert!(
        current["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["action"] == "unchanged")
    );
    let zdotdir = remote.join("config/zsh");
    fs::create_dir_all(&zdotdir).unwrap();
    let zsh = |review| path_change(DotfileShell::Zsh, Some("config/zsh"), false, review);
    let zsh_plan = guarded_job(&pane, Some(generation), zsh(None))
        .await
        .unwrap();
    assert_eq!(zsh_plan["files"][0]["path"], "/config/zsh/.zprofile");
    assert_eq!(zsh_plan["files"][1]["path"], "/config/zsh/.zshrc");
    let zsh_token = zsh_plan["review"].as_str().unwrap().to_owned();
    guarded_job(&pane, Some(generation), zsh(Some(zsh_token)))
        .await
        .unwrap();
    assert!(
        fs::read_to_string(zdotdir.join(".zshrc"))
            .unwrap()
            .contains("$HOME/operator/bin")
    );
    assert!(!remote.join(".zshrc").exists());
    assert_eq!(fs::read_to_string(&rc).unwrap(), content);

    let remove = |review| path_change(bash, None, true, review);
    let profile = remote.join(".profile");
    let profile_before = fs::read_to_string(&profile).unwrap();
    let higher_priority = remote.join(".bash_profile");
    fs::write(&higher_priority, "# added login file\n").unwrap();
    let edited = content.replace("export PATH ;;", "export PATH; echo retained ;;");
    fs::write(&rc, &edited).unwrap();
    let blocked = guarded_job(&pane, Some(generation), remove(None))
        .await
        .unwrap();
    assert_eq!(blocked["ready"], false);
    assert!(blocked["blocked"].as_str().unwrap().contains(".bashrc"));
    assert!(blocked["review"].is_null());
    assert_eq!(fs::read_to_string(&rc).unwrap(), edited);
    assert_eq!(fs::read_to_string(&profile).unwrap(), profile_before);
    fs::write(&rc, content.replace('\n', "\r\n")).unwrap();

    let removal = guarded_job(&pane, Some(generation), remove(None))
        .await
        .unwrap();
    assert_eq!(removal["remove"], true);
    assert_eq!(removal["ready"], true);
    assert_eq!(removal["files"][2]["path"], "/.profile");
    assert_eq!(removal["files"][2]["action"], "remove-hook");
    assert_eq!(removal["files"][2]["replaced_hook"], profile_before);
    let prior_token = removal["review"].as_str().unwrap().to_owned();
    let cross_action = guarded_job(
        &pane,
        Some(generation),
        setup(bash, Some(prior_token.clone())),
    )
    .await
    .unwrap_err();
    assert!(cross_action.contains("review is stale"), "{cross_action}");

    let updated = format!(
        "{}# outside the block\r\n",
        fs::read_to_string(&rc).unwrap()
    );
    fs::write(&rc, &updated).unwrap();
    let stale = guarded_job(&pane, Some(generation), remove(Some(prior_token)))
        .await
        .unwrap_err();
    assert!(stale.contains("review is stale"), "{stale}");
    assert_eq!(fs::read_to_string(&rc).unwrap(), updated);
    assert_eq!(fs::read_to_string(&profile).unwrap(), profile_before);

    fs::write(&launcher, "operator-owned launcher").unwrap();
    let removal = guarded_job(&pane, Some(generation), remove(None))
        .await
        .unwrap();
    let token = removal["review"].as_str().unwrap().to_owned();
    guarded_job(&pane, Some(generation), remove(Some(token)))
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(&rc).unwrap(),
        "# edited after preview\r\n# outside the block\r\n"
    );
    assert_eq!(fs::read_to_string(&profile).unwrap(), "");
    assert_eq!(
        fs::read_to_string(&higher_priority).unwrap(),
        "# added login file\n"
    );
    assert_eq!(
        fs::read_to_string(&launcher).unwrap(),
        "operator-owned launcher"
    );
    assert_eq!(fs::read_to_string(&selector).unwrap(), active);
    let removed = guarded_job(&pane, Some(generation), remove(None))
        .await
        .unwrap();
    assert_eq!(removed["ready"], false);
    assert!(removed["review"].is_null());
    assert!(removed["blocked"].is_null());

    let remove_zsh = |review| path_change(DotfileShell::Zsh, Some("config/zsh"), true, review);
    let zsh_removal = guarded_job(&pane, Some(generation), remove_zsh(None))
        .await
        .unwrap();
    let zsh_token = zsh_removal["review"].as_str().unwrap().to_owned();
    guarded_job(&pane, Some(generation), remove_zsh(Some(zsh_token)))
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(zdotdir.join(".zshrc")).unwrap(), "");
    assert!(!remote.join(".zshrc").exists());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn adding_tools_keeps_active_binaries_and_pauses_login_sync() {
    use UtilityArchitecture::X86_64;

    let (pane, _, root, _, server) = fixture(false).await;
    let remote = root.path();
    let local = tempfile::tempdir().unwrap();
    probe_response(remote, "x86_64", "/", "/usr/bin:/operator/bin");
    fs::write(remote.join("posix-rename-version"), "1").unwrap();
    let original = executable(X86_64, None);
    let Operation::PreviewToolBundle { mut bundle } =
        preview(vec![build(local.path(), "rg", X86_64, original.clone())])
    else {
        unreachable!()
    };
    bundle.bin_directory = "operator/bin".into();
    let replacement = || Operation::PreviewToolBundle {
        bundle: bundle.clone(),
    };
    let initial = guarded_job(&pane, None, replacement()).await.unwrap();
    guarded_job(&pane, None, apply(replacement(), &initial))
        .await
        .unwrap();

    let store = remote.join(".local/share/w11term/tools/portable");
    let before = initial["bundle_sha256"].as_str().unwrap();
    let selector = || fs::read_to_string(store.join("current")).unwrap();
    let old_selector = selector();
    let rg = store.join("versions").join(before).join("rg");
    let bin = remote.join("operator/bin");
    let old_launcher = fs::read_to_string(bin.join("rg")).unwrap();
    fs::remove_file(local.path().join("rg")).unwrap();

    let mut requested = bundle;
    requested.bin_directory = ".local/bin".into();
    requested.tools[0].builds[0].sha256 = "a".repeat(64);
    let mut jq = original.clone();
    jq[255] = 2;
    requested.tools.push(UtilityTool {
        name: "jq".into(),
        builds: vec![build(local.path(), "jq", X86_64, jq.clone())],
    });
    let add = |review: Option<&Value>| Operation::AddToToolBundle {
        bundle: requested.clone(),
        review: review.map(|preview| preview["review"].as_str().unwrap().to_owned()),
    };
    fs::write(bin.join("jq"), b"user-managed executable").unwrap();
    let blocked = guarded_job(&pane, None, add(None)).await.unwrap();
    assert_eq!(blocked["ready"], false);
    assert!(blocked["review"].is_null());
    assert_eq!(
        fs::read(bin.join("jq")).unwrap(),
        b"user-managed executable"
    );
    fs::remove_file(bin.join("jq")).unwrap();

    let addition = guarded_job(&pane, None, add(None)).await.unwrap();
    assert_eq!(addition["mode"], "add");
    assert_eq!(addition["ready"], true);
    assert_eq!(addition["bin_directory"], "/operator/bin");
    assert_eq!(addition["automatic_sync_paused"], true);
    assert_eq!(
        addition["tools"][1]["retained"]["sha256"],
        tool_sha256(&original)
    );
    assert_eq!(selector(), old_selector);
    assert!(!bin.join("jq").exists());

    fs::write(&rg, &jq).unwrap();
    let error = guarded_job(&pane, None, add(Some(&addition)))
        .await
        .unwrap_err();
    assert!(
        error.contains("Checking the tools that will stay installed"),
        "{error}"
    );
    assert_eq!(selector(), old_selector);
    assert!(!bin.join("jq").exists());
    fs::write(&rg, &original).unwrap();

    let installed = guarded_job(&pane, None, add(Some(&addition)))
        .await
        .unwrap();
    let after = installed["bundle_sha256"].as_str().unwrap();
    assert_eq!(installed["previous_bundle_sha256"], before);
    assert_eq!(installed["copied_tools"], json!(["rg"]));
    assert_eq!(installed["automatic_sync_paused"], true);
    assert!(selector().ends_with(&format!("./versions/{after}\n")));
    let extended = store.join("versions").join(after);
    assert_eq!(fs::read(extended.join("rg")).unwrap(), original);
    assert_eq!(fs::read(extended.join("jq")).unwrap(), jq);
    assert_eq!(fs::read(&rg).unwrap(), original);
    assert_eq!(fs::read_to_string(bin.join("rg")).unwrap(), old_launcher);
    assert!(
        fs::read_to_string(bin.join("jq"))
            .unwrap()
            .ends_with("/current/jq\n")
    );

    fs::remove_file(local.path().join("jq")).unwrap();
    let repeat = guarded_job(&pane, None, add(None)).await.unwrap();
    assert_eq!(repeat["bundle_sha256"], after);
    let unchanged = guarded_job(&pane, None, add(Some(&repeat))).await.unwrap();
    assert_eq!(unchanged["changed"], false);
    assert_eq!(unchanged["uploaded"], false);
    assert_eq!(unchanged["automatic_sync_paused"], true);

    let mut fresh = requested;
    fresh.name = "extra-tools".into();
    fresh.tools.remove(0);
    fs::write(local.path().join("jq"), &jq).unwrap();
    let fresh_add = |review| Operation::AddToToolBundle {
        bundle: fresh.clone(),
        review,
    };
    let first = guarded_job(&pane, None, fresh_add(None)).await.unwrap();
    assert_eq!(first["bin_directory"], "/.local/bin");
    assert!(first["current"]["version"].is_null());
    let token = first["review"].as_str().unwrap().to_owned();
    let wrong_mode = Operation::ApplyToolBundle {
        bundle: fresh.clone(),
        review: token.clone(),
    };
    let error = guarded_job(&pane, None, wrong_mode).await.unwrap_err();
    assert!(error.contains("review is stale"), "{error}");
    assert!(!remote.join(".local/bin/jq").exists());
    let added = guarded_job(&pane, None, fresh_add(Some(token)))
        .await
        .unwrap();
    assert_eq!(added["automatic_sync_paused"], true);
    assert!(
        fs::read_to_string(remote.join(".local/bin/jq"))
            .unwrap()
            .contains("extra-tools")
    );
    server.abort();
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
    let empty = guarded_job(&pane, None, Operation::InstalledTools)
        .await
        .unwrap();
    assert_eq!(empty, json!({"tools": [], "skipped_bundles": 0}));
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
        if byte == 2 {
            bundle.tools.push(UtilityTool {
                name: "team-report".into(),
                builds: vec![build(
                    local.path(),
                    "team-report",
                    UtilityArchitecture::X86_64,
                    executable(UtilityArchitecture::X86_64, None),
                )],
            });
        }
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
    fs::create_dir(store.join("versions/.w11term-stage-incomplete")).unwrap();
    let retained = guarded_job(&pane, None, Operation::ToolBundleVersions)
        .await
        .unwrap();
    let versions = retained["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["bundle"], "portable");
    assert_eq!(versions[0]["bundle_sha256"], b_id);
    assert_eq!(versions[0]["active"], true);
    assert_eq!(versions[0]["held"], false);
    assert_eq!(versions[1]["bundle_sha256"], a_id);
    assert_eq!(versions[1]["active"], false);
    assert_eq!(retained["skipped_bundles"], 0);
    assert_eq!(retained["omitted_versions"], 0);
    let current = guarded_job(&pane, None, Operation::InstalledTools)
        .await
        .unwrap();
    assert_eq!(
        current["tools"][2],
        json!({"name":"team-report", "bundle":"portable", "path":"/.local/bin/team-report"})
    );

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
    let retained = guarded_job(&pane, None, Operation::ToolBundleVersions)
        .await
        .unwrap();
    assert_eq!(retained["versions"][0]["bundle_sha256"], a_id);
    assert_eq!(retained["versions"][0]["active"], true);
    assert_eq!(retained["versions"][0]["held"], true);
    assert_eq!(retained["versions"][1]["bundle_sha256"], b_id);
    assert_eq!(retained["versions"][1]["active"], false);
    let current = guarded_job(&pane, None, Operation::InstalledTools)
        .await
        .unwrap();
    assert_eq!(current["tools"].as_array().unwrap().len(), 2);
    assert!(
        remote.join(".local/bin/team-report").exists(),
        "rollback leaves an inactive launcher"
    );
    fs::remove_file(remote.join(".local/bin/jq")).unwrap();
    let current = guarded_job(&pane, None, Operation::InstalledTools)
        .await
        .unwrap();
    assert_eq!(
        current["tools"],
        json!([{"name":"rg", "bundle":"portable", "path":"/.local/bin/rg"}])
    );
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

fn save_login_manifest(directory: &std::path::Path, variant: u8) {
    save_login_tools(directory, "fleet.json", variant, &["rg"]);
}

fn save_login_tools(directory: &std::path::Path, manifest: &str, variant: u8, tools: &[&str]) {
    let bundle = UtilityBundle {
        version: 1,
        name: "portable".into(),
        bin_directory: ".local/bin".into(),
        tools: tools
            .iter()
            .map(|name| {
                let mut binary = executable(UtilityArchitecture::X86_64, None);
                binary[255] = variant;
                let mut build = build(directory, name, UtilityArchitecture::X86_64, binary);
                build.path = (*name).into();
                UtilityTool {
                    name: (*name).into(),
                    builds: vec![build],
                }
            })
            .collect(),
    };
    fs::write(
        directory.join(manifest),
        serde_json::to_vec(&bundle).unwrap(),
    )
    .unwrap();
}

fn approved_login(source: Source, local: &std::path::Path) -> SshOptions {
    let Source::Ssh(mut options) = source else {
        unreachable!()
    };
    options.tool_bundle = Some(Box::new(ToolSync {
        bundle: "fleet.json".into(),
        name: "portable".into(),
        bin_directory: ".local/bin".into(),
        host: options.host.clone(),
        user: options.user.clone(),
        port: options.port,
        fingerprint: options.host_key.clone().unwrap(),
        home: "/".into(),
        add_to_path: Some(ToolPath {
            shell: DotfileShell::Bash,
            zdotdir: None,
        }),
    }));
    let mut restored: SshOptions =
        serde_json::from_value(serde_json::to_value(options).unwrap()).unwrap();
    restored
        .prepare_profile_connection(&local.join("profiles.json"))
        .unwrap();
    restored
}

#[cfg(feature = "ui-smoke")]
pub(super) async fn seed_catalog_tool_for_reference(
    pane: &Session,
    local: &std::path::Path,
    remote: &std::path::Path,
) -> String {
    probe_response(remote, "x86_64", "/", "/usr/bin");
    fs::write(remote.join("posix-rename-version"), "1").unwrap();
    save_login_tools(local, "catalog-seed.json", 1, &["rg"]);
    let mut bundle = UtilityBundle::load(&local.join("catalog-seed.json")).unwrap();
    bundle.name = w11term::sftp::builtin_tool_bundle("rg").unwrap().name;
    let operation = || Operation::PreviewToolBundle {
        bundle: bundle.clone(),
    };
    let plan = guarded_job(pane, None, operation()).await.unwrap();
    let installed = guarded_job(pane, None, apply(operation(), &plan))
        .await
        .unwrap();
    installed["bundle_sha256"].as_str().unwrap().into()
}

#[cfg(feature = "ui-smoke")]
pub(super) fn reference_tool_profile(
    source: Source,
    local: &std::path::Path,
    remote: &std::path::Path,
) -> SshOptions {
    probe_response(remote, "x86_64", "/", "/usr/bin:/.local/bin");
    fs::write(remote.join("posix-rename-version"), "1").unwrap();
    save_login_tools(local, "fleet.json", 1, &["rg", "mdd"]);
    save_login_tools(local, "Tool review.json", 1, &["rg", "mdd", "netwatch"]);
    let bin = remote.join(".local/bin");
    fs::create_dir_all(&bin).unwrap();
    let conflict = bin.join("netwatch");
    fs::write(&conflict, "existing fixture command").unwrap();
    fs::write(
        local.join("tool-launcher-conflict"),
        conflict.to_str().unwrap(),
    )
    .unwrap();
    let path_marker = tool_sha256(b".local/bin");
    fs::write(remote.join(".bashrc"), format!(
        "# preserve my shell changes\n# >>> w11term tool PATH {path_marker} >>>\n# edited managed block\n# <<< w11term tool PATH {path_marker} <<<\n# trailing user settings\n"
    )).unwrap();
    approved_login(source, local)
}

async fn wait_tool_login(pane: &Session, generation: u64, status: &str, path: Option<&str>) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let frame = pane.frames.load();
            if frame.connection_generation >= generation
                && frame
                    .rows
                    .iter()
                    .any(|row| row.as_str().contains("tools-at-shell:"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|e| {
        panic!(
            "Tool shell failed to start: {e}: {:?}",
            pane.events.since(0).events
        )
    });
    let batch = pane.events.since(0);
    let sync = batch
        .events
        .iter()
        .find(|event| event.kind == "tool-bundle-sync" && event.data["generation"] == generation)
        .unwrap();
    assert_eq!(sync.data["status"], status, "{sync:?}");
    assert_eq!(sync.data["path"].as_str(), path, "{sync:?}");
    let report = pane.events.tool_bundle_sync(generation).unwrap();
    assert_eq!(report.bundle, "portable");
    assert_eq!(serde_json::to_value(report.as_ref()).unwrap(), sync.data);
}

fn connect_tools(options: &SshOptions) -> Session {
    Session::start_sized(
        Source::Ssh(options.clone()),
        wezterm_term::TerminalSize {
            cols: 140,
            ..Default::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approved_tool_sync_installs_before_shell_and_respects_manual_changes() {
    let (source, remote, _, server) = fixture_connections(false, 8).await;
    let root = remote.path();
    let local = tempfile::tempdir().unwrap();
    probe_response(root, "x86_64", "/", "/usr/bin");
    fs::write(root.join("posix-rename-version"), "1").unwrap();
    fs::write(root.join("utility-shell-probe"), []).unwrap();
    fs::write(root.join(".bashrc"), "# existing user shell settings\n").unwrap();
    save_login_manifest(local.path(), 1);
    let mut options = approved_login(source, local.path());
    let dotfiles = local.path().join("dotfiles.json");
    fs::write(
        &dotfiles,
        br#"{"version":1,"environment":"export EDITOR=vi"}"#,
    )
    .unwrap();
    let scope = options.tool_bundle.as_ref().unwrap();
    options.dotfiles = Some(Box::new(w11term::options::DotfileSync {
        bundle: dotfiles,
        shell: DotfileShell::Bash,
        zdotdir: None,
        host: scope.host.clone(),
        user: scope.user.clone(),
        port: scope.port,
        fingerprint: scope.fingerprint.clone(),
        home: scope.home.clone(),
    }));
    if cfg!(feature = "ui-smoke") {
        options.player.reconnect = true;
        fs::write(root.join("keep-first-connection"), []).unwrap();
    }

    let pane = connect_tools(&options);
    wait_tool_login(&pane, 1, "updated", Some("updated")).await;
    let store = root.join(".local/share/w11term/tools/portable");
    let selector = || fs::read_to_string(store.join("current")).unwrap();
    let first = selector()
        .lines()
        .nth(1)
        .unwrap()
        .strip_prefix("versions/")
        .unwrap()
        .to_owned();
    assert!(pane.frames.load().rows.iter().any(|row| {
        row.as_str()
            .contains(&format!("tools-at-shell:{first}:true"))
    }));
    let rc = fs::read_to_string(root.join(".bashrc")).unwrap();
    assert!(rc.starts_with("# existing user shell settings\n"));
    assert_eq!(rc.matches("# >>> w11term dotfiles >>>").count(), 1);
    assert!(rc.contains("PATH=\"${PATH:+$PATH:}$HOME/.local/bin\""));
    assert_eq!(rc.matches("# >>> w11term tool PATH ").count(), 1);

    save_login_manifest(local.path(), 2);
    let updated = if cfg!(feature = "ui-smoke") {
        fs::remove_file(root.join("keep-first-connection")).unwrap();
        wait_tool_login(&pane, 2, "updated", Some("current")).await;
        assert!(pane.events.tool_bundle_sync(1).is_none());
        pane
    } else {
        let updated = connect_tools(&options);
        wait_tool_login(&updated, 1, "updated", Some("current")).await;
        updated
    };
    assert!(!selector().contains(&first));
    let second = selector()
        .lines()
        .nth(1)
        .unwrap()
        .strip_prefix("versions/")
        .unwrap()
        .to_owned();
    assert_eq!(fs::read_to_string(root.join(".bashrc")).unwrap(), rc);
    let review = guarded_job(&updated, None, rollback(&first, None))
        .await
        .unwrap();
    guarded_job(&updated, None, rollback(&first, Some(&review)))
        .await
        .unwrap();
    assert!(selector().contains(&format!("./versions/{first}")));

    fs::remove_file(local.path().join("fleet.json")).unwrap();
    let held = connect_tools(&options);
    wait_tool_login(&held, 1, "held", None).await;
    assert!(held.frames.load().rows.iter().any(|row| {
        row.as_str()
            .contains(&format!("tools-at-shell:{first}:true"))
    }));
    let resume = |review| Operation::ResumeToolBundleUpdates {
        name: "portable".into(),
        bundle_sha256: first.clone(),
        review,
    };
    let paused = selector();
    let report = guarded_job(&held, Some(1), resume(None)).await.unwrap();
    assert_eq!(report["ready"], true);
    assert_eq!(report["current"]["manual"], true);
    assert_eq!(selector(), paused);
    let token = || Some(report["review"].as_str().unwrap().to_owned());
    let changed = paused.replace(&first, &second);
    fs::write(store.join("current"), &changed).unwrap();
    let rejected = guarded_job(&held, Some(1), resume(token()))
        .await
        .unwrap_err();
    assert!(
        rejected.contains("no longer the active paused release"),
        "{rejected}"
    );
    assert_eq!(selector(), changed);
    fs::write(store.join("current"), paused).unwrap();
    let trace = root.join("transfer-trace");
    fs::write(&trace, []).unwrap();
    let resumed = guarded_job(&held, Some(1), resume(token())).await.unwrap();
    assert_eq!(resumed["changed"], true);
    assert_eq!(resumed["bundle_sha256"], first);
    assert_eq!(
        selector().lines().nth(1).unwrap(),
        format!("versions/{first}")
    );
    assert!(!sftp_opened_tool(&trace));

    save_login_manifest(local.path(), 2);
    let resumed = connect_tools(&options);
    wait_tool_login(&resumed, 1, "updated", Some("current")).await;
    assert_eq!(
        selector().lines().nth(1).unwrap(),
        format!("versions/{second}")
    );
    let mut extra = UtilityBundle::load(&local.path().join("fleet.json")).unwrap();
    extra.tools = vec![UtilityTool {
        name: "jq".into(),
        builds: vec![build(
            local.path(),
            "jq",
            UtilityArchitecture::X86_64,
            executable(UtilityArchitecture::X86_64, None),
        )],
    }];
    let add = |review| Operation::AddToToolBundle {
        bundle: extra.clone(),
        review,
    };
    let plan = guarded_job(&resumed, Some(1), add(None)).await.unwrap();
    let review = plan["review"].as_str().unwrap().to_owned();
    let added = guarded_job(&resumed, Some(1), add(Some(review)))
        .await
        .unwrap();
    let extended = added["bundle_sha256"].as_str().unwrap();
    let next = connect_tools(&options);
    wait_tool_login(&next, 1, "held", None).await;
    assert_eq!(
        selector().lines().nth(1).unwrap(),
        format!("./versions/{extended}")
    );
    assert!(store.join("versions").join(extended).join("jq").is_file());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_sync_skips_scope_changes_and_preserves_edited_startup_hooks() {
    let (source, remote, _, server) = fixture_connections(false, 5).await;
    let root = remote.path();
    let local = tempfile::tempdir().unwrap();
    probe_response(root, "x86_64", "/", "/usr/bin");
    fs::write(root.join("posix-rename-version"), "1").unwrap();
    fs::write(root.join("utility-shell-probe"), []).unwrap();
    save_login_manifest(local.path(), 1);
    let options = approved_login(source, local.path());
    let mut wrong = options.clone();
    wrong.tool_bundle.as_mut().unwrap().fingerprint = "SHA256:unapproved".into();
    let wrong = connect_tools(&wrong);
    wait_tool_login(&wrong, 1, "skipped", None).await;
    let repair = |review| Operation::ReviewToolPath { review };
    assert!(
        guarded_job(&wrong, Some(1), repair(None))
            .await
            .unwrap_err()
            .contains("no matching tool PATH approval")
    );
    assert!(
        !root.join("utility-probe-command").exists(),
        "scope mismatch must stop before probing"
    );

    let mut wrong = options.clone();
    wrong.tool_bundle.as_mut().unwrap().bin_directory = "other-bin".into();
    let wrong = connect_tools(&wrong);
    wait_tool_login(&wrong, 1, "skipped", None).await;
    assert!(!root.join(".local").exists());

    let pane = connect_tools(&options);
    wait_tool_login(&pane, 1, "updated", Some("updated")).await;
    let path = root.join(".bashrc");
    let original = fs::read_to_string(&path).unwrap();
    let changed = original.replace("export PATH", "export PATH # user edit");
    fs::write(&path, &changed).unwrap();
    fs::write(root.join("utility-digest-mode"), "valid").unwrap();
    let trace = root.join("transfer-trace");
    fs::write(&trace, []).unwrap();
    let pane = connect_tools(&options);
    wait_tool_login(&pane, 1, "current", Some("skipped")).await;
    assert!(
        !sftp_opened_tool(&trace),
        "repeat login should keep the bytes on the remote host"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);

    let review = guarded_job(&pane, Some(1), repair(None)).await.unwrap();
    assert_eq!(review["ready"], true);
    assert_eq!(review["files"][1]["action"], "replace-hook");
    assert!(
        review["files"][1]["replaced_hook"]
            .as_str()
            .unwrap()
            .contains("# user edit")
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
    let token = |report: &Value| Some(report["review"].as_str().unwrap().to_owned());
    let concurrent_edit = format!("# outside the managed block\n{changed}");
    fs::write(&path, &concurrent_edit).unwrap();
    let stale = guarded_job(&pane, Some(1), repair(token(&review)))
        .await
        .unwrap_err();
    assert!(stale.contains("review is stale"), "{stale}");
    assert_eq!(fs::read_to_string(&path).unwrap(), concurrent_edit);

    let duplicate = format!("{concurrent_edit}{changed}");
    fs::write(&path, &duplicate).unwrap();
    let blocked = guarded_job(&pane, Some(1), repair(None)).await.unwrap();
    assert_eq!(blocked["ready"], false);
    assert!(blocked.get("review").is_none());
    assert_eq!(blocked["files"][1]["action"], "hook-conflict");
    assert_eq!(fs::read_to_string(&path).unwrap(), duplicate);

    fs::write(&path, &concurrent_edit).unwrap();
    let review = guarded_job(&pane, Some(1), repair(None)).await.unwrap();
    let applied = guarded_job(&pane, Some(1), repair(token(&review)))
        .await
        .unwrap();
    assert_eq!(applied["changed"], true);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        format!("# outside the managed block\n{original}")
    );
    let current = guarded_job(&pane, Some(1), repair(None)).await.unwrap();
    assert_eq!(current["ready"], false);
    assert!(
        current["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["action"] == "unchanged")
    );

    let zdotdir = root.join("config/zsh");
    fs::create_dir_all(&zdotdir).unwrap();
    fs::write(zdotdir.join(".zshrc"), "# existing Zsh settings\n").unwrap();
    let mut zsh = options;
    zsh.tool_bundle.as_mut().unwrap().add_to_path = Some(ToolPath {
        shell: DotfileShell::Zsh,
        zdotdir: Some("config/zsh".into()),
    });
    let pane = connect_tools(&zsh);
    wait_tool_login(&pane, 1, "current", Some("updated")).await;
    let zshrc = fs::read_to_string(zdotdir.join(".zshrc")).unwrap();
    assert!(zshrc.starts_with("# existing Zsh settings\n"));
    assert!(zshrc.contains("$HOME/.local/bin"));
    assert!(zdotdir.join(".zprofile").is_file());
    assert!(!root.join(".zshrc").exists());
    server.abort();
}
