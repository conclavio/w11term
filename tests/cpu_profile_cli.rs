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
