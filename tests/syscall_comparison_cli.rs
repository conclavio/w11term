use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn compare(directory: &Path, options: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(directory)
        .env_remove("LOCALAPPDATA")
        .arg("compare-syscall-traces")
        .args(options)
        .output()
        .unwrap()
}

fn report(output: &Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn compares_offline_with_matching_error_evidence_and_optional_timing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    fs::write(
        path.join("working trace.strace"),
        concat!(
            "21 1700000000.000000 execve(\"/bin/diag\", [\"diag\"], NULL) = 0 <0.000001>\n",
            "21 1700000000.001000 access(\"/tmp/settings\", R_OK) = 0 <0.000001>\n",
            "21 1700000000.002000 access(\"/tmp/settings\", R_OK) = 0 <0.000001>\n",
            "21 1700000000.003000 clock_nanosleep(0, 0, 0x0, NULL) = 0 <0.002000>\n",
        ),
    )
    .unwrap();
    fs::write(path.join("failing trace.strace"), concat!(
        "87 1800000000.000000 execve(\"/bin/diag\", [\"diag\"], NULL) = 0 <0.000001>\n",
        "87 1800000000.001000 access(\"/tmp/settings\", R_OK) = -1 ENOENT (No such file or directory) <0.000001>\n",
        "87 1800000000.002000 access(\"/tmp/settings\", R_OK) = -1 EACCES (Permission denied) <0.000001>\n",
        "87 1800000000.003000 clock_nanosleep(0, 0, 0x0, NULL) = 0 <0.005000>\n",
    )).unwrap();
    let inputs = [
        "--working",
        "working trace.strace",
        "--failing",
        "failing trace.strace",
    ];
    let all = report(&compare(path, &inputs));
    assert_eq!(all["schema_version"], 1);
    assert_eq!(all["limited"], false);
    assert_eq!(all["total_changes"], 2);
    assert_eq!(all["changes"].as_array().unwrap().len(), 1);
    assert!(all.get("timing_anchor").is_none());
    let with_timing = [&inputs[..], &["--include-timing"]].concat();
    let timed = report(&compare(path, &with_timing));
    assert_eq!(timed["changes"].as_array().unwrap().len(), 2);
    assert_eq!(timed["changes"][1]["kind"], "timing");
    assert_eq!(timed["changes"][1]["duration_delta_ms"], 3.0);

    let filtered = [&inputs[..], &["--filter", " access EACCES "]].concat();
    let result = compare(path, &filtered);
    let filtered_report = report(&result);
    assert_eq!(filtered_report["filter"], "access EACCES");
    assert_eq!(
        filtered_report["working"]["file_names"],
        json!(["working trace.strace"])
    );
    assert_eq!(filtered_report["changes"].as_array().unwrap().len(), 1);
    let change = &filtered_report["changes"][0];
    assert_eq!(change["kind"], "result");
    assert_eq!(change["syscall"], "access");
    assert_eq!(change["first_divergence"], true);
    assert_eq!(change["count_delta"], 0);
    assert_eq!(change["result_changes"], 2);
    assert_eq!(change["representative_failing_result"], "ENOENT");
    assert_eq!(change["evidence_pair"], 2);
    assert_eq!(change["working_evidence"]["elapsed_ns"], 2_000_000);
    assert_eq!(
        change["failing_evidence"],
        json!({
            "pid": 87,
            "outcome": "error",
            "errno": "EACCES",
            "elapsed_ns": 2_000_000,
            "duration_ns": 1_000,
            "sources": [{ "file_name": "failing trace.strace", "line": 3 }],
        })
    );

    let destination = "syscall comparison.json";
    let save = [&filtered[..], &["--output-file", destination]].concat();
    let receipt = report(&compare(path, &save));
    assert_eq!(receipt["ok"], true);
    assert_eq!(fs::read(path.join(destination)).unwrap(), result.stdout);
    let collision = compare(
        path,
        &[&inputs[..], &["--output-file", destination]].concat(),
    );
    assert!(!collision.status.success());
    assert_eq!(fs::read(path.join(destination)).unwrap(), result.stdout);
}

#[test]
fn uses_even_an_unchanged_call_as_a_timing_reference() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    fs::write(
        path.join("working.strace"),
        concat!(
            "1700000000.000000 execve(\"/bin/diag\", [\"diag\"], NULL) = 0\n",
            "1700000000.001000 access(\"/tmp/before\", F_OK) = 0\n",
            "1700000000.006000 access(\"/tmp/phase\", F_OK) = 0\n",
            "1700000000.010000 access(\"/tmp/after\", F_OK) = 0\n",
        ),
    )
    .unwrap();
    fs::write(
        path.join("failing.strace"),
        concat!(
            "execve(\"/bin/diag\", [\"diag\"], NULL) = 0\n",
            "1800000000.004000 access(\"/tmp/before\", F_OK) = -1 ENOENT (Missing)\n",
            "1800000000.008000 access(\"/tmp/phase\", F_OK) = 0\n",
            "1800000000.013000 access(\"/tmp/after\", F_OK) = -1 EACCES (Permission denied)\n",
        ),
    )
    .unwrap();
    let inputs = ["--working", "working.strace", "--failing", "failing.strace"];
    let anchored = [&inputs[..], &["--anchor", "1:3,1:3"]].concat();
    let result = report(&compare(path, &anchored));
    assert_eq!(
        result["timing_anchor"],
        json!({
            "syscall": "access",
            "working": {"file_number": 1, "file_name": "working.strace", "line": 3},
            "failing": {"file_number": 1, "file_name": "failing.strace", "line": 3},
        })
    );
    let changes = result["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0]["working_evidence"]["elapsed_ns"], 1_000_000);
    assert!(changes[0]["failing_evidence"]["elapsed_ns"].is_null());
    assert_eq!(
        changes[0]["working_evidence"]["offset_from_anchor_ns"],
        -5_000_000
    );
    assert_eq!(
        changes[0]["failing_evidence"]["offset_from_anchor_ns"],
        -4_000_000
    );
    assert_eq!(
        changes[1]["working_evidence"]["offset_from_anchor_ns"],
        4_000_000
    );
    assert_eq!(
        changes[1]["failing_evidence"]["offset_from_anchor_ns"],
        5_000_000
    );

    let filtered = [&anchored[..], &["--filter", "EACCES"]].concat();
    let filtered = report(&compare(path, &filtered));
    assert_eq!(filtered["timing_anchor"], result["timing_anchor"]);
    assert_eq!(filtered["changes"], json!([changes[1]]));

    for (selector, message) in [
        ("0:3,1:3", "one-based file and line"),
        ("1:1,1:1", "reliably aligned call"),
        ("1:3,1:4", "reliably aligned call"),
        ("2:3,1:3", "reliably aligned call"),
    ] {
        let failed = compare(path, &[&inputs[..], &["--anchor", selector]].concat());
        assert!(!failed.status.success());
        assert!(failed.stdout.is_empty());
        let error: Value = serde_json::from_slice(&failed.stderr).unwrap();
        assert!(
            error["error"].as_str().unwrap().contains(message),
            "{error}"
        );
    }
}

#[test]
fn compares_split_runs_and_rejects_mismatched_input_sets() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    for (name, text) in [
        (
            "ok.21",
            concat!(
                "1700000000.000000 execve(\"/bin/diag\", [\"diag\"], NULL) = 0\n",
                "1700000000.001000 clone(flags=SIGCHLD) = 22\n",
            ),
        ),
        (
            "ok.22",
            concat!(
                "1700000000.002000 execve(\"/bin/helper\", [\"helper\"], NULL) = 0\n",
                "1700000000.003000 openat(AT_FDCWD, \"/tmp/settings\", O_RDONLY) = 3</tmp/settings>\n",
            ),
        ),
        (
            "failed.77",
            concat!(
                "1800000000.000000 execve(\"/bin/diag\", [\"diag\"], NULL) = 0\n",
                "1800000000.001000 clone(flags=SIGCHLD) = 78\n",
            ),
        ),
        (
            "failed.78",
            concat!(
                "1800000000.002000 execve(\"/bin/helper\", [\"helper\"], NULL) = 0\n",
                "1800000000.003000 openat(AT_FDCWD, \"/tmp/settings\", O_RDONLY) = -1 ENOENT (No such file or directory)\n",
            ),
        ),
    ] {
        fs::write(path.join(name), text).unwrap();
    }
    let result = report(&compare(
        path,
        &[
            "--working",
            "ok.22",
            "--working",
            "ok.21",
            "--failing",
            "failed.78",
            "--failing",
            "failed.77",
            "--anchor",
            "1:1,1:1",
        ],
    ));
    assert_eq!(result["working"]["merge_order"], "epoch");
    assert_eq!(
        result["failing"]["file_names"],
        json!(["failed.78", "failed.77"])
    );
    assert_eq!(result["total_changes"], 1);
    let change = &result["changes"][0];
    assert_eq!(change["syscall"], "openat");
    assert_eq!(change["executable"], "/bin/helper");
    assert_eq!(change["working_evidence"]["pid"], 22);
    assert_eq!(change["failing_evidence"]["elapsed_ns"], 3_000_000);
    assert_eq!(
        change["failing_evidence"]["offset_from_anchor_ns"],
        1_000_000
    );
    assert_eq!(result["timing_anchor"]["working"]["file_name"], "ok.22");
    assert_eq!(
        change["failing_evidence"]["sources"],
        json!([{"file_name": "failed.78", "line": 2}])
    );

    let failed = compare(
        path,
        &[
            "--working",
            "ok.21",
            "--working",
            "failed.78",
            "--failing",
            "failed.77",
            "--output-file",
            "invalid.json",
        ],
    );
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    let error: Value = serde_json::from_slice(&failed.stderr).unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("Reading the working trace")
    );
    assert!(!path.join("invalid.json").exists());
}
