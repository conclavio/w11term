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
