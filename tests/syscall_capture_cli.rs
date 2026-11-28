use serde_json::{Value, json};
use std::{fs, process::Command};
use w11term::control::{SyscallFilter, SyscallOptions};

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
