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

fn ssh_profile(shell: &str, bundle: &str, pin: Option<&str>) -> Value {
    const KEY: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    json!({
        "host": "fleet.example.test", "user": "dev", "port": 2022,
        "host_key": KEY, "auth": "identity", "identity": "private-ssh-key",
        "dotfiles": {
            "bundle": bundle, "shell": shell, "bundle_sha256": pin,
            "home": "/home/dev", "host": "fleet.example.test", "user": "dev", "port": 2022,
            "fingerprint": KEY,
        },
    })
}

#[test]
fn audits_shared_bundles_and_a_candidate_against_each_profiles_shell_pin() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let catalog_dir = root.join("profile data");
    fs::create_dir(&catalog_dir).unwrap();
    let current = json!({"version": 1, "environment": "export TEAM=ops\r\n", "bash": "PS1=new"});
    let candidate = json!({"version": 1, "environment": "export TEAM=ops", "bash": "PS1=old"});
    let bundle = "shared fragments.json";
    fs::write(catalog_dir.join(bundle), current.to_string()).unwrap();
    fs::write(root.join("candidate.json"), candidate.to_string()).unwrap();
    let baseline = DotfileBundle::load(&root.join("candidate.json")).unwrap();
    let bash_pin = baseline.content_version(DotfileShell::Bash).unwrap();
    let zsh_pin = baseline.content_version(DotfileShell::Zsh).unwrap();
    let catalog = json!({"profiles": {
        "Follow Bash": ssh_profile("bash", bundle, None),
        "Pinned Bash": ssh_profile("bash", bundle, Some(&bash_pin)),
        "Pinned Zsh": ssh_profile("zsh", bundle, Some(&zsh_pin)),
        "Missing": ssh_profile("bash", "removed.json", None),
        "Fish": ssh_profile("fish", bundle, None),
        "Local": {"backend": "local", "executable":"powershell.exe"},
    }})
    .to_string();
    let path = "profile data/profiles.json";
    fs::write(root.join(path), &catalog).unwrap();
    let report = reply(cli(
        root,
        "audit-dotfiles-profiles",
        &["--profiles", path, "--candidate", "candidate.json"],
    ));
    let rows = report["result"]["profiles"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    let row = |name: &str| rows.iter().find(|row| row["profile"] == name).unwrap();
    assert_eq!(row("Follow Bash")["status"], "follows-file");
    assert_eq!(row("Pinned Bash")["status"], "pin-mismatch");
    assert_eq!(
        row("Pinned Bash")["bundle_sha256"],
        row("Follow Bash")["bundle_sha256"]
    );
    assert_eq!(
        row("Pinned Bash")["candidate"]["comparison"],
        "different-content"
    );
    assert_eq!(row("Pinned Bash")["candidate"]["matches_pin"], true);
    assert_eq!(row("Pinned Bash")["candidate"]["bundle_sha256"], bash_pin);
    assert_eq!(row("Pinned Zsh")["status"], "matches-pin");
    assert_eq!(row("Pinned Zsh")["candidate"]["comparison"], "same-content");
    assert_eq!(row("Fish")["status"], "bundle-error");
    assert_eq!(row("Fish")["candidate"]["comparison"], "shell-unavailable");
    assert_eq!(row("Missing")["status"], "bundle-error");
    assert_eq!(
        row("Missing")["candidate"]["comparison"],
        "no-current-content"
    );
    assert_eq!(fs::read_to_string(root.join(path)).unwrap(), catalog);
    assert!(Path::new(row("Pinned Bash")["bundle"].as_str().unwrap()).is_absolute());
    assert!(!report.to_string().contains("PS1="));
    assert!(!report.to_string().contains("private-ssh-key"));
}

#[test]
fn audits_profile_settings_and_can_select_one_saved_ssh_profile() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let path = "profiles.json";
    let standard = ssh_profile("fish", "missing.json", None);
    let mut missing_approval = standard.clone();
    missing_approval.as_object_mut().unwrap().remove("dotfiles");
    let mut durable = standard.clone();
    durable["durable"] = json!(true);
    let mut different_account = standard.clone();
    different_account["dotfiles"]["user"] = json!("other");
    let mut invalid_startup = standard.clone();
    invalid_startup["dotfiles"]["fish_config_dir"] = json!("../fish");
    fs::write(
        root.join(path),
        json!({"profiles": {
            "Disabled": missing_approval, "Durable": durable,
            "Wrong account": different_account, "Wrong startup": invalid_startup,
        }})
        .to_string(),
    )
    .unwrap();
    let report = reply(cli(root, "audit-dotfiles-profiles", &["--profiles", path]));
    let rows = report["result"]["profiles"].as_array().unwrap();
    assert_eq!(rows[0]["status"], "disabled");
    assert_eq!(rows[1]["status"], "not-run");
    assert_eq!(rows[2]["status"], "invalid-settings");
    assert!(
        rows[2]["detail"]
            .as_str()
            .unwrap()
            .contains("different SSH account")
    );
    assert_eq!(rows[3]["status"], "invalid-settings");
    assert!(
        rows[3]["detail"]
            .as_str()
            .unwrap()
            .contains("Fish configuration directory")
    );
    let selected = reply(cli(
        root,
        "audit-dotfiles-profiles",
        &["--profiles", path, "--profile", "Durable"],
    ));
    assert_eq!(selected["result"]["profiles"], json!([rows[1]]));
    let default_catalog = reply(
        Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
            .current_dir(root)
            .env_remove("LOCALAPPDATA")
            .env("W11TERM_DATA_DIR", root)
            .arg("audit-dotfiles-profiles")
            .output()
            .unwrap(),
    );
    assert_eq!(default_catalog, report);
    for args in [
        &["--profiles", "absent.json"][..],
        &["--profiles", path, "--profile", "absent"],
        &["--profiles", path, "--candidate", "absent.json"],
    ] {
        assert_eq!(
            cli(root, "audit-dotfiles-profiles", args).status.code(),
            Some(1)
        );
    }
}

fn rollout(root: &Path, args: &[&str]) -> std::process::Output {
    let mut options = vec![
        "--profiles",
        "profiles.json",
        "--candidate",
        "revision.json",
    ];
    options.extend_from_slice(args);
    cli(root, "rollout-dotfiles-profiles", &options)
}

fn rollback(root: &Path, review: Option<&str>) -> std::process::Output {
    let mut args = vec!["rollout-reply.json"];
    if let Some(review) = review {
        args.extend(["--review", review]);
    }
    cli(root, "rollback-dotfiles-profiles", &args)
}

#[test]
fn reviewed_rollout_updates_only_named_approvals_and_retains_pin_policy() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let catalog = root.join("profiles.json");
    let candidate = root.join("revision.json");
    let old_path = root.join("original.json");
    fs::write(
        &old_path,
        br#"{"version":1,"bash":"PS1=old","fish":"set -g fish_greeting old"}"#,
    )
    .unwrap();
    let old = DotfileBundle::load(&old_path).unwrap();
    let pin = old.content_version(DotfileShell::Bash).unwrap();
    let mut east = ssh_profile("bash", "original.json", Some(&pin));
    east["local_custom_field"] = json!({"keep": true});
    let mut west = ssh_profile("fish", "original.json", None);
    west["dotfiles"]
        .as_object_mut()
        .unwrap()
        .remove("bundle_sha256");
    west["dotfiles"]["fish_config_dir"] = json!("settings/fish");
    let original = json!({
        "profiles": {
            "East": east,
            "West": west,
            "Other": ssh_profile("zsh", "original.json", Some(&pin)),
        },
        "ssh_host_groups": {"Fleet": ["West", "East", "Other"]},
        "local_custom_metadata": "retain",
    });
    fs::write(&catalog, original.to_string()).unwrap();
    fs::write(
        &candidate,
        json!({"version": 1, "bash": "PS1=revision", "fish": "set -g fish_greeting revision"})
            .to_string(),
    )
    .unwrap();
    let preview = reply(rollout(root, &["--profile", "West", "--profile", "East"]));
    let result = &preview["result"];
    assert_eq!(result["applied"], false);
    assert_eq!(result["changed_profiles"], 2);
    assert_eq!(result["profiles"][0]["profile"], "East");
    assert_eq!(
        result["profiles"][0]["baseline"],
        json!({"kind": "pinned", "bundle_sha256": pin})
    );
    assert_eq!(
        result["profiles"][1]["baseline"],
        json!({"kind": "file", "bundle_sha256": old.content_version(DotfileShell::Fish).unwrap()})
    );
    assert!(!preview.to_string().contains("PS1="));
    assert!(!preview.to_string().contains("fish_greeting"));
    let expected_pin = DotfileBundle::load(&candidate)
        .unwrap()
        .content_version(DotfileShell::Bash)
        .unwrap();
    assert_eq!(result["profiles"][0]["next_pin"], expected_pin);
    assert_eq!(result["profiles"][1]["next_pin"], Value::Null);
    let token = result["review"].as_str().unwrap();
    assert_eq!(fs::read_to_string(&catalog).unwrap(), original.to_string());

    let applied = reply(rollout(
        root,
        &["--profile", "East", "--profile", "West", "--review", token],
    ));
    assert_eq!(applied["result"]["applied"], true);
    assert_eq!(applied["result"]["changed_profiles"], 2);
    let mut expected = original.clone();
    let path = &result["candidate_path"];
    expected["profiles"]["East"]["dotfiles"]["bundle"] = path.clone();
    expected["profiles"]["West"]["dotfiles"]["bundle"] = path.clone();
    expected["profiles"]["East"]["dotfiles"]["bundle_sha256"] = json!(expected_pin);
    let after = fs::read_to_string(&catalog).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&after).unwrap(), expected);

    let unchanged = reply(rollout(root, &["--profile", "East", "--profile", "West"]));
    assert_eq!(unchanged["result"]["changed_profiles"], 0);
    let current_token = unchanged["result"]["review"].as_str().unwrap();
    reply(rollout(
        root,
        &[
            "--profile",
            "East",
            "--profile",
            "West",
            "--review",
            current_token,
        ],
    ));
    assert_eq!(fs::read_to_string(&catalog).unwrap(), after);

    fs::write(
        root.join("rollout-reply.json"),
        format!("\u{feff}{applied}"),
    )
    .unwrap();
    let undo = reply(rollback(root, None));
    assert_eq!(undo["result"]["applied"], false);
    assert_eq!(
        undo["result"]["profiles"][0]["restore_status"],
        "matches-pin"
    );
    assert_eq!(
        undo["result"]["profiles"][1]["restore_status"],
        "follows-file"
    );
    assert_eq!(fs::read_to_string(&catalog).unwrap(), after);
    let undo_token = undo["result"]["review"].as_str().unwrap();
    let receipt = reply(rollback(root, Some(undo_token)));
    assert_eq!(receipt["result"]["applied"], true);
    assert_eq!(receipt["result"]["changed_profiles"], 2);
    let mut restored = original.clone();
    restored["profiles"]["East"]["dotfiles"]["bundle"] =
        undo["result"]["profiles"][0]["restore"]["bundle"].clone();
    restored["profiles"]["West"]["dotfiles"]["bundle"] =
        undo["result"]["profiles"][1]["restore"]["bundle"].clone();
    let after_undo = fs::read_to_string(&catalog).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&after_undo).unwrap(),
        restored
    );
    assert_eq!(rollback(root, Some(undo_token)).status.code(), Some(1));
    assert_eq!(fs::read_to_string(&catalog).unwrap(), after_undo);
}

#[test]
fn rollout_rejects_stale_inputs_ineligible_profiles_and_concurrent_writers() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let catalog = root.join("profiles.json");
    let candidate = root.join("revision.json");
    let baseline_path = root.join("original.json");
    let baseline = br#"{"version":1,"bash":"PS1=original"}"#;
    let mut disabled = ssh_profile("bash", "original.json", None);
    disabled.as_object_mut().unwrap().remove("dotfiles");
    let original = json!({"profiles": {
        "East": ssh_profile("bash", "original.json", None),
        "Fish": ssh_profile("fish", "original.json", None),
        "Disabled": disabled,
    }});
    let original_bundle = json!({"version": 1, "bash": "PS1=revision"}).to_string();
    fs::write(&catalog, original.to_string()).unwrap();
    fs::write(&candidate, &original_bundle).unwrap();
    fs::write(&baseline_path, baseline).unwrap();
    for (args, expected) in [
        (&[][..], "Choose at least one SSH profile"),
        (&["--profile", "East", "--profile", "East"], "only once"),
        (&["--profile", "Disabled"], "no dotfiles approval"),
        (
            &["--profile", "East", "--profile", "Fish"],
            "no fragments for Fish",
        ),
    ] {
        let result = rollout(root, args);
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains(expected));
    }
    let plan = reply(rollout(root, &["--profile", "East"]));
    let token = plan["result"]["review"].as_str().unwrap();
    let apply = || rollout(root, &["--profile", "East", "--review", token]);
    fs::write(
        &candidate,
        json!({"version": 1, "bash": "PS1=stale"}).to_string(),
    )
    .unwrap();
    let stale_bundle = apply();
    assert_eq!(stale_bundle.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&stale_bundle.stderr).contains("Preview the rollout again"));
    assert_eq!(fs::read_to_string(&catalog).unwrap(), original.to_string());

    fs::write(&candidate, original_bundle).unwrap();
    fs::write(
        &baseline_path,
        br#"{"version":1,"bash":"PS1=changed-after-review"}"#,
    )
    .unwrap();
    let stale_baseline = apply();
    assert_eq!(stale_baseline.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&stale_baseline.stderr).contains("Preview the rollout again"));
    assert_eq!(fs::read_to_string(&catalog).unwrap(), original.to_string());
    fs::write(&baseline_path, baseline).unwrap();
    let mut moved = original.clone();
    moved["profiles"]["East"]["dotfiles"]["home"] = json!("/home/moved");
    fs::write(&catalog, moved.to_string()).unwrap();
    let stale_target = apply();
    assert_eq!(stale_target.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&stale_target.stderr).contains("Preview the rollout again"));
    assert_eq!(fs::read_to_string(&catalog).unwrap(), moved.to_string());

    fs::write(&catalog, original.to_string()).unwrap();
    let lock = fs::File::create(root.join("profiles.json.w11term.lock")).unwrap();
    lock.try_lock().unwrap();
    let busy = apply();
    assert_eq!(busy.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&busy.stderr).contains("Another window is updating profiles"));
    assert_eq!(fs::read_to_string(&catalog).unwrap(), original.to_string());
}

#[test]
fn rollback_checks_the_applied_receipt_approvals_and_reviewed_files() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let catalog = root.join("profiles.json");
    let original = root.join("previous.json");
    let receipt = root.join("rollout-reply.json");
    let old = br#"{"version":1,"bash":"PS1=old"}"#;
    let revision = br#"{"version":1,"bash":"PS1=revision"}"#;
    fs::write(
        &catalog,
        json!({"profiles": {
            "East": ssh_profile("bash", "previous.json", None),
            "West": ssh_profile("bash", "missing.json", None),
        }})
        .to_string(),
    )
    .unwrap();
    fs::write(&original, old).unwrap();
    fs::write(root.join("revision.json"), revision).unwrap();
    let rollout_preview = reply(rollout(root, &["--profile", "East", "--profile", "West"]));
    fs::write(&receipt, rollout_preview.to_string()).unwrap();
    let rejected = rollback(root, None);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("applied dotfile rollout"));
    let rollout_token = rollout_preview["result"]["review"].as_str().unwrap();
    let applied = reply(rollout(
        root,
        &[
            "--profile",
            "East",
            "--profile",
            "West",
            "--review",
            rollout_token,
        ],
    ));
    fs::write(&receipt, applied.to_string()).unwrap();
    let rolled_out_catalog = fs::read_to_string(&catalog).unwrap();

    let preview = reply(rollback(root, None));
    assert_eq!(
        preview["result"]["profiles"][1]["restore_status"],
        "bundle-unavailable"
    );
    let token = preview["result"]["review"].as_str().unwrap();
    fs::write(&original, br#"{"version":1,"bash":"PS1=changed"}"#).unwrap();
    let changed = rollback(root, Some(token));
    assert_eq!(changed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&changed.stderr).contains("Preview the rollback again"));
    assert_eq!(fs::read_to_string(&catalog).unwrap(), rolled_out_catalog);

    fs::write(&original, old).unwrap();
    let mut edited: Value = serde_json::from_str(&rolled_out_catalog).unwrap();
    edited["profiles"]["West"]["dotfiles"]["home"] = json!("/home/other");
    fs::write(&catalog, edited.to_string()).unwrap();
    let stale = rollback(root, Some(token));
    assert_eq!(stale.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&stale.stderr)
            .contains("West no longer matches the applied rollout")
    );
    assert_eq!(fs::read_to_string(&catalog).unwrap(), edited.to_string());
}
