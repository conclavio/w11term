use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const PROBES: [&str; 10] = [
    "uptime", "dmesg", "vmstat", "mpstat", "pidstat", "iostat", "free", "sar-dev", "sar-tcp", "top",
];

fn fixture(after: bool) -> Value {
    let probes: Vec<_> = PROBES
        .iter()
        .map(|id| json!({ "id": id, "command": id, "sampling": "Same sample setup" }))
        .collect();
    let mut samples: Vec<_> = PROBES
        .iter()
        .map(|id| {
            json!({
                "id": id, "status": "ok", "exit_status": 0,
                "started_unix_ms": 100, "finished_unix_ms": 200,
                "stdout": { "text": "", "at_byte_limit": false },
                "stderr": { "text": "", "at_byte_limit": false },
            })
        })
        .collect();
    if after {
        samples[1]["stdout"]["text"] = json!("System booted");
    } else {
        samples[1]["status"] = json!("permission-denied");
        samples[1]["exit_status"] = json!(1);
        samples[1]["stderr"]["text"] = json!("Operation not permitted");
    }
    let cpu = if after { "70 14 14" } else { "40 12 46" };
    samples[2]["stdout"]["text"] = json!(format!(
        "r b si so us sy id wa st\n99 1 42 42 95 4 1 0 0\n3 0 0 8 {cpu} 1 1\n"
    ));
    let utilization = if after { 75 } else { 20 };
    samples[5]["stdout"]["text"] = json!(format!("Device r/s %util\nsda 3 {utilization}\n"));
    json!({
        "version": 1,
        "target": {
            "connection_generation": 1, "host": "lab.example", "user": "operator", "port": 22,
            "host_key_fingerprint": "SHA256:fixture-key", "deadline_seconds": 60,
            "max_bytes_per_stream": 16384, "probes": probes,
        },
        "collection": { "started_unix_ms": 100, "finished_unix_ms": 200, "supervisor_status": 0, "samples": samples },
        "findings": [{ "label": "Stale saved finding", "value": "This is regenerated from the samples" }],
    })
}

fn compare(directory: &Path, options: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(directory)
        .env_remove("LOCALAPPDATA")
        .args([
            "compare-linux-triage",
            "before report.json",
            "after report.json",
        ])
        .args(options)
        .output()
        .unwrap()
}

fn reports(path: &Path, after: Value) {
    fs::write(path.join("before report.json"), fixture(false).to_string()).unwrap();
    fs::write(path.join("after report.json"), after.to_string()).unwrap();
}

fn change<'a>(report: &'a Value, title: &str) -> &'a Value {
    report["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["title"] == title)
        .unwrap_or_else(|| panic!("Missing {title}: {report}"))
}

#[test]
fn compares_saved_reports_and_keeps_evidence_with_numeric_deltas() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    reports(path, fixture(true));
    let output = compare(path, &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["same_endpoint"], true);
    assert_eq!(report["before"]["file_name"], "before report.json");
    assert_eq!(report["before"]["complete_checks"], 9);
    assert_eq!(report["after"]["complete_checks"], 10);
    let cpu = change(&report, "Peak CPU busy");
    assert_eq!(cpu["before"]["value"], "52% user + system");
    assert_eq!(
        cpu["after"]["evidence"],
        json!({ "stdout_line": 3, "text": "3 0 0 8 70 14 14 1 1" })
    );
    assert_eq!(
        cpu["delta"],
        json!({ "amount": 32.0, "unit": "percentage_points" })
    );
    assert_eq!(
        change(&report, "Peak device busy time")["delta"]["amount"],
        55.0
    );
    assert_eq!(change(&report, "dmesg check status")["kind"], "check");
    let checks = report["changed_checks"].as_array().unwrap();
    let dmesg = checks.iter().find(|check| check["id"] == "dmesg").unwrap();
    assert_eq!(dmesg["before"]["status"], "permission-denied");
    assert_eq!(dmesg["before"]["stderr"]["text"], "Operation not permitted");
    assert_eq!(dmesg["after"]["status"], "ok");
    let rendered = report.to_string();
    assert!(!rendered.contains("SHA256:fixture-key") && !rendered.contains("Stale saved finding"));

    let output = compare(path, &["--format", "text"]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Delta · +32.0 pp"));
    assert!(text.contains("1 | Operation not permitted"));
}

#[test]
fn withholds_incomparable_deltas_and_never_replaces_a_report() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path();
    let mut after = fixture(true);
    after["target"]["host_key_fingerprint"] = json!("SHA256:other-fixture-key");
    after["target"]["probes"][2]["sampling"] = json!("Changed intervals");
    reports(path, after);
    let destination = path.join("comparison.json");
    let save = ["--output-file", "comparison.json"];
    let output = compare(path, &save);
    assert!(output.status.success(), "{output:?}");
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["ok"], true);
    let original = fs::read(&destination).unwrap();
    let report: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(report["same_endpoint"], false);
    assert!(change(&report, "Peak CPU busy")["delta"].is_null());
    assert!(change(&report, "Peak device busy time")["delta"].is_null());
    assert_eq!(
        change(&report, "vmstat check or sampling changed")["kind"],
        "check"
    );
    let collision = compare(path, &save);
    assert!(!collision.status.success());
    assert_eq!(fs::read(&destination).unwrap(), original);

    fs::write(path.join("after report.json"), b"{}").unwrap();
    let invalid = compare(path, &["--output-file", "invalid.json"]);
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    let error: Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("Reading the after triage report")
    );
    assert!(!path.join("invalid.json").exists());
}
