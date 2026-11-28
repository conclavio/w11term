use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::Path,
    process::{Command, Output},
    sync::Arc,
};
use w11term::{
    cpu_profile::{Comparison, Profile},
    ssh::verify_cpu_profile_report,
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

fn artifact(file_name: &str, bytes: &[u8]) -> Value {
    let sha256: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    json!({
        "file_name": file_name,
        "bytes": bytes.len(),
        "sha256": sha256,
    })
}

fn process_report(file_name: &str, stacks: &[u8], samples: u64) -> Value {
    json!({
        "schema_version": 1,
        "kind": "cpu-profile-capture",
        "folded": artifact(file_name, stacks),
        "reviewed": {
            "host": "bench.example",
            "user": "reviewed",
            "port": 22,
            "host_key_fingerprint": "SHA256:fixture",
            "event": "cpu-clock:u",
            "target": {
                "mode": "process",
                "process_name": "main",
                "process_start_ticks": 800,
                "options": { "target_pid": 42, "seconds": 2, "frequency": 99, "call_graph": "fp" },
            },
            "raw_perf_data_requested": false,
        },
        "samples": samples,
        "distinct_stacks": 1,
        "unresolved_frames": 0,
        "stopped_by_user": false,
    })
}

fn assert_rejected(output: Output, expected: &str) {
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    let reply: Value = serde_json::from_slice(&output.stderr).unwrap();
    let error = reply["error"].as_str().unwrap();
    assert!(error.contains(expected), "{error}");
}

#[test]
fn verifies_renamed_cpu_artifacts_and_reports_distinct_mismatches() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    let folded = b"main 3\n";
    let raw = b"PERFILE2 fixture";
    fs::write(path.join("renamed profile.folded"), folded).unwrap();
    fs::write(path.join("renamed raw.data"), raw).unwrap();
    let mut report = process_report("original.folded", folded, 3);
    report["reviewed"]["raw_perf_data_requested"] = json!(true);
    report["raw_perf_data"] = artifact("original.perf.data", raw);
    let save_report = |report: &Value| {
        fs::write(
            path.join("report.json"),
            serde_json::to_vec(report).unwrap(),
        )
        .unwrap();
    };
    save_report(&report);
    let verify = |options: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
            .current_dir(path)
            .env_remove("LOCALAPPDATA")
            .args([
                "verify-cpu-profile-report",
                "renamed profile.folded",
                "report.json",
            ])
            .args(options)
            .output()
            .unwrap()
    };
    let options = ["--perf-data", "renamed raw.data"];
    let both = verify(&options);
    assert!(both.status.success(), "{both:?}");
    let reply: Value = serde_json::from_slice(&both.stdout).unwrap();
    assert_eq!(reply["result"]["report"], report);
    assert!(
        Path::new(reply["result"]["verified_perf_data_path"].as_str().unwrap())
            .ends_with("renamed raw.data")
    );

    fs::write(path.join("renamed raw.data"), b"PERFILE2 modified").unwrap();
    assert_rejected(verify(&options), "byte count differs");
    fs::write(path.join("renamed raw.data"), b"PERFILE2 Fixture").unwrap();
    assert_rejected(verify(&options), "Raw perf recording SHA-256 differs");
    let folded_only = verify(&[]);
    assert!(folded_only.status.success(), "{folded_only:?}");
    let reply: Value = serde_json::from_slice(&folded_only.stdout).unwrap();
    assert!(reply["result"]["verified_perf_data_path"].is_null());

    fs::write(path.join("renamed profile.folded"), b"main 4\n").unwrap();
    assert_rejected(verify(&[]), "Folded profile SHA-256 differs");
    report["schema_version"] = json!(2);
    save_report(&report);
    assert_rejected(verify(&[]), "Unsupported CPU profile report version 2");
    fs::write(path.join("report.json"), vec![b' '; 256 * 1024 + 1]).unwrap();
    assert_rejected(verify(&[]), "CPU profile report exceeds 256 KiB");
}

#[test]
fn comparison_exports_verified_reports_and_only_compares_available_sampling_settings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    let [before, after] = [b"main 3\n", b"main 9\n"];
    let after_file = "after capture.folded";
    fs::write(path.join("before capture.folded"), before).unwrap();
    fs::write(path.join(after_file), after).unwrap();
    let before_report = process_report("original before.folded", before, 3);
    let mut after_report = process_report("original after.folded", after, 9);
    let save = |file, report: &Value| {
        fs::write(path.join(file), serde_json::to_vec(report).unwrap()).unwrap();
    };
    save("before report.json", &before_report);
    save("after report.json", &after_report);

    let read = |options: &[&str]| -> Value {
        let output = compare(path, after_file, options);
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let both = [
        "--before-report",
        "before report.json",
        "--after-report",
        "after report.json",
    ];
    let same = read(&both);
    assert_eq!(same["sampling_differences"], json!([]));
    assert_eq!(same["before"]["capture_report"], before_report);
    assert_eq!(same["after"]["capture_report"], after_report);

    after_report["reviewed"]["event"] = json!("cycles:u");
    after_report["reviewed"]["target"]["options"]["seconds"] = json!(4);
    after_report["reviewed"]["target"]["options"]["frequency"] = json!(67);
    after_report["reviewed"]["target"]["options"]["call_graph"] = json!("dwarf");
    save("after report.json", &after_report);
    let changed = read(&both);
    assert_eq!(
        changed["sampling_differences"],
        json!(["event", "duration", "frequency", "unwind-mode"])
    );
    assert_eq!(changed["after"]["capture_report"], after_report);
    assert_eq!(changed["changes"], json!([]));

    let one = read(&both[2..]);
    assert!(one.get("sampling_differences").is_none());
    assert!(one["before"].get("capture_report").is_none());
    assert_eq!(one["after"]["capture_report"], after_report);

    let wrong = compare(
        path,
        after_file,
        &[
            "--after-report",
            "before report.json",
            "--output-file",
            "should not exist.json",
        ],
    );
    assert_rejected(wrong, "Verifying the After capture report");
    assert!(!path.join("should not exist.json").exists());
    let paths = [path.join("before capture.folded"), path.join(after_file)];
    let folded = paths
        .each_ref()
        .map(|path| Arc::new(Profile::open(path).unwrap()));
    let verified_before =
        verify_cpu_profile_report(&paths[0], &path.join("before report.json"), None).unwrap();
    let comparison = Arc::new(Comparison::new(folded, paths));
    let mut invalid_json = Vec::new();
    let error = comparison
        .filter("")
        .write_json_with_capture_reports(&mut invalid_json, [None, Some(&verified_before)])
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(invalid_json.is_empty());
    assert_rejected(
        compare(
            path,
            after_file,
            &["--format", "svg", "--before-report", "before report.json"],
        ),
        "apply to JSON comparisons",
    );
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
