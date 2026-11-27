use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn compare(directory: &Path, after: &str, options: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(directory)
        .env_remove("LOCALAPPDATA")
        .args(["compare-cpu-profiles", "before capture.folded", after])
        .args(options)
        .output()
        .unwrap()
}

#[test]
fn compares_offline_and_writes_the_same_report_to_a_new_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    fs::write(
        path.join("before capture.folded"),
        "main;work[1] 3\nidle 1\n",
    )
    .unwrap();
    let after = "after capture.folded";
    fs::write(path.join(after), "main;work[1] 5\nread 15\n").unwrap();

    let filter = ["--filter", " WoRK[1] "];
    let output = compare(path, after, &filter);
    assert!(output.status.success(), "{:?}", output);
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({
            "schema_version": 1,
            "before": { "file_name": "before capture.folded", "total_samples": 4 },
            "after": { "file_name": "after capture.folded", "total_samples": 20 },
            "filter": "WoRK[1]",
            "total_changed_stacks": 3,
            "changes": [{
                "stack": ["main", "work[1]"],
                "before_samples": 3,
                "after_samples": 5,
                "before_percent": 75.0,
                "after_percent": 25.0,
                "delta_percentage_points": -50.0
            }]
        })
    );

    let destination = "CPU comparison.json";
    let options = ["--output-file", destination, filter[0], filter[1]];
    let saved = compare(path, after, &options);
    assert!(saved.status.success(), "{:?}", saved);
    let reply: Value = serde_json::from_slice(&saved.stdout).unwrap();
    let output_path = Path::new(reply["result"]["path"].as_str().unwrap());
    assert!(output_path.is_absolute());
    assert_eq!(output_path.file_name().unwrap(), destination);
    assert_eq!(fs::read(path.join(destination)).unwrap(), output.stdout);

    let collision = compare(path, after, &["--output-file", destination]);
    assert!(!collision.status.success());
    assert!(collision.stdout.is_empty());
    assert_eq!(fs::read(path.join(destination)).unwrap(), output.stdout);
}

#[test]
fn invalid_inputs_fail_before_creating_a_report() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    fs::write(path.join("before capture.folded"), "main 1\n").unwrap();
    fs::write(path.join("bad.folded"), "missing-count\n").unwrap();

    let output = compare(path, "bad.folded", &["--output-file", "report.json"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["ok"], false);
    let error = error["error"].as_str().unwrap();
    assert!(error.contains("after CPU profile bad.folded"), "{error}");
    assert!(error.contains("Invalid folded stack on line 1"), "{error}");
    assert!(!path.join("report.json").exists());
}

#[test]
fn exports_differential_svg_with_both_widths_and_preserves_existing_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    let after = "after capture.folded";
    fs::write(
        path.join("before capture.folded"),
        "main;work<&> 3\nidle 1\n",
    )
    .unwrap();
    fs::write(path.join(after), "main;work<&> 5\nread 15\n").unwrap();

    let output = compare(path, after, &["--format", "svg"]);
    assert!(output.status.success(), "{output:?}");
    let svg = std::str::from_utf8(&output.stdout).unwrap();
    assert!(svg.contains("<desc>After widths."));
    assert!(svg.contains("work&lt;&amp;&gt;"));
    assert!(svg.contains("Before 0 · After 15 · +75.00 pp"));
    assert!(!svg.contains("idle"));
    assert!(output.stderr.is_empty());

    let widths = ["--format", "svg", "--widths", "before"];
    let output = compare(path, after, &widths);
    assert!(output.status.success(), "{output:?}");
    let svg = std::str::from_utf8(&output.stdout).unwrap();
    assert!(svg.contains("<desc>Before widths."));
    assert!(svg.contains("Before 1 · After 0 · -25.00 pp"));
    assert!(!svg.contains("<title>read"));

    let destination = "differential graph.svg";
    let mut options = widths.to_vec();
    options.extend(["--output-file", destination]);
    let saved = compare(path, after, &options);
    assert!(saved.status.success(), "{saved:?}");
    let reply: Value = serde_json::from_slice(&saved.stdout).unwrap();
    assert!(Path::new(reply["result"]["path"].as_str().unwrap()).is_absolute());
    assert_eq!(fs::read(path.join(destination)).unwrap(), output.stdout);
    let collision = compare(path, after, &options);
    assert!(!collision.status.success());
    assert!(collision.stdout.is_empty());
    assert_eq!(fs::read(path.join(destination)).unwrap(), output.stdout);

    for options in [
        ["--format", "svg", "--widths", "unknown"].as_slice(),
        ["--format", "svg", "--filter", "read"].as_slice(),
        ["--widths", "before"].as_slice(),
        ["--format", "png"].as_slice(),
    ] {
        let error = compare(path, after, options);
        assert!(!error.status.success(), "{options:?}");
        assert!(error.stdout.is_empty());
        let reply: Value = serde_json::from_slice(&error.stderr).unwrap();
        assert_eq!(reply["ok"], false);
    }
}
