use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, process::Command};
use w11term::control::{SyscallFilter, SyscallOptions};

#[test]
fn verifies_a_renamed_syscall_trace_and_rejects_mismatched_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let trace = root.join("renamed trace.strace");
    let report = root.join("capture report.json");
    let recorded_trace = b"1234 1700000000.000000 close(3) = 0 <0.000001>\n";
    fs::write(&trace, recorded_trace).unwrap();
    let checksum: String = Sha256::digest(recorded_trace)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut recorded = json!({
        "schema_version": 1, "kind": "syscall-capture", "file_name": "original.strace",
        "bytes": recorded_trace.len(), "sha256": checksum.to_ascii_uppercase(),
        "stop_reason": "command-exited", "command_exit_status": 0,
        "reviewed": {
            "host": "example.test", "user": "test", "port": 22, "host_key_fingerprint": "SHA256:fixture",
            "program": "/bin/true", "directory": "/tmp", "strace_arguments": ["--", "/bin/true"],
            "options": {"program": "true", "arguments": ["$literal", "--"], "directory": "/tmp", "seconds": 15,
                        "max_mib": 1, "string_size": 128, "filter": "all"}
        }
    });
    let save = |value: &Value| fs::write(&report, serde_json::to_vec(value).unwrap()).unwrap();
    let verify = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
            .current_dir(root)
            .env_remove("LOCALAPPDATA")
            .args([
                "verify-syscall-capture-report",
                "renamed trace.strace",
                "capture report.json",
            ])
            .args(extra)
            .output()
            .unwrap()
    };
    let reject = |extra: &[&str], expected: &str| {
        let output = verify(extra);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        let reply: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(
            reply["error"].as_str().unwrap().contains(expected),
            "{reply}"
        );
    };
    save(&recorded);
    let result = verify(&[]);
    assert!(result.status.success(), "{result:?}");
    let verified: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(verified["result"]["capture_path"], json!(trace));
    assert_eq!(verified["result"]["report_path"], json!(report));
    assert_eq!(verified["result"]["report"]["sha256"], checksum);
    assert_eq!(verified["result"]["report"]["file_name"], "original.strace");
    assert_eq!(
        verified["result"]["report"]["reviewed"],
        recorded["reviewed"]
    );
    assert!(verified["result"].get("settings_path").is_none());
    let output = root.join("settings to rerun.json");
    let export = ["--save-settings", "settings to rerun.json"];
    let saved = verify(&export);
    assert!(saved.status.success(), "{saved:?}");
    let reply: Value = serde_json::from_slice(&saved.stdout).unwrap();
    assert_eq!(reply["result"]["settings_path"], json!(output));
    let settings = fs::read(&output).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&settings).unwrap(),
        json!({"schema_version": 1, "options": recorded["reviewed"]["options"]})
    );
    let loaded = SyscallOptions::load_settings(&output).unwrap();
    assert_eq!(loaded.program, "true");
    assert_eq!(loaded.arguments, ["$literal", "--"]);
    reject(&export, "Saving verified syscall capture settings");
    assert_eq!(fs::read(&output).unwrap(), settings);
    reject(&["--save-settings"], "use --save-settings PATH");
    let invalid_output = root.join("invalid settings.json");
    let invalid_export = ["--save-settings", "invalid settings.json"];
    fs::write(&trace, &recorded_trace[..recorded_trace.len() - 1]).unwrap();
    reject(&invalid_export, "File byte count differs");
    let mut edited = recorded_trace.to_vec();
    edited[0] ^= 1;
    fs::write(&trace, edited).unwrap();
    reject(&invalid_export, "Trace SHA-256 differs");
    assert!(!invalid_output.exists());
    recorded["schema_version"] = json!(2);
    save(&recorded);
    reject(&[], "Unsupported syscall capture report version");
    recorded["schema_version"] = json!(1);
    recorded["bytes"] = json!(2 * 1024 * 1024);
    save(&recorded);
    reject(&[], "invalid byte count");
    recorded["bytes"] = json!(recorded_trace.len());
    recorded["stop_reason"] = json!("stopped");
    save(&recorded);
    reject(&[], "invalid capture outcome");
    fs::write(report, vec![b' '; 256 * 1024 + 1]).unwrap();
    reject(&[], "Syscall capture report exceeds 256 KiB");
}

#[test]
fn saves_portable_syscall_settings_with_literal_arguments_and_no_overwrite() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let save = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
            .current_dir(root)
            .env_remove("LOCALAPPDATA")
            .args([
                "save-syscall-capture-settings",
                "--program",
                "/usr/bin/cat",
                "--arg",
                "--",
                "--arg",
                "a $file 'name'\n",
                "--arg",
                "",
                "--directory",
                "/tmp",
                "--filter",
                "files",
                "--max-mib",
                "4",
            ])
            .args(extra)
            .output()
            .unwrap()
    };
    let printed = save(&[]);
    assert!(printed.status.success(), "{printed:?}");
    assert!(printed.stderr.is_empty());
    let settings: Value = serde_json::from_slice(&printed.stdout).unwrap();
    assert_eq!(
        settings,
        json!({"schema_version": 1, "options": {
            "program": "/usr/bin/cat", "arguments": ["--", "a $file 'name'\n", ""],
            "directory": "/tmp", "filter": "files", "seconds": 15, "max_mib": 4, "string_size": 128,
        }})
    );

    let name = "working capture.json";
    let path = root.join(name);
    let output = save(&["--output-file", name]);
    assert!(output.status.success(), "{output:?}");
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["result"]["path"], json!(path));
    assert_eq!(fs::read(&path).unwrap(), printed.stdout);
    let loaded = SyscallOptions::load_settings(&path).unwrap();
    assert!(matches!(loaded.filter, SyscallFilter::Files));
    assert_eq!(loaded.arguments, ["--", "a $file 'name'\n", ""]);
    for flags in [
        &["--seconds", "20", "--output-file", name][..],
        &["--seconds", "61", "--output-file", "invalid.json"],
        &["--pane", "1", "--output-file", "invalid.json"],
        &["--review-id", "old-review"],
    ] {
        let failed = save(flags);
        assert!(!failed.status.success(), "{failed:?}");
        assert!(failed.stdout.is_empty());
    }
    assert_eq!(fs::read(path).unwrap(), printed.stdout);
    assert!(!root.join("invalid.json").exists());
}

#[test]
fn loads_only_bounded_versioned_syscall_options() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trace settings.json");
    let options = SyscallOptions {
        program: "/bin/true".into(),
        arguments: vec!["\u{1}".repeat(8192 - "/bin/true".len())],
        directory: String::new(),
        seconds: 60,
        max_mib: 64,
        string_size: 256,
        filter: SyscallFilter::All,
    };
    let original = options.clone().settings_json().unwrap();
    fs::write(&path, &original).unwrap();
    assert_eq!(
        SyscallOptions::load_settings(&path).unwrap().arguments,
        options.arguments
    );
    let mut json: Value = serde_json::from_str(&original).unwrap();
    let load = |document: &Value| {
        fs::write(&path, serde_json::to_vec(document).unwrap()).unwrap();
        SyscallOptions::load_settings(&path)
    };
    json["schema_version"] = json!(2);
    assert!(load(&json).unwrap_err().to_string().contains("version 2"));
    json["schema_version"] = json!(1);
    json["options"]["connection_generation"] = json!(8);
    assert!(load(&json).is_err());
    json["options"]
        .as_object_mut()
        .unwrap()
        .remove("connection_generation");
    json["output_file"] = json!("stale.strace");
    assert!(load(&json).is_err());
    json.as_object_mut().unwrap().remove("output_file");
    json["options"]["seconds"] = json!(61);
    assert!(
        load(&json)
            .unwrap_err()
            .to_string()
            .contains("1..=60 seconds")
    );
    fs::write(&path, vec![b' '; 64 * 1024 + 1]).unwrap();
    assert!(
        SyscallOptions::load_settings(&path)
            .unwrap_err()
            .to_string()
            .contains("64 KiB")
    );
}
