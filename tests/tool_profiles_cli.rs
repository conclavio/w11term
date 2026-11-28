use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};

const VERB: &str = "audit-tool-profiles";
const KEY: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn audit(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(root)
        .env("W11TERM_DATA_DIR", root)
        .arg(VERB)
        .args(args)
        .output()
        .unwrap()
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

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
fn audits_shared_manifests_offline_without_touching_the_catalog() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let catalog = root.join("profile data");
    fs::create_dir(&catalog).unwrap();
    let manifest = manifest();
    let actual = sha256(&manifest);
    let uppercase = actual.to_uppercase();
    let old = sha256(&manifest.replace("local-build", "old-build"));
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
