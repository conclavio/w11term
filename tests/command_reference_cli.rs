use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(directory)
        .env("W11TERM_DATA_DIR", directory)
        .env_remove("LOCALAPPDATA")
        .arg("command-reference")
        .args(args)
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn write_page(directory: &Path, platform: &str, name: &str, text: &str) {
    let pages = directory
        .join("command-reference")
        .join("pages")
        .join(platform);
    fs::create_dir_all(&pages).unwrap();
    fs::write(pages.join(format!("{name}.md")), text).unwrap();
}

#[test]
fn finds_bundled_platform_and_common_manuals_offline() {
    let data = tempfile::tempdir().unwrap();
    let report = json(&run(data.path(), &["Get-ChildItem"]));
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["preferred_platform"], "windows");
    assert_eq!(report["matches"][0]["name"], "get-childitem");
    assert_eq!(report["matches"][0]["platform"], "windows");
    assert_eq!(report["matches"][0]["source"], "tldr");
    assert!(
        report["matches"][0]["examples"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );

    let report = json(&run(data.path(), &["git checkout", "--platform", "linux"]));
    assert_eq!(report["preferred_platform"], "linux");
    assert_eq!(report["matches"][0]["name"], "git-checkout");
    assert_eq!(report["matches"][0]["platform"], "common");
    assert!(!data.path().join("command-reference").exists());
}

#[test]
fn loads_private_overrides_and_searches_their_tasks() {
    let data = tempfile::tempdir().unwrap();
    for (platform, title) in [
        ("linux", "Linux fleet manual"),
        ("windows", "Windows fleet manual"),
        ("common", "Portable fleet manual"),
    ] {
        write_page(
            data.path(),
            platform,
            "fleet-route",
            &format!(
                "# {title}\n\n> Review a tidal manifest.\n> See also: `git log`.\n\n- Show the short report:\n\n`fleet-route --short`\n"
            ),
        );
    }
    let report = json(&run(data.path(), &["fleet route", "--platform", "linux"]));
    assert_eq!(report["matches"][0]["title"], "Linux fleet manual");
    assert_eq!(report["matches"][0]["source"], "private");
    assert_eq!(report["matches"][0]["related"][0], "git log");
    assert_eq!(
        report["matches"][0]["examples"][0]["command"],
        "fleet-route --short"
    );

    let report = json(&run(
        data.path(),
        &["tidal manifest", "--search", "--platform", "windows"],
    ));
    assert_eq!(report["matches"][0]["title"], "Windows fleet manual");
    assert_eq!(
        report["matches"][0]["description"],
        "Review a tidal manifest."
    );
}

#[test]
fn renders_text_safely_and_fails_clearly_for_a_missing_exact_manual() {
    let data = tempfile::tempdir().unwrap();
    write_page(
        data.path(),
        "common",
        "fleet-runbook",
        "# Fleet\x1b[31m runbook\n> Label \u{009b}31m in an example.\n\n- Show help:\n\n`fleet-runbook --help`\n",
    );
    let output = run(data.path(), &["fleet-runbook", "--format", "text"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("fleet-runbook --help"));
    assert!(text.contains("Common"));
    assert!(!text.contains(['\x1b', '\u{009b}']));

    let missing = run(data.path(), &["no-such-command-reference-fixture"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("No offline manual"));

    let empty = json(&run(
        data.path(),
        &["no-such-command-reference-fixture", "--search"],
    ));
    assert_eq!(empty["matches"].as_array().unwrap().len(), 0);
    let help = run(data.path(), &["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--platform PLATFORM"));
}
