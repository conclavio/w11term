use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn save(directory: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(directory)
        .env_remove("LOCALAPPDATA")
        .arg("save-dotfiles-bundle")
        .args(args)
        .output()
        .unwrap()
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
    ] {
        assert!(!save(root, invalid).status.success(), "{invalid:?}");
    }
}
