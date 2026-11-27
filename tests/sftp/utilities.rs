use super::*;
use sha2::{Digest, Sha256};
use w11term::sftp::{Operation, UtilityArchitecture, UtilityBuild, UtilityBundle, UtilityTool};

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
