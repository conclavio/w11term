use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};
use w11term::sftp::{DotfileBundle, DotfileShell};

fn cli(directory: &Path, verb: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(directory)
        .env_remove("LOCALAPPDATA")
        .arg(verb)
        .args(args)
        .output()
        .unwrap()
}

fn save(directory: &Path, args: &[&str]) -> std::process::Output {
    cli(directory, "save-dotfiles-bundle", args)
}

fn inspect(directory: &Path, args: &[&str]) -> std::process::Output {
    cli(directory, "inspect-dotfiles-bundle", args)
}

fn compare(directory: &Path, args: &[&str]) -> std::process::Output {
    cli(directory, "compare-dotfiles-bundles", args)
}

fn reply(output: std::process::Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn creates_a_portable_bundle_on_stdout_with_only_the_selected_shell() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for (shell, directory_hook, command_hook) in [
        (
            "bash",
            "__w11term_enable_bash_directory",
            "__w11term_enable_bash_integration",
        ),
        (
            "zsh",
            "__w11term_enable_zsh_directory",
            "__w11term_enable_zsh_integration",
        ),
        (
            "fish",
            "__w11term_report_fish_directory",
            "__w11term_fish_preexec",
        ),
    ] {
        for mode in ["directory", "full"] {
            let bundle = reply(save(root, &["--shell", shell, "--setup", mode]));
            assert_eq!(bundle["version"], 1);
            for other in ["bash", "fish", "zsh"]
                .into_iter()
                .filter(|other| *other != shell)
            {
                assert!(bundle[other].is_null());
            }
            let text = bundle[shell].as_str().unwrap();
            assert!(text.contains(directory_hook), "{shell}: {text}");
            assert_eq!(text.contains(command_hook), mode == "full", "{shell}");
            assert_eq!(text.matches("# >>> w11term shell setup >>>").count(), 1);
        }
    }
}

#[test]
fn revises_only_the_selected_shell_and_preserves_the_source_and_existing_destination() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let source = "incoming profile bundle.json";
    let destination = "approved candidate.json";
    let first = reply(save(root, &["--shell", "fish", "--setup", "directory"]));
    let fish = first["fish"].as_str().unwrap().replace('\n', "\r\n");
    let original = json!({
        "version": 1,
        "bash": "PS1='retained> '\r\n",
        "fish_environment": "set -gx EDITOR vi\r\n",
        "fish": format!("# existing prompt setup\r\n{fish}\r\n# user tail\r\n"),
    });
    let original_bytes = serde_json::to_vec(&original).unwrap();
    fs::write(root.join(source), &original_bytes).unwrap();
    let args = [
        "--from-bundle",
        source,
        "--shell",
        "fish",
        "--setup",
        "full",
        "--output-file",
        destination,
    ];
    let saved = reply(save(root, &args));
    assert_eq!(saved["ok"], true);
    let returned_path = Path::new(saved["result"]["path"].as_str().unwrap());
    assert!(returned_path.is_absolute());
    assert_eq!(
        fs::canonicalize(returned_path).unwrap(),
        fs::canonicalize(root.join(destination)).unwrap()
    );
    let authored_bytes = fs::read(root.join(destination)).unwrap();
    let authored: Value = serde_json::from_slice(&authored_bytes).unwrap();
    assert_eq!(authored["bash"], original["bash"]);
    assert_eq!(authored["fish_environment"], original["fish_environment"]);
    let revised = authored["fish"].as_str().unwrap();
    assert!(revised.starts_with("# existing prompt setup\n"));
    assert!(revised.contains("__w11term_fish_preexec"));
    assert!(revised.ends_with("# user tail\n"));
    assert_eq!(revised.matches("# >>> w11term shell setup >>>").count(), 1);
    assert_eq!(
        reply(save(
            root,
            &[
                "--shell",
                "fish",
                "--setup",
                "full",
                "--from-bundle",
                destination
            ]
        )),
        authored
    );
    assert!(!save(root, &args).status.success());
    assert_eq!(fs::read(root.join(source)).unwrap(), original_bytes);
    assert_eq!(fs::read(root.join(destination)).unwrap(), authored_bytes);
}

#[test]
fn refuses_edited_blocks_and_invalid_options_before_writing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::write(
        root.join("edited.json"),
        serde_json::to_vec(&json!({
            "version": 1,
            "bash": "# >>> w11term shell setup >>>\n# custom\n# <<< w11term shell setup <<<",
        }))
        .unwrap(),
    )
    .unwrap();
    let output = save(
        root,
        &[
            "--shell",
            "bash",
            "--setup",
            "full",
            "--from-bundle",
            "edited.json",
            "--output-file",
            "new.json",
        ],
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("block was edited")
    );
    assert!(!root.join("new.json").exists());
    for invalid in [
        &["--shell", "fish", "--setup", "other"][..],
        &["--shell", "sh", "--setup", "full"],
        &["--shell", "zsh", "--setup", "full", "--setup", "directory"],
        &["--shell", "zsh", "--setup"],
        &["--setup", "full"],
        &["--shell", "fish"],
        &["--bash-file"],
        &["--output-file", "new.json"],
    ] {
        assert!(!save(root, invalid).status.success(), "{invalid:?}");
    }
}

#[test]
fn imports_local_files_into_all_five_fragments_in_the_requested_order() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for (path, contents) in [
        (".profile", "\u{feff}export EDITOR=vi\r\n"),
        (".bashrc", "PS1='ops> '"),
        ("team aliases.sh", "alias gs='git status'\r\n"),
        (".zshrc", "PROMPT='zsh> '\r\n"),
        ("environment.fish", "set -gx EDITOR vi\r\n"),
        ("config.fish", "set -g fish_greeting ''\r\n"),
    ] {
        fs::write(root.join(path), contents).unwrap();
    }
    let bundle = reply(save(
        root,
        &[
            "--bash-file",
            ".bashrc",
            "--environment-file",
            ".profile",
            "--zsh-file",
            ".zshrc",
            "--bash-file",
            "team aliases.sh",
            "--fish-environment-file",
            "environment.fish",
            "--fish-file",
            "config.fish",
        ],
    ));
    assert_eq!(
        bundle,
        json!({
            "version": 1,
            "environment": "export EDITOR=vi\n",
            "bash": "PS1='ops> '\nalias gs='git status'\n",
            "zsh": "PROMPT='zsh> '\n",
            "fish_environment": "set -gx EDITOR vi\n",
            "fish": "set -g fish_greeting ''\n",
        })
    );
}

#[test]
fn appends_imports_to_a_revision_and_inserts_setup_after_the_prompt() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let original = br##"{"version":1,"bash":"export PAGER=less\r\n","zsh":"# keep CRLF\r\n"}"##;
    fs::write(root.join("fleet.json"), original).unwrap();
    fs::write(root.join(".bashrc"), "PS1='ops> '\r\n").unwrap();
    let bundle = reply(save(
        root,
        &[
            "--from-bundle",
            "fleet.json",
            "--shell",
            "bash",
            "--setup",
            "full",
            "--bash-file",
            ".bashrc",
        ],
    ));
    let bash = bundle["bash"].as_str().unwrap();
    assert!(bash.starts_with("export PAGER=less\nPS1='ops> '\n"));
    assert!(bash.contains("__w11term_enable_bash_integration"));
    assert_eq!(bundle["zsh"], "# keep CRLF\r\n");
    assert_eq!(fs::read(root.join("fleet.json")).unwrap(), original);
}

#[test]
fn failed_import_leaves_no_bundle_or_partial_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    // At the limit for a single fragment. The second valid file pushes it over.
    fs::write(root.join("full.sh"), vec![b'#'; 16 * 1024]).unwrap();
    fs::write(root.join("extra.sh"), "# more\n").unwrap();
    fs::write(root.join("invalid.sh"), [0xff]).unwrap();
    for (first, second, expected_error) in [
        ("full.sh", "extra.sh", "at most 16 KiB"),
        ("extra.sh", "invalid.sh", "must use UTF-8"),
        (
            "extra.sh",
            "missing.sh",
            "Importing shell fragment missing.sh",
        ),
    ] {
        let output = save(
            root,
            &[
                "--bash-file",
                first,
                "--bash-file",
                second,
                "--output-file",
                "fleet.json",
            ],
        );
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(error["error"].as_str().unwrap().contains(expected_error));
        assert!(!root.join("fleet.json").exists());
    }
}

#[test]
fn inspection_reports_profile_pin_digests_for_each_available_shell() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let path = "team bundle.json";
    fs::write(root.join(".profile"), "export EDITOR=vi\r\n").unwrap();
    fs::write(root.join("config.fish"), "set -gx EDITOR vi\n").unwrap();
    reply(save(
        root,
        &[
            "--environment-file",
            ".profile",
            "--fish-file",
            "config.fish",
            "--output-file",
            path,
        ],
    ));
    let file = DotfileBundle::load(&root.join(path)).unwrap();
    let report = reply(inspect(root, &[path]));
    assert_eq!(report["ok"], true);
    assert_eq!(report["result"]["version"], 1);
    let shells: Vec<_> = DotfileShell::ALL
        .into_iter()
        .map(|shell| json!({"shell": shell, "bundle_sha256": file.content_version(shell).unwrap()}))
        .collect();
    assert_eq!(report["result"]["shells"], Value::Array(shells));
    assert!(!report.to_string().contains("EDITOR"));
}

#[test]
fn pin_verification_uses_normalized_content_and_detects_drift_for_the_selected_shell() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let path = "bundle.json";
    let mut manifest = json!({
        "version": 1,
        "bash": "# original\n",
        "fish": "# original fish",
    });
    fs::write(root.join(path), manifest.to_string()).unwrap();
    let baseline = reply(inspect(root, &[path, "--shell", "bash"]));
    let selected = baseline["result"]["shells"].as_array().unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0]["shell"], "bash");
    let expected = selected[0]["bundle_sha256"].as_str().unwrap();
    manifest["bash"] = json!("# original\r\n");
    manifest["fish"] = json!("# fish changed");
    fs::write(root.join(path), manifest.to_string()).unwrap();
    let verify = || {
        inspect(
            root,
            &[
                path,
                "--shell",
                "bash",
                "--sha256",
                &expected.to_uppercase(),
            ],
        )
    };
    assert_eq!(reply(verify()), baseline);

    manifest["bash"] = json!("# bash changed");
    fs::write(root.join(path), manifest.to_string()).unwrap();
    let drift = verify();
    assert_eq!(drift.status.code(), Some(1), "{drift:?}");
    assert!(drift.stdout.is_empty());
    let error: Value = serde_json::from_slice(&drift.stderr).unwrap();
    let error = error["error"].as_str().unwrap();
    assert!(error.contains("Bash dotfiles digest mismatch"));
    assert!(error.contains(expected));
    assert!(!error.contains("# bash changed"));
}

#[test]
fn inspection_rejects_an_invalid_bundle_or_unavailable_shell() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let path = "bundle.json";
    fs::write(root.join(path), "{\"version\":1,\"fish\":\"# fish\"}").unwrap();
    let report = reply(inspect(root, &[path]));
    assert_eq!(report["result"]["shells"].as_array().unwrap().len(), 1);
    assert_eq!(report["result"]["shells"][0]["shell"], "fish");
    for (args, message) in [
        (
            &[path, "--shell", "bash"][..],
            "needs an environment or bash fragment",
        ),
        (&[path, "--sha256", "bad"], "--sha256 requires --shell"),
        (
            &[path, "--shell", "fish", "--sha256", "bad"],
            "64 hexadecimal digits",
        ),
    ] {
        let output = inspect(root, args);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(error["error"].as_str().unwrap().contains(message));
    }
    for manifest in [
        json!({"version": 1, "bash": "# bash", "fish": "nul\0byte"}),
        json!({"version": 2, "bash": "# bash"}),
        json!({"version": 1}),
    ] {
        fs::write(root.join(path), manifest.to_string()).unwrap();
        assert_eq!(inspect(root, &[path]).status.code(), Some(1));
        assert_eq!(
            inspect(root, &[path, "--shell", "bash"]).status.code(),
            Some(1)
        );
    }
}

#[test]
fn compares_the_profile_pin_and_changed_fragment_roles_for_each_shell() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before_path = "old fleet.json";
    let after_path = "revised fleet.json";
    let old = json!({"version": 1, "environment": "export EDITOR=vi\r\n", "bash": "PS1=old"});
    let new = json!({
        "version": 1,
        "environment": "export EDITOR=vi",
        "bash": "PS1=new\r\n",
        "fish": "set -g fish_greeting new",
    });
    fs::write(root.join(before_path), old.to_string()).unwrap();
    fs::write(root.join(after_path), new.to_string()).unwrap();
    let report = reply(compare(root, &[before_path, after_path]));
    assert_eq!(report["result"]["changed"], true);
    let profiles = |path: &str| reply(inspect(root, &[path]))["result"]["shells"].clone();
    let (before_pins, after_pins) = (profiles(before_path), profiles(after_path));
    assert_eq!(
        report["result"]["shells"],
        json!([
            {
                "shell": "bash",
                "change": "changed",
                "before_bundle_sha256": before_pins[0]["bundle_sha256"],
                "after_bundle_sha256": after_pins[0]["bundle_sha256"],
                "changed_fragments": [{"role": "prompt"}],
            },
            {
                "shell": "zsh",
                "change": "unchanged",
                "before_bundle_sha256": before_pins[1]["bundle_sha256"],
                "after_bundle_sha256": before_pins[1]["bundle_sha256"],
                "changed_fragments": [],
            },
            {
                "shell": "fish",
                "change": "added",
                "before_bundle_sha256": null,
                "after_bundle_sha256": after_pins[2]["bundle_sha256"],
                "changed_fragments": [{"role": "prompt"}],
            },
        ])
    );
    for fragment_text in ["EDITOR", "PS1", "fish_greeting"] {
        assert!(!report.to_string().contains(fragment_text));
    }
    let zsh = reply(compare(root, &[before_path, after_path, "--shell", "zsh"]));
    assert_eq!(zsh["result"]["changed"], false);
    assert_eq!(
        zsh["result"]["shells"],
        json!([report["result"]["shells"][1]])
    );
}

#[test]
fn can_review_exact_normalized_changes_for_a_single_shell() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = json!({"version": 1, "fish_environment": "set -gx EDITOR vi\r\n"});
    let after = json!({"version": 1, "bash": "PS1=new"});
    fs::write(root.join("before.json"), before.to_string()).unwrap();
    fs::write(root.join("after.json"), after.to_string()).unwrap();
    let fish = reply(compare(
        root,
        &[
            "before.json",
            "after.json",
            "--shell",
            "fish",
            "--include-text",
        ],
    ));
    assert_eq!(fish["result"]["shells"].as_array().unwrap().len(), 1);
    let removed = &fish["result"]["shells"][0];
    assert_eq!(removed["change"], "removed");
    assert_eq!(removed["after_bundle_sha256"], Value::Null);
    assert_eq!(
        removed["changed_fragments"],
        json!([
            {"role": "environment", "before": "set -gx EDITOR vi\n", "after": null}
        ])
    );
    let added = reply(compare(
        root,
        &[
            "before.json",
            "after.json",
            "--shell",
            "bash",
            "--include-text",
        ],
    ));
    assert_eq!(
        added["result"]["shells"][0]["changed_fragments"],
        json!([
            {"role": "prompt", "before": null, "after": "PS1=new\n"}
        ])
    );
}

#[test]
fn comparison_rejects_invalid_bundles_and_unavailable_shells() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::write(
        root.join("valid.json"),
        json!({"version": 1, "bash": "PS1=new"}).to_string(),
    )
    .unwrap();
    fs::write(
        root.join("bad.json"),
        json!({"version": 2, "bash": "PS1=bad"}).to_string(),
    )
    .unwrap();
    for (args, message) in [
        (
            &["bad.json", "valid.json"][..],
            "Validating the before dotfiles bundle",
        ),
        (
            &["valid.json", "bad.json"],
            "Validating the after dotfiles bundle",
        ),
        (
            &["valid.json", "valid.json", "--shell", "fish"],
            "Neither bundle has fragments for Fish",
        ),
    ] {
        let output = compare(root, args);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty());
        let error: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert!(error["error"].as_str().unwrap().contains(message));
    }
}
