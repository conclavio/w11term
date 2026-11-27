use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn inspect(directory: &Path, file: &str, options: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(directory)
        .env_remove("LOCALAPPDATA")
        .args(["inspect-packet-capture", file])
        .args(options)
        .output()
        .unwrap()
}

fn compare(directory: &Path, before: &str, after: &str, options: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(directory)
        .env_remove("LOCALAPPDATA")
        .args(["compare-packet-captures", before, after])
        .args(options)
        .output()
        .unwrap()
}

fn raw_ip_capture(pairs: impl IntoIterator<Item = (u32, u32)>) -> Vec<u8> {
    let mut bytes = classic_capture()[..24].to_vec();
    bytes[20] = 101;
    for (index, (a, b)) in pairs.into_iter().enumerate() {
        bytes.extend(
            [10 + index as u32, 0, 20, 20]
                .into_iter()
                .flat_map(u32::to_le_bytes),
        );
        bytes.extend([0x45, 0, 0, 20, 0, 0, 0, 0, 64, 253, 0, 0]);
        bytes.extend(a.to_be_bytes());
        bytes.extend(b.to_be_bytes());
    }
    bytes
}

fn classic_capture() -> Vec<u8> {
    let mut bytes = vec![
        0xd4, 0xc3, 0xb2, 0xa1, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 64, 0, 0, 0, 1, 0, 0, 0,
    ];
    bytes.extend(
        [10_u32, 250_000, 4, 4]
            .into_iter()
            .flat_map(u32::to_le_bytes),
    );
    bytes.extend([0_u8; 4]);
    bytes.extend(
        [11_u32, 500_000, 2, 8]
            .into_iter()
            .flat_map(u32::to_le_bytes),
    );
    bytes.extend([0_u8; 2]);
    bytes
}

#[test]
fn inspects_offline_and_writes_the_same_report_to_a_new_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let input = "saved capture.pcap";
    fs::write(root.join(input), classic_capture()).unwrap();
    let output = inspect(root, input, &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let mut report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let timeline = report.as_object_mut().unwrap().remove("timeline").unwrap();
    assert_eq!(timeline["start_unix_ns"], "10223616000");
    assert_eq!(timeline["bucket_width_ns"], "32768000");
    let buckets = timeline["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 39);
    assert_eq!(buckets[0], json!({"packets": 1, "captured_bytes": 4}));
    assert_eq!(buckets[1], json!({"packets": 0, "captured_bytes": 0}));
    assert_eq!(buckets[38], json!({"packets": 1, "captured_bytes": 2}));
    assert_eq!(
        report,
        json!({
            "schema_version": 1,
            "format": "pcap",
            "file_name": input,
            "file_bytes": 62,
            "packets": 2,
            "captured_bytes": 6,
            "truncated_packets": 1,
            "timestamped_packets": 2,
            "timestamp_span_ns": "1250000000",
            "ip_conversations": {
                "tracked_pairs": 0,
                "unclassified_packets": 2,
                "untracked_packets": 0,
                "top": []
            },
            "interfaces": 1,
            "interface_details": [{
                "section": null,
                "interface_id": 0,
                "name": null,
                "link_type": 1,
                "snap_length": 64,
                "capture_statistics": null,
                "packets": 2,
                "captured_bytes": 6,
                "truncated_packets": 1,
                "timestamped_packets": 2,
                "timestamp_span_ns": "1250000000"
            }],
            "link_types": [1],
            "complete": true,
            "issue": null
        })
    );

    let filename = "packet report.json";
    let saved = inspect(root, input, &["--output-file", filename]);
    assert!(saved.status.success(), "{saved:?}");
    let reply: Value = serde_json::from_slice(&saved.stdout).unwrap();
    let reported = Path::new(reply["result"]["path"].as_str().unwrap());
    assert!(reported.is_absolute());
    assert_eq!(reported.file_name().unwrap(), filename);
    assert_eq!(fs::read(root.join(filename)).unwrap(), output.stdout);
    let collision = inspect(root, input, &["--output-file", filename]);
    assert!(!collision.status.success());
    assert!(collision.stdout.is_empty());
    assert_eq!(fs::read(root.join(filename)).unwrap(), output.stdout);
}

#[test]
fn reports_a_partial_capture_and_rejects_an_unrecognized_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let input = "partial.pcap";
    let mut bytes = classic_capture();
    bytes.pop();
    fs::write(root.join(input), bytes).unwrap();
    let output = inspect(root, input, &[]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["complete"], false);
    assert_eq!(report["packets"], 1);
    assert_eq!(report["timestamp_span_ns"], "0");
    assert_eq!(
        report["timeline"]["buckets"],
        json!([{"packets": 1, "captured_bytes": 4}])
    );
    assert!(report["issue"].as_str().unwrap().contains("incomplete"));

    let input = "empty.pcap";
    fs::write(root.join(input), &classic_capture()[..24]).unwrap();
    let empty = inspect(root, input, &[]);
    assert!(empty.status.success(), "{empty:?}");
    let report: Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(report["complete"], true);
    assert!(report["timeline"].is_null());

    let input = "not a capture.pcap";
    fs::write(root.join(input), b"not a capture file").unwrap();
    let invalid = inspect(root, input, &["--output-file", "invalid.json"]);
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    let reply: Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert_eq!(reply["ok"], false);
    assert!(reply["error"].as_str().unwrap().contains("PCAP or PCAPNG"));
    assert!(!root.join("invalid.json").exists());
}

#[test]
fn compares_beyond_the_ranked_summary_and_keeps_direction_only_changes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = "before.pcap";
    let after = "after.pcap";
    fs::write(
        root.join(before),
        raw_ip_capture((1..=12).map(|peer| (0, peer))),
    )
    .unwrap();
    let mut after_bytes = raw_ip_capture((1..=11).map(|peer| (0, peer)).chain([(12, 0), (0, 13)]));
    fs::write(root.join(after), &after_bytes).unwrap();
    let output = compare(root, before, after, &[]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["before"]["file_name"], before);
    assert_eq!(report["after"]["file_name"], after);
    assert_eq!(report["before"]["timestamp_span_ns"], "11000000000");
    assert_eq!(
        report["totals_delta"],
        json!({"packets": 1, "captured_bytes": 20, "truncated_packets": 0})
    );
    let changes = report["conversation_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0]["b"], "0.0.0.13");
    assert_eq!(changes[0]["before"]["a_to_b"]["packets"], 0);
    assert_eq!(changes[0]["delta"]["captured_bytes"], 20);
    assert_eq!(changes[1]["b"], "0.0.0.12");
    assert_eq!(changes[1]["before"]["a_to_b"]["packets"], 1);
    assert_eq!(changes[1]["after"]["b_to_a"]["packets"], 1);
    assert_eq!(changes[1]["delta"]["captured_bytes"], 0);

    let saved = compare(root, before, after, &["--output-file", "comparison.json"]);
    assert!(saved.status.success(), "{saved:?}");
    assert_eq!(
        fs::read(root.join("comparison.json")).unwrap(),
        output.stdout
    );
    after_bytes.pop();
    fs::write(root.join(after), after_bytes).unwrap();
    let partial = compare(root, before, after, &[]);
    assert!(partial.status.success(), "{partial:?}");
    let report: Value = serde_json::from_slice(&partial.stdout).unwrap();
    assert_eq!(report["after"]["complete"], false);
    assert!(report["totals_delta"].is_null());
    assert_eq!(report["conversation_changes"][0]["b"], "0.0.0.12");
    assert!(report["conversation_changes"][0]["delta"].is_null());
}

#[test]
fn a_pair_missing_after_the_tracking_limit_has_unknown_coverage() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = "many.pcap";
    let after = "one.pcap";
    fs::write(
        root.join(before),
        raw_ip_capture((1..=4097).map(|peer| (0, peer))),
    )
    .unwrap();
    fs::write(root.join(after), raw_ip_capture([(0, 4097)])).unwrap();
    let output = compare(root, before, after, &[]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["before"]["untracked_packets"], 1);
    let changes = report["conversation_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 4097);
    let unknown = changes.last().unwrap();
    assert_eq!(unknown["b"], "0.0.16.1");
    assert!(unknown["before"].is_null());
    assert!(unknown["delta"].is_null());
    assert_eq!(unknown["after"]["a_to_b"]["packets"], 1);
}
