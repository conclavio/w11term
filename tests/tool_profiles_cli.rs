use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};
use w11term::sftp::UtilityArchitecture;

#[path = "support/tool_elf.rs"]
mod tool_elf;

const VERB: &str = "audit-tool-profiles";
const KEY: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn audit(root: &Path, args: &[&str]) -> std::process::Output {
    cli(root, VERB, args)
}

fn cli(root: &Path, verb: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(root)
        .env("W11TERM_DATA_DIR", root)
        .arg(verb)
        .args(args)
        .output()
        .unwrap()
}

fn rollout(root: &Path, args: &[&str]) -> std::process::Output {
    cli(root, "rollout-tool-profiles", args)
}

#[test]
fn rolls_back_a_saved_tool_receipt_after_review_and_preserves_other_fields() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let old = fleet_manifest(&[("rg", 'a')]);
    let next = fleet_manifest(&[("rg", 'b')]);
    let previous_pin = sha256(&old).to_uppercase();
    let old_path = root.join("previous.json");
    let next_path = root.join("next.json");
    fs::write(&old_path, &old).unwrap();
    fs::write(&next_path, &next).unwrap();
    fs::write(
        root.join("wrong-scope.json"),
        old.replace("\"fleet\"", "\"other\""),
    )
    .unwrap();
    let catalog = root.join("fleet profiles.json");
    let unrelated = root.join("profiles.json");
    fs::write(&unrelated, br#"{"profiles":{}}"#).unwrap();
    let original = json!({"profiles": {
        "East": profile("previous.json", Some(&previous_pin)),
        "West": profile("missing.json", None),
        "Scope": profile("wrong-scope.json", None),
        "Unchecked": profile("previous.json", Some(&previous_pin)),
    }});
    fs::write(&catalog, original.to_string()).unwrap();
    let mut args = vec![
        "--profiles",
        "fleet profiles.json",
        "--candidate",
        "next.json",
        "--profile",
        "East",
        "--profile",
        "West",
        "--profile",
        "Scope",
    ];
    let receipt_name = "Tool rollout.json";
    let receipt = root.join(receipt_name);
    let undo = |args: &[&str]| cli(root, "rollback-tool-profiles", args);
    let reject = |args: &[&str], expected: &str| {
        let failed = undo(args);
        assert_eq!(failed.status.code(), Some(1), "{failed:?}");
        assert!(failed.stdout.is_empty());
        let reply: Value = serde_json::from_slice(&failed.stderr).unwrap();
        assert!(
            reply["error"].as_str().unwrap().contains(expected),
            "{reply}"
        );
    };
    let preview = rollout(root, &args);
    assert!(preview.status.success(), "{preview:?}");
    fs::write(&receipt, &preview.stdout).unwrap();
    reject(&[receipt_name], "applied SSH tool rollout");
    let preview = reply(preview);
    args.extend(["--review", preview["review"].as_str().unwrap()]);
    let applied = rollout(root, &args);
    assert!(applied.status.success(), "{applied:?}");
    let mut formatted = b"\xef\xbb\xbf".to_vec();
    formatted.extend(&applied.stdout);
    let rolled_out = fs::read_to_string(&catalog).unwrap();

    let mut altered_receipt: Value = serde_json::from_slice(&applied.stdout).unwrap();
    let entry = altered_receipt["result"]["profiles"][0].clone();
    altered_receipt["result"]["profiles"]
        .as_array_mut()
        .unwrap()
        .push(entry);
    fs::write(&receipt, altered_receipt.to_string()).unwrap();
    reject(&[receipt_name], "appears more than once");
    let mut altered_receipt: Value = serde_json::from_slice(&applied.stdout).unwrap();
    altered_receipt["result"]["profiles"][0]["next_pin"] = Value::Null;
    fs::write(&receipt, altered_receipt.to_string()).unwrap();
    reject(&[receipt_name], "pin policy or candidate pin");
    fs::write(&receipt, formatted).unwrap();

    let review = reply(undo(&[receipt_name]));
    assert_eq!(review["applied"], false);
    assert_eq!(review["profiles_path"], json!(catalog));
    assert_eq!(review["changed_profiles"], 3);
    let rows = review["profiles"].as_array().unwrap();
    let row = |name: &str| rows.iter().find(|row| row["profile"] == name).unwrap();
    assert_eq!(row("East")["restore_status"], "matches-pin");
    assert_eq!(row("East")["current_manifest_sha256"], sha256(&next));
    assert_eq!(row("East")["restored_manifest_sha256"], sha256(&old));
    assert_eq!(row("West")["restore_status"], "manifest-unavailable");
    assert_eq!(row("Scope")["restore_status"], "scope-mismatch");
    assert_eq!(fs::read_to_string(&catalog).unwrap(), rolled_out);

    let apply = [receipt_name, "--review", review["review"].as_str().unwrap()];
    fs::write(&old_path, format!("{old}\n")).unwrap();
    reject(&apply, "Preview the rollback again");
    fs::write(&old_path, &old).unwrap();
    fs::write(&next_path, format!("{next}\n")).unwrap();
    reject(&apply, "Preview the rollback again");
    fs::write(&next_path, &next).unwrap();
    let mut edited: Value = serde_json::from_str(&rolled_out).unwrap();
    edited["profiles"]["West"]["tool_bundle"]["add_to_path"] = json!({"shell": "fish"});
    let edited = edited.to_string();
    fs::write(&catalog, &edited).unwrap();
    reject(&apply, "no longer matches the applied rollout");
    assert_eq!(fs::read_to_string(&catalog).unwrap(), edited);
    fs::write(&catalog, &rolled_out).unwrap();

    assert_eq!(reply(undo(&apply))["applied"], true);
    let mut expected = original;
    for (name, path) in [
        ("East", "previous.json"),
        ("West", "missing.json"),
        ("Scope", "wrong-scope.json"),
    ] {
        expected["profiles"][name]["tool_bundle"]["bundle"] = json!(root.join(path));
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&catalog).unwrap()).unwrap(),
        expected
    );
    assert_eq!(fs::read(&unrelated).unwrap(), br#"{"profiles":{}}"#);
    reject(&apply, "no longer matches the applied rollout");
}

fn reply(output: std::process::Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["ok"], true);
    body["result"].clone()
}

fn manifest() -> String {
    json!({
        "version": 1, "name": "fleet",
        "tools": [{"name": "rg", "builds": [{
            "architecture": "x86_64", "path": "local-build", "sha256": "a".repeat(64),
        }]}],
    })
    .to_string()
}

fn sha256(bytes: impl AsRef<[u8]>) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn verifies_local_and_cached_executables_per_architecture_without_fetching() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let x86 = tool_elf::executable(
        UtilityArchitecture::X86_64,
        Some("/lib64/ld-linux-x86-64.so.2"),
    );
    let arm = tool_elf::executable(UtilityArchitecture::Aarch64, None);
    let x86_pin = sha256(&x86);
    let arm_pin = sha256(&arm);
    fs::write(root.join("x86-build"), &x86).unwrap();
    fs::write(root.join("arm-build"), &arm).unwrap();
    fs::write(root.join("damaged"), b"damaged").unwrap();
    let cache = root.join("tools").join("cache").join("v1");
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join(&arm_pin), &arm).unwrap();
    let missing_pin = "d".repeat(64);
    let damaged_pin = "e".repeat(64);
    fs::write(cache.join(&damaged_pin), b"damaged cache").unwrap();
    let download = json!({"format": "raw", "url": "https://example.test/tool"});
    let local =
        |path: &str, pin: &str| json!({"architecture": "x86_64", "path": path, "sha256": pin});
    let file = "checked-tools.json";
    let manifest = json!({
        "version": 1, "name": "fleet", "tools": [
            {"name": "rg", "builds": [
                local("missing-local", &x86_pin), local("damaged", &x86_pin), local("x86-build", &x86_pin),
                {"architecture": "aarch64", "sha256": arm_pin, "download": download},
            ]},
            {"name": "jq", "builds": [
                {"architecture": "x86_64", "sha256": missing_pin, "download": download},
                {"architecture": "x86_64", "sha256": damaged_pin, "download": download},
                local("arm-build", &arm_pin), local("x86-build", &x86_pin),
            ]},
        ],
    })
    .to_string();
    fs::write(root.join(file), &manifest).unwrap();
    let catalog = json!({"profiles": {
        "Approved": profile(file, Some(&sha256(&manifest))),
        "Follow": profile(file, None),
        "Stale": profile(file, Some(&"0".repeat(64))),
    }})
    .to_string();
    let catalog_path = root.join("profiles.json");
    fs::write(&catalog_path, &catalog).unwrap();

    let normal = reply(audit(root, &[]));
    assert!(normal["profiles"][0].get("build_checks").is_none());
    let audit_result = reply(audit(root, &["--verify-builds"]));
    let rows = audit_result["profiles"].as_array().unwrap();
    let checks = &rows[0]["build_checks"];
    assert_eq!(rows[0]["status"], "matches-pin");
    assert_eq!(rows[1]["build_checks"], *checks);
    assert_eq!(rows[2]["status"], "pin-mismatch");
    assert!(rows[2].get("build_checks").is_none());

    let checked_x86 = &checks[0];
    assert_eq!(checked_x86["architecture"], "x86_64");
    assert_eq!(checked_x86["locally_available"], true);
    let rg = &checked_x86["commands"][0]["builds"];
    assert_eq!(rg[0]["status"], "missing");
    assert_eq!(rg[0]["source"], "local-file");
    assert_eq!(rg[1]["status"], "error");
    assert!(rg[1]["detail"].as_str().unwrap().contains("SHA-256"));
    assert_eq!(rg[2]["status"], "verified");
    assert_eq!(rg[2]["interpreter"], "/lib64/ld-linux-x86-64.so.2");
    let jq = &checked_x86["commands"][1]["builds"];
    assert_eq!(jq[0]["status"], "missing");
    assert_eq!(jq[0]["source"], "download-cache");
    assert_eq!(jq[1]["status"], "error");
    assert!(jq[1]["detail"].as_str().unwrap().contains("SHA-256"));
    assert_eq!(jq[2]["status"], "error");
    assert!(jq[2]["detail"].as_str().unwrap().contains("architecture"));
    assert_eq!(jq[3]["status"], "verified");

    let checked_arm = &checks[1];
    assert_eq!(checked_arm["architecture"], "aarch64");
    assert_eq!(checked_arm["locally_available"], false);
    let cached = &checked_arm["commands"][0]["builds"][0];
    assert_eq!(cached["status"], "verified");
    assert_eq!(cached["source"], "download-cache");
    assert_eq!(cached["path"], json!(cache.join(&arm_pin)));
    assert_eq!(checked_arm["commands"][1]["locally_available"], false);
    assert_eq!(checked_arm["commands"][1]["builds"], json!([]));
    let selected = reply(audit(
        root,
        &[
            "--profile",
            "Approved",
            "--verify-builds",
            "--architecture",
            "aarch64",
        ],
    ));
    assert_eq!(
        selected["profiles"][0]["build_checks"],
        json!([checked_arm])
    );
    assert_eq!(fs::read_to_string(catalog_path).unwrap(), catalog);
    assert_eq!(fs::read_to_string(root.join(file)).unwrap(), manifest);
    assert_eq!(fs::read(cache.join(arm_pin)).unwrap(), arm);
    assert_eq!(fs::read(cache.join(damaged_pin)).unwrap(), b"damaged cache");
    assert_eq!(fs::read_dir(&cache).unwrap().count(), 2);
    assert_eq!(fs::read(root.join("damaged")).unwrap(), b"damaged");
    for args in [
        &["--architecture", "aarch64"][..],
        &["--verify-builds", "--architecture", "invalid"],
        &["--verify-builds", "--verify-builds"],
    ] {
        assert_eq!(audit(root, args).status.code(), Some(1));
    }
}

fn profile(bundle: &str, pin: Option<&str>) -> Value {
    json!({
        "host": "linux.example.test", "user": "dev", "port": 2022,
        "auth": "identity", "identity": "private-ssh-key", "host_key": KEY,
        "tool_bundle": {
            "bundle": bundle, "name": "fleet", "bin_directory": ".local/bin",
            "host": "linux.example.test", "user": "dev", "port": 2022,
            "fingerprint": KEY, "home": "/home/dev",
            "manifest_sha256": pin,
        },
    })
}

#[test]
fn prepares_selected_profile_downloads_and_reports_partial_failures() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let cache = root.join("tools/cache/v1");
    fs::create_dir_all(&cache).unwrap();
    let arm = tool_elf::executable(UtilityArchitecture::Aarch64, None);
    let arm_pin = sha256(&arm);
    fs::write(cache.join(&arm_pin), &arm).unwrap();
    let download = json!({"format": "raw", "url": "https://tools.example.test/rg"});
    let tools = json!({"version": 1, "name": "fleet", "tools": [{"name": "rg", "builds": [
        {"architecture": "aarch64", "sha256": arm_pin, "download": download},
        {"architecture": "x86_64", "sha256": "f".repeat(64), "download": download},
    ]}]})
    .to_string();
    let local = fleet_manifest(&[("rg", 'a')]);
    fs::write(root.join("release.json"), &tools).unwrap();
    fs::write(root.join("local.json"), &local).unwrap();
    let invalid = tools.replace("\"fleet\"", "\"wrong-bundle\"");
    fs::write(root.join("wrong.json"), &invalid).unwrap();
    let pin = sha256(&tools);
    let catalog_path = root.join("fleet profiles.json");
    let catalog = json!({"profiles": {
        "Pinned": profile("release.json", Some(&pin)),
        "Follow file": profile("release.json", None),
        "Stale": profile("release.json", Some(&"0".repeat(64))),
        "Wrong scope": profile("wrong.json", Some(&sha256(&invalid))),
        "Local only": profile("local.json", Some(&sha256(&local))),
        "Unselected": profile("missing-manifest.json", None),
    }})
    .to_string();
    fs::write(&catalog_path, &catalog).unwrap();
    let prepare = |args: &[&str]| {
        let mut scoped = vec![
            "--profiles",
            "fleet profiles.json",
            "--architecture",
            "aarch64",
        ];
        scoped.extend_from_slice(args);
        cli(root, "prepare-tool-profiles", &scoped)
    };
    let prepared = reply(prepare(&["--profile", "Pinned", "--profile", "Local only"]));
    assert_eq!(prepared["failed_profiles"], 0);
    assert_eq!(prepared["architecture"], "aarch64");
    assert_eq!(prepared["profiles_path"], json!(catalog_path));
    assert_eq!(
        prepared["profiles"],
        json!([
            {"profile": "Pinned", "status": "prepared", "manifest_sha256": pin, "prepared_builds": 1},
            {"profile": "Local only", "status": "no-downloads", "manifest_sha256": sha256(&local)},
        ])
    );

    let partial = prepare(&[
        "--profile",
        "Stale",
        "--profile",
        "Follow file",
        "--profile",
        "Wrong scope",
        "--profile",
        "Absent",
    ]);
    assert_eq!(partial.status.code(), Some(1), "{partial:?}");
    assert!(partial.stderr.is_empty(), "{partial:?}");
    let response: Value = serde_json::from_slice(&partial.stdout).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["failed_profiles"], 3);
    let rows = response["result"]["profiles"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    for (index, name, detail) in [
        (0, "Stale", "differs from its saved pin"),
        (2, "Wrong scope", "manifest name or bin directory changed"),
        (3, "Absent", "SSH profile Absent was not found"),
    ] {
        assert_eq!(rows[index]["profile"], name);
        assert_eq!(rows[index]["status"], "failed");
        assert!(
            rows[index]["error"].as_str().unwrap().contains(detail),
            "{}",
            rows[index]
        );
    }
    assert_eq!(rows[1]["profile"], "Follow file");
    assert_eq!(rows[1]["status"], "prepared");
    assert_eq!(rows[1]["prepared_builds"], 1);

    let local_only = reply(cli(
        root,
        "prepare-tool-profiles",
        &[
            "--profiles",
            "fleet profiles.json",
            "--profile",
            "Local only",
        ],
    ));
    assert!(local_only["architecture"].is_null());
    assert_eq!(local_only["profiles"][0]["status"], "no-downloads");
    for (args, detail) in [
        (&[][..], "at least one SSH profile"),
        (
            &["--profile", "Pinned", "--profile", "Pinned"][..],
            "each SSH profile only once",
        ),
    ] {
        let invalid = prepare(args);
        assert_eq!(invalid.status.code(), Some(1));
        assert!(invalid.stdout.is_empty());
        let response: Value = serde_json::from_slice(&invalid.stderr).unwrap();
        assert!(
            response["error"].as_str().unwrap().contains(detail),
            "{response}"
        );
    }
    assert_eq!(fs::read_to_string(catalog_path).unwrap(), catalog);
    assert_eq!(
        fs::read_to_string(root.join("release.json")).unwrap(),
        tools
    );
    assert_eq!(fs::read(cache.join(arm_pin)).unwrap(), arm);
    assert_eq!(fs::read_dir(cache).unwrap().count(), 1);
}

fn fleet_manifest(commands: &[(&str, char)]) -> String {
    let tools: Vec<_> = commands
        .iter()
        .map(|(name, pin)| {
            json!({"name": name, "builds": [{
                "architecture": "x86_64", "path": name, "sha256": pin.to_string().repeat(64),
            }]})
        })
        .collect();
    json!({"version": 1, "name": "fleet", "tools": tools}).to_string()
}

#[test]
fn reviewed_tool_rollout_updates_only_the_selected_manifest_bindings() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = fleet_manifest(&[("rg", 'a'), ("ncdu", 'c'), ("dust", 'e')]);
    let candidate = fleet_manifest(&[("rg", 'b'), ("jq", 'd'), ("dust", 'e')]);
    let before_pin = sha256(&before);
    let next_pin = sha256(&candidate);
    fs::write(root.join("current.json"), &before).unwrap();
    fs::write(root.join("new tools.json"), &candidate).unwrap();
    let mut east = profile("current.json", Some(&before_pin));
    east["tool_bundle"]["add_to_path"] =
        json!({"shell": "fish", "fish_config_dir": "settings/fish"});
    east["future_field"] = json!({"unrelated": true});
    let mut west = profile("current.json", None);
    west["tool_bundle"]
        .as_object_mut()
        .unwrap()
        .remove("manifest_sha256");
    let original = json!({"profiles": {
        "East": east, "West": west,
        "Missing": profile("missing.json", Some(&before_pin)),
        "Current": profile("new tools.json", Some(&next_pin.to_uppercase())),
        "In place": profile("new tools.json", Some(&before_pin)),
        "Untouched": profile("current.json", Some(&before_pin)),
    }});
    let catalog = root.join("profiles.json");
    let original_text = original.to_string();
    fs::write(&catalog, &original_text).unwrap();
    let mut args = vec![
        "--candidate",
        "new tools.json",
        "--profile",
        "West",
        "--profile",
        "East",
        "--profile",
        "Missing",
        "--profile",
        "Current",
        "--profile",
        "In place",
    ];
    let preview = reply(rollout(root, &args));
    assert!(preview.get("candidate_builds").is_none());
    assert_eq!(preview["applied"], false);
    assert_eq!(preview["changed_profiles"], 4);
    assert_eq!(preview["candidate_manifest_sha256"], next_pin);
    assert_eq!(preview["candidate_commands"], json!(["rg", "jq", "dust"]));
    let rows = preview["profiles"].as_array().unwrap();
    let row = |name: &str| rows.iter().find(|row| row["profile"] == name).unwrap();
    assert_eq!(row("East")["baseline"]["kind"], "pinned");
    assert_eq!(row("West")["baseline"]["kind"], "follows-file");
    assert_eq!(row("Missing")["baseline"]["kind"], "unavailable");
    assert!(row("Missing")["changes"].is_null());
    assert_eq!(row("In place")["baseline"]["kind"], "pin-mismatch");
    assert_eq!(row("In place")["baseline"]["manifest_sha256"], next_pin);
    assert_eq!(row("Current")["changed"], false);
    assert_eq!(
        row("East")["changes"],
        json!({"added": ["jq"], "removed": ["ncdu"], "changed": ["rg"]})
    );
    assert_eq!(row("East")["next_pin"], next_pin);
    assert!(row("West")["next_pin"].is_null());
    assert_eq!(fs::read_to_string(&catalog).unwrap(), original_text);

    args.extend(["--review", preview["review"].as_str().unwrap()]);
    let applied = reply(rollout(root, &args));
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["changed_profiles"], 4);
    let mut expected = original;
    for name in ["East", "West", "Missing", "In place"] {
        expected["profiles"][name]["tool_bundle"]["bundle"] = json!(root.join("new tools.json"));
    }
    for name in ["East", "Missing", "In place"] {
        expected["profiles"][name]["tool_bundle"]["manifest_sha256"] = json!(next_pin);
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(catalog).unwrap()).unwrap(),
        expected
    );
    assert_eq!(
        fs::read_to_string(root.join("current.json")).unwrap(),
        before
    );
    assert_eq!(
        fs::read_to_string(root.join("new tools.json")).unwrap(),
        candidate
    );
}

#[test]
fn reviewed_rollout_checks_candidate_builds_and_binds_the_apply_gate_to_the_token() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = fleet_manifest(&[("rg", 'a')]);
    let x86 = tool_elf::executable(UtilityArchitecture::X86_64, None);
    let executable = root.join("release-rg");
    fs::write(&executable, &x86).unwrap();
    let candidate = json!({"version": 1, "name": "fleet", "tools": [{"name": "rg", "builds": [
        {"architecture": "x86_64", "path": "missing-rg", "sha256": "b".repeat(64)},
        {"architecture": "x86_64", "path": "release-rg", "sha256": sha256(&x86)},
        {"architecture": "aarch64", "sha256": "d".repeat(64),
         "download": {"format": "raw", "url": "https://tools.example.test/rg"}},
    ]}]})
    .to_string();
    fs::write(root.join("current.json"), &before).unwrap();
    fs::write(root.join("next.json"), &candidate).unwrap();
    let catalog = root.join("profiles.json");
    let original = json!({"profiles": {
        "East": profile("current.json", Some(&sha256(&before))),
        "West": profile("current.json", None),
    }});
    let original_text = original.to_string();
    fs::write(&catalog, &original_text).unwrap();
    let run = |profile: &str, flags: &[&str]| {
        let mut args = vec!["--candidate", "next.json", "--profile", profile];
        args.extend_from_slice(flags);
        rollout(root, &args)
    };
    let reject = |flags: &[&str], expected: &str| {
        let failed = run("East", flags);
        assert_eq!(failed.status.code(), Some(1), "{failed:?}");
        assert!(failed.stdout.is_empty());
        let response: Value = serde_json::from_slice(&failed.stderr).unwrap();
        assert!(
            response["error"].as_str().unwrap().contains(expected),
            "{response}"
        );
        assert_eq!(fs::read_to_string(&catalog).unwrap(), original_text);
    };

    let all = reply(run("East", &["--require-ready"]));
    let builds = &all["candidate_builds"];
    assert_eq!(builds["require_ready"], true);
    assert!(builds["architecture"].is_null());
    assert_eq!(builds["ready"], false);
    assert_eq!(builds["checks"][0]["locally_available"], true);
    assert_eq!(builds["checks"][1]["locally_available"], false);
    assert_eq!(
        builds["checks"][1]["commands"][0]["builds"][0]["status"],
        "missing"
    );
    reject(
        &[
            "--require-ready",
            "--review",
            all["review"].as_str().unwrap(),
        ],
        "Candidate builds are missing or invalid for rg (Aarch64)",
    );
    let scoped = reply(run(
        "East",
        &["--require-ready", "--architecture", "x86_64"],
    ));
    let token = scoped["review"].as_str().unwrap();
    assert_eq!(scoped["candidate_builds"]["ready"], true);
    assert_eq!(scoped["candidate_builds"]["architecture"], "x86_64");
    assert_eq!(
        scoped["candidate_builds"]["checks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    reject(&["--review", token], "Preview the rollout again");
    reject(
        &[
            "--require-ready",
            "--architecture",
            "aarch64",
            "--review",
            token,
        ],
        "build policy changed",
    );
    reject(
        &[
            "--verify-builds",
            "--architecture",
            "x86_64",
            "--review",
            token,
        ],
        "build policy changed",
    );
    reject(
        &["--architecture", "x86_64"],
        "requires --verify-builds or --require-ready",
    );
    reject(
        &["--verify-builds", "--require-ready"],
        "Choose either --verify-builds or --require-ready once",
    );

    let apply = [
        "--require-ready",
        "--architecture",
        "x86_64",
        "--review",
        token,
    ];
    fs::write(&executable, b"changed after preview").unwrap();
    reject(
        &apply,
        "Candidate builds are missing or invalid for rg (X86_64)",
    );
    fs::write(&executable, &x86).unwrap();
    let applied = run("East", &apply);
    let receipt = "verified rollout.json";
    fs::write(root.join(receipt), &applied.stdout).unwrap();
    let applied = reply(applied);
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["candidate_builds"]["ready"], true);
    let rollback = reply(cli(root, "rollback-tool-profiles", &[receipt]));
    assert_eq!(rollback["profiles"][0]["profile"], "East");
    assert_eq!(rollback["profiles"][0]["restore_status"], "matches-pin");

    let verify = reply(run("West", &["--verify-builds"]));
    assert_eq!(verify["candidate_builds"]["ready"], false);
    let applied = reply(run(
        "West",
        &[
            "--verify-builds",
            "--review",
            verify["review"].as_str().unwrap(),
        ],
    ));
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["candidate_builds"]["ready"], false);
    assert_eq!(applied["candidate_builds"]["require_ready"], false);
    let saved: Value = serde_json::from_slice(&fs::read(catalog).unwrap()).unwrap();
    for name in ["East", "West"] {
        assert_eq!(
            saved["profiles"][name]["tool_bundle"]["bundle"],
            json!(root.join("next.json"))
        );
    }
    assert_eq!(
        saved["profiles"]["East"]["tool_bundle"]["manifest_sha256"],
        sha256(&candidate)
    );
    assert!(saved["profiles"]["West"]["tool_bundle"]["manifest_sha256"].is_null());
    assert_eq!(
        fs::read_to_string(root.join("next.json")).unwrap(),
        candidate
    );
}

#[test]
fn tool_rollout_rejects_stale_reviews_and_requires_the_existing_destination() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = fleet_manifest(&[("rg", 'a')]);
    let revision = fleet_manifest(&[("rg", 'b')]);
    fs::write(root.join("current.json"), &before).unwrap();
    fs::write(root.join("next.json"), &revision).unwrap();
    fs::write(
        root.join("wrong.json"),
        revision.replace("\"fleet\"", "\"other\""),
    )
    .unwrap();
    let mut durable = profile("current.json", None);
    durable["durable"] = json!(true);
    let mut unapproved = profile("current.json", None);
    unapproved.as_object_mut().unwrap().remove("tool_bundle");
    let original = json!({"profiles": {
        "East": profile("current.json", Some(&sha256(&before))), "West": profile("current.json", None),
        "Durable": durable, "Unapproved": unapproved,
    }});
    let catalog = root.join("profiles.json");
    let original_text = original.to_string();
    fs::write(&catalog, &original_text).unwrap();
    let args = [
        "--candidate",
        "next.json",
        "--profile",
        "West",
        "--profile",
        "East",
    ];
    let review = reply(rollout(root, &args));
    let mut apply = args.to_vec();
    apply.extend(["--review", review["review"].as_str().unwrap()]);
    let reject = |args: &[&str], expected: &str| {
        let failed = rollout(root, args);
        assert_eq!(failed.status.code(), Some(1), "{failed:?}");
        let reply: Value = serde_json::from_slice(&failed.stderr).unwrap();
        assert!(
            reply["error"].as_str().unwrap().contains(expected),
            "{reply}"
        );
        assert!(failed.stdout.is_empty());
    };
    fs::write(root.join("next.json"), format!("{revision}\n")).unwrap();
    reject(&apply, "Preview the rollout again");
    fs::write(root.join("next.json"), &revision).unwrap();
    fs::write(root.join("current.json"), format!("{before}\n")).unwrap();
    reject(&apply, "Preview the rollout again");
    fs::write(root.join("current.json"), &before).unwrap();
    let mut altered = original.clone();
    altered["profiles"]["West"]["tool_bundle"]["home"] = json!("/home/new");
    let altered = altered.to_string();
    fs::write(&catalog, &altered).unwrap();
    reject(&apply, "Preview the rollout again");
    assert_eq!(fs::read_to_string(&catalog).unwrap(), altered);
    fs::write(&catalog, &original_text).unwrap();
    reject(
        &["--candidate", "wrong.json", "--profile", "East"],
        "approved destination",
    );
    reject(
        &["--candidate", "next.json", "--profile", "Durable"],
        "own shell startup",
    );
    reject(
        &["--candidate", "next.json", "--profile", "Unapproved"],
        "no SSH tool approval",
    );
    reject(
        &[
            "--candidate",
            "next.json",
            "--profile",
            "East",
            "--profile",
            "East",
        ],
        "only once",
    );
    assert_eq!(fs::read_to_string(&catalog).unwrap(), original_text);

    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| rollout(root, &apply));
        let second = scope.spawn(|| rollout(root, &apply));
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_ne!(first.status.success(), second.status.success());
    let refused = if first.status.success() {
        second
    } else {
        first
    };
    let failure: Value = serde_json::from_slice(&refused.stderr).unwrap();
    let message = failure["error"].as_str().unwrap();
    assert!(
        message.contains("Another window is updating profiles")
            || message.contains("Preview the rollout again"),
        "{failure}"
    );
    reject(&apply, "Preview the rollout again");
    let final_catalog: Value = serde_json::from_slice(&fs::read(catalog).unwrap()).unwrap();
    for name in ["East", "West"] {
        assert_eq!(
            final_catalog["profiles"][name]["tool_bundle"]["bundle"],
            json!(root.join("next.json"))
        );
    }
}

#[test]
fn audits_shared_manifests_offline_without_touching_the_catalog() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let catalog = root.join("profile data");
    fs::create_dir(&catalog).unwrap();
    let manifest = manifest();
    let actual = sha256(&manifest);
    let uppercase = actual.to_uppercase();
    let old = sha256(manifest.replace("local-build", "old-build"));
    let file = "Fleet tools.json";
    fs::write(catalog.join(file), &manifest).unwrap();
    fs::write(catalog.join("invalid.json"), b"invalid JSON").unwrap();
    let mut wrong_destination = profile(file, None);
    wrong_destination["tool_bundle"]["bin_directory"] = json!("different/bin");
    let profiles = json!({"profiles": {
        "Matches": profile(file, Some(&uppercase)),
        "Changed": profile(file, Some(&old)),
        "Follows": profile(file, None),
        "Missing": profile("missing.json", None),
        "Invalid manifest": profile("invalid.json", None),
        "Different destination": wrong_destination,
        "Local": {"backend": "local", "executable": "powershell.exe"},
    }})
    .to_string();
    let path = "profile data/profiles.json";
    fs::write(root.join(path), &profiles).unwrap();

    let result = reply(audit(root, &["--profiles", path]));
    let rows = result["profiles"].as_array().unwrap();
    assert_eq!(rows.len(), 6);
    let row = |name: &str| rows.iter().find(|row| row["profile"] == name).unwrap();
    for (name, status) in [
        ("Matches", "matches-pin"),
        ("Changed", "pin-mismatch"),
        ("Follows", "follows-file"),
        ("Missing", "manifest-error"),
        ("Invalid manifest", "manifest-error"),
        ("Different destination", "manifest-scope-mismatch"),
    ] {
        assert_eq!(row(name)["status"], status, "{name}");
    }
    for name in ["Matches", "Changed", "Follows", "Different destination"] {
        assert_eq!(row(name)["manifest_sha256"], actual);
    }
    assert_eq!(row("Changed")["pinned_manifest_sha256"], old);
    assert_eq!(row("Matches")["bundle"], json!(catalog.join(file)));
    assert_eq!(row("Matches")["bundle_name"], "fleet");
    assert!(row("Missing")["manifest_sha256"].is_null());
    assert_eq!(fs::read_to_string(root.join(path)).unwrap(), profiles);
    assert_eq!(fs::read_to_string(catalog.join(file)).unwrap(), manifest);
    let json = result.to_string();
    assert!(!json.contains("private-ssh-key") && !json.contains("local-build"));
}

#[test]
fn audits_local_approval_settings_and_selects_a_saved_ssh_profile() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let file = "fleet.json";
    fs::write(root.join(file), manifest()).unwrap();
    let mut disabled = profile(file, None);
    disabled.as_object_mut().unwrap().remove("tool_bundle");
    let mut durable = profile("missing.json", None);
    durable["durable"] = json!(true);
    let mut game = profile("missing.json", None);
    game["player"] = json!({"rustmush": true});
    let mut wrong_account = profile(file, None);
    wrong_account["tool_bundle"]["user"] = json!("other");
    let mut wrong_path = profile(file, None);
    wrong_path["tool_bundle"]["add_to_path"] =
        json!({"shell": "fish", "fish_config_dir": "../fish"});
    let mut wrong_home = profile(file, None);
    wrong_home["tool_bundle"]["home"] = json!("relative");
    let mut wrong_key = profile(file, None);
    wrong_key["tool_bundle"]["fingerprint"] = json!("SHA256:invalid");
    let path = "profiles.json";
    fs::write(
        root.join(path),
        json!({"profiles": {
            "Disabled": disabled, "Durable": durable, "Game": game,
            "Account": wrong_account, "Path": wrong_path, "Home": wrong_home,
            "Host key": wrong_key, "Invalid pin": profile(file, Some("invalid")),
        }})
        .to_string(),
    )
    .unwrap();

    let result = reply(audit(root, &[]));
    let rows = result["profiles"].as_array().unwrap();
    let row = |name: &str| rows.iter().find(|row| row["profile"] == name).unwrap();
    for (name, status) in [
        ("Disabled", "disabled"),
        ("Durable", "not-run"),
        ("Game", "not-run"),
        ("Path", "follows-file"),
        ("Account", "invalid-settings"),
        ("Home", "invalid-settings"),
        ("Host key", "invalid-settings"),
        ("Invalid pin", "invalid-settings"),
    ] {
        assert_eq!(row(name)["status"], status, "{name}");
    }
    assert!(
        row("Path")["path_error"]
            .as_str()
            .unwrap()
            .contains("Fish configuration directory")
    );
    let selected = reply(audit(root, &["--profiles", path, "--profile", "Path"]));
    assert_eq!(selected["profiles"], json!([row("Path")]));
    for args in [
        &["--profile", "Missing"][..],
        &["--profiles", "absent.json"],
        &["--profile"],
        &["--profile", "Path", "--profile", "Game"],
    ] {
        let output = audit(root, args);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stderr).unwrap()["ok"],
            false
        );
    }
}
