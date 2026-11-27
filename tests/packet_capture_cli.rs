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

fn clip(directory: &Path, input: &str, elapsed: &str, destination: &str) -> Output {
    clip_with(directory, input, destination, &["--elapsed-ns", elapsed])
}

fn clip_with(directory: &Path, input: &str, destination: &str, options: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
        .current_dir(directory)
        .env_remove("LOCALAPPDATA")
        .args(["clip-packet-capture", input, "--output-file", destination])
        .args(options)
        .output()
        .unwrap()
}

fn raw_ip_capture(pairs: impl IntoIterator<Item = (u32, u32)>) -> Vec<u8> {
    raw_capture(pairs.into_iter().map(|(a, b)| {
        let mut packet = [0_u8; 20];
        packet[..12].copy_from_slice(&[0x45, 0, 0, 20, 0, 0, 0, 0, 64, 253, 0, 0]);
        packet[12..16].copy_from_slice(&a.to_be_bytes());
        packet[16..20].copy_from_slice(&b.to_be_bytes());
        packet
    }))
}

fn raw_capture(packets: impl IntoIterator<Item = impl AsRef<[u8]>>) -> Vec<u8> {
    raw_capture_with_spacing(packets, 1)
}

fn raw_capture_with_spacing(
    packets: impl IntoIterator<Item = impl AsRef<[u8]>>,
    spacing_seconds: u32,
) -> Vec<u8> {
    raw_capture_at(
        packets
            .into_iter()
            .enumerate()
            .map(|(index, packet)| (10 + index as u32 * spacing_seconds, 0, packet)),
    )
}

fn raw_capture_at(packets: impl IntoIterator<Item = (u32, u32, impl AsRef<[u8]>)>) -> Vec<u8> {
    let mut bytes = classic_capture()[..24].to_vec();
    bytes[20] = 101;
    for (seconds, micros, packet) in packets {
        let packet = packet.as_ref();
        let size = packet.len() as u32;
        bytes.extend(
            [seconds, micros, size, size]
                .into_iter()
                .flat_map(u32::to_le_bytes),
        );
        bytes.extend(packet);
    }
    bytes
}

fn raw_transport_packet(protocol: u8, port: u16, reverse: bool) -> Vec<u8> {
    let mut packet = vec![0_u8; if protocol == 6 { 40 } else { 28 }];
    packet[0] = 0x45;
    packet[3] = packet.len() as u8;
    packet[9] = protocol;
    let mut endpoints = [([192, 0, 2, 1], port), ([192, 0, 2, 2], 443_u16)];
    if reverse {
        endpoints.swap(0, 1);
    }
    for (index, (address, port)) in endpoints.into_iter().enumerate() {
        let address_offset = 12 + index * 4;
        packet[address_offset..address_offset + 4].copy_from_slice(&address);
        let port_offset = 20 + index * 2;
        packet[port_offset..port_offset + 2].copy_from_slice(&port.to_be_bytes());
    }
    if protocol == 6 {
        packet[32] = 0x50;
    } else {
        packet[25] = 8;
    }
    packet
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
            "ip_protocols": {
                "by_number": {},
                "unclassified_packets": 2,
                "unresolved_protocol_packets": 0,
                "fragmented_packets": 0,
                "tcp_flags": {"decoded_headers": 0, "syn_without_ack": 0, "syn_ack": 0, "rst": 0, "fin": 0}
            },
            "transport_conversations": {
                "tracked_pairs": 0,
                "packets_without_ports": 0,
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
fn lists_and_filters_ranked_conversations_beyond_the_top_ten() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let file = "many conversations.pcap";
    let mut tcp = raw_transport_packet(6, 49116, false);
    tcp[19] = 16;
    let frames = (2..=16)
        .map(|host| {
            let mut frame = raw_transport_packet(17, 49100 + u16::from(host), false);
            frame[19] = host;
            frame
        })
        .chain([tcp]);
    fs::write(root.join(file), raw_capture(frames)).unwrap();

    let default = inspect(root, file, &[]);
    assert!(default.status.success(), "{default:?}");
    let default: Value = serde_json::from_slice(&default.stdout).unwrap();
    assert!(default.get("conversation_results").is_none());
    assert_eq!(
        default["ip_conversations"]["top"].as_array().unwrap().len(),
        10
    );
    assert_eq!(
        default["transport_conversations"]["top"]
            .as_array()
            .unwrap()
            .len(),
        10
    );

    let all = inspect(root, file, &["--all-conversations"]);
    assert!(all.status.success(), "{all:?}");
    let mut all: Value = serde_json::from_slice(&all.stdout).unwrap();
    let results = all
        .as_object_mut()
        .unwrap()
        .remove("conversation_results")
        .unwrap();
    assert_eq!(all, default);
    let ip = results["ip"].as_array().unwrap();
    let transport = results["transport"].as_array().unwrap();
    assert_eq!(ip.len(), 15);
    assert_eq!(transport.len(), 16);
    assert_eq!(
        &ip[..10],
        default["ip_conversations"]["top"].as_array().unwrap()
    );
    assert_eq!(
        &transport[..10],
        default["transport_conversations"]["top"]
            .as_array()
            .unwrap()
    );
    let udp = transport.last().unwrap();
    assert_eq!(udp["protocol"], 17);
    assert_eq!(udp["a"], "192.0.2.1:49116");
    assert_eq!(udp["b"], "192.0.2.16:443");

    let query = "UDP 192.0.2.16/32 :49116";
    let filtered = inspect(root, file, &["--filter-conversations", query]);
    assert!(filtered.status.success(), "{filtered:?}");
    let result: Value = serde_json::from_slice(&filtered.stdout).unwrap();
    assert_eq!(
        result["conversation_results"],
        json!({"ip": [], "transport": [udp]})
    );
    let output = inspect(
        root,
        file,
        &[
            "--filter-conversations",
            query,
            "--output-file",
            "matches.json",
        ],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read(root.join("matches.json")).unwrap(),
        filtered.stdout
    );

    let address = inspect(root, file, &["--filter-conversations", "192.0.2.16"]);
    assert!(address.status.success(), "{address:?}");
    let report: Value = serde_json::from_slice(&address.stdout).unwrap();
    assert_eq!(
        report["conversation_results"]["ip"][0]["a_to_b"]["packets"],
        2
    );
    assert_eq!(
        report["conversation_results"]["transport"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let empty = inspect(root, file, &["--filter-conversations", "203.0.113.0/24"]);
    assert!(empty.status.success(), "{empty:?}");
    let empty: Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert_eq!(
        empty["conversation_results"],
        json!({"ip": [], "transport": []})
    );

    let conflicting = inspect(
        root,
        file,
        &["--all-conversations", "--filter-conversations", query],
    );
    assert!(!conflicting.status.success());
    assert!(conflicting.stdout.is_empty());
}

#[test]
fn saves_exact_classic_records_inside_the_elapsed_bounds_without_replacing_files() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let source = classic_capture();
    fs::write(root.join("original.pcap"), &source).unwrap();
    let output = clip(
        root,
        "original.pcap",
        "1250000000:1250000001",
        "excerpt.pcap",
    );
    assert!(output.status.success(), "{output:?}");
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["result"]["packets"], 1);
    let expected = [&source[..24], &source[44..]].concat();
    assert_eq!(fs::read(root.join("excerpt.pcap")).unwrap(), expected);
    let output = inspect(root, "excerpt.pcap", &[]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["complete"], true);
    assert_eq!(report["truncated_packets"], 1);
    assert_eq!(report["captured_bytes"], 2);

    let duplicate = clip(root, "original.pcap", "0:1", "excerpt.pcap");
    assert!(!duplicate.status.success());
    assert_eq!(fs::read(root.join("excerpt.pcap")).unwrap(), expected);
    let empty = clip(root, "original.pcap", "1:1250000000", "empty.pcap");
    assert!(!empty.status.success());
    assert!(!root.join("empty.pcap").exists());
    fs::write(root.join("partial.pcap"), &source[..source.len() - 1]).unwrap();
    let partial = clip(root, "partial.pcap", "0:1", "failed.pcap");
    assert!(!partial.status.success());
    assert!(!root.join("failed.pcap").exists());
    assert_eq!(fs::read(root.join("original.pcap")).unwrap(), source);
}

#[test]
fn saves_bidirectional_conversations_and_uses_the_source_clock_for_combined_filters() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut unrelated = raw_transport_packet(17, 49152, false);
    unrelated[19] = 3;
    let forward = raw_transport_packet(17, 49152, false);
    let reverse = raw_transport_packet(17, 49152, true);
    let tcp = raw_transport_packet(6, 49152, false);
    let other_port = raw_transport_packet(17, 49153, false);
    let src = raw_capture_at([
        (10, 0, &unrelated),
        (11, 0, &forward),
        (11, 500_000, &tcp),
        (12, 0, &reverse),
        (12, 500_000, &other_port),
        (13, 1_000_000, &forward),
    ]);
    fs::write(root.join("source.pcap"), src).unwrap();
    let pair = ["--udp-pair", "192.0.2.2:443,192.0.2.1:49152"];
    let output = clip_with(root, "source.pcap", "pair.pcap", &pair);
    assert!(output.status.success(), "{output:?}");
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["result"]["packets"], 3);
    assert_eq!(
        fs::read(root.join("pair.pcap")).unwrap(),
        raw_capture_at([
            (11, 0, &forward),
            (12, 0, &reverse),
            (13, 1_000_000, &forward),
        ])
    );
    let output = clip_with(
        root,
        "source.pcap",
        "window.pcap",
        &[pair[0], pair[1], "--elapsed-ns", "1000000000:2000000000"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read(root.join("window.pcap")).unwrap(),
        raw_capture_at([(11, 0, &forward)])
    );

    let output = clip_with(
        root,
        "source.pcap",
        "ip.pcap",
        &["--ip-pair", "192.0.2.1,192.0.2.2"],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read(root.join("ip.pcap")).unwrap(),
        raw_capture_at([
            (11, 0, &forward),
            (11, 500_000, &tcp),
            (12, 0, &reverse),
            (12, 500_000, &other_port),
            (13, 1_000_000, &forward),
        ])
    );

    let empty = clip_with(
        root,
        "source.pcap",
        "empty.pcap",
        &["--tcp-pair", pair[1], "--elapsed-ns", "0:1"],
    );
    assert!(!empty.status.success());
    assert!(!root.join("empty.pcap").exists());
    let invalid = clip_with(
        root,
        "source.pcap",
        "invalid.pcap",
        &["--ip-pair", "192.0.2.1,::1"],
    );
    assert!(!invalid.status.success());
    assert!(!root.join("invalid.pcap").exists());
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
    let text = compare(root, before, after, &["--format", "text"]);
    assert!(text.status.success(), "{text:?}");
    let text = std::str::from_utf8(&text.stdout).unwrap();
    assert!(text.contains("Before · before.pcap · PCAP"));
    assert!(text.contains("After · after.pcap · PCAP"));
    assert!(text.contains("Timestamp span 11000000000 ns · 12 of 12 packets timestamped"));
    assert!(text.contains("Capture change · +1 packets · +20 captured bytes · +0 truncated"));
    assert!(text.contains("IP conversation changes · 2"));
    assert!(text.contains(
        "0.0.0.0 ↔ 0.0.0.12\nBefore · A to B 1 packets, 20 bytes · B to A 0 packets, 0 bytes"
    ));
    assert!(!text.contains(root.to_string_lossy().as_ref()));
    let collision = compare(
        root,
        before,
        after,
        &["--format", "text", "--output-file", "comparison.json"],
    );
    assert!(!collision.status.success());
    assert_eq!(
        fs::read(root.join("comparison.json")).unwrap(),
        output.stdout
    );
    let invalid = compare(root, before, after, &["--format", "csv"]);
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("--format requires json or text"));
    after_bytes.pop();
    fs::write(root.join(after), after_bytes).unwrap();
    let partial = compare(root, before, after, &[]);
    assert!(partial.status.success(), "{partial:?}");
    let report: Value = serde_json::from_slice(&partial.stdout).unwrap();
    assert_eq!(report["after"]["complete"], false);
    assert!(report["totals_delta"].is_null());
    assert_eq!(report["conversation_changes"][0]["b"], "0.0.0.12");
    assert!(report["conversation_changes"][0]["delta"].is_null());
    let text = compare(root, before, after, &["--format", "text"]);
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Partial scan · The pcap packet is incomplete or invalid"));
    assert!(text.contains("Capture deltas unavailable for a partial scan"));
    assert!(text.contains("Change · Unavailable"));
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
    let text = compare(root, before, after, &["--format", "text"]);
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("IP conversation changes · 4097"));
    assert!(text.contains("0.0.0.0 ↔ 0.0.16.1\nBefore · Pair coverage unavailable"));
}

#[test]
fn compares_transport_pairs_beyond_the_ranked_summary_and_keeps_protocols_distinct() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = "baseline.pcap";
    let after = "current.pcap";
    fs::write(
        root.join(before),
        raw_capture((1001..=1012).map(|port| raw_transport_packet(17, port, false))),
    )
    .unwrap();
    fs::write(
        root.join(after),
        raw_capture(
            (1001..=1012)
                .map(|port| raw_transport_packet(17, port, port == 1012))
                .chain([raw_transport_packet(6, 1012, false)]),
        ),
    )
    .unwrap();

    let output = compare(root, before, after, &[]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let changes = report["transport_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    let tcp = &changes[0];
    let udp = &changes[1];
    assert_eq!(tcp["protocol"], 6);
    assert_eq!(udp["protocol"], 17);
    assert_eq!(tcp["a"], "192.0.2.1:1012");
    assert_eq!(tcp["a"], udp["a"]);
    assert_eq!(tcp["b"], "192.0.2.2:443");
    assert_eq!(tcp["b"], udp["b"]);
    assert_eq!(tcp["before"]["a_to_b"]["packets"], 0);
    assert_eq!(tcp["delta"], json!({"packets": 1, "captured_bytes": 40}));
    assert_eq!(udp["before"]["a_to_b"]["packets"], 1);
    assert_eq!(udp["after"]["a_to_b"]["packets"], 0);
    assert_eq!(udp["after"]["b_to_a"]["packets"], 1);
    assert_eq!(udp["delta"], json!({"packets": 0, "captured_bytes": 0}));
    assert_eq!(report["conversation_changes"][0]["a"], "192.0.2.1");
    assert!(report["conversation_changes"][0].get("protocol").is_none());

    let text = compare(root, before, after, &["--format", "text"]);
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("TCP and UDP conversation changes · 2"));
    assert!(text.contains("TCP · 192.0.2.1:1012 ↔ 192.0.2.2:443"));
    assert!(text.contains(
        "UDP · 192.0.2.1:1012 ↔ 192.0.2.2:443\nBefore · A to B 1 packets, 28 bytes · B to A 0 packets, 0 bytes\nAfter · A to B 0 packets, 0 bytes · B to A 1 packets, 28 bytes"
    ));
}

#[test]
fn aligns_packet_timelines_with_distinct_origins_and_preserves_empty_or_uncovered_intervals() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = "baseline.pcap";
    let after = "current.pcap";
    let frame = raw_transport_packet(17, 1012, false);
    let capture_at = |times: [(u32, u32); 3]| {
        raw_capture_at(times.map(|(seconds, micros)| (seconds, micros, &frame)))
    };
    fs::write(
        root.join(before),
        capture_at([(1100, 0), (1099, 999_999), (1101, 0)]),
    )
    .unwrap();
    fs::write(
        root.join(after),
        capture_at([(2100, 320), (2100, 321), (2102, 320)]),
    )
    .unwrap();
    let output = compare(root, before, after, &[]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let timeline = &report["timeline"];
    assert_eq!(timeline["before_origin_unix_ns"], "1100000000000");
    assert_eq!(timeline["after_origin_unix_ns"], "2100000320000");
    assert_eq!(timeline["start_offset_ns"], "-32768000");
    assert_eq!(timeline["bucket_width_ns"], "32768000");
    let bins = timeline["buckets"].as_array().unwrap();
    assert_eq!(bins.len(), 63);
    let bucket = |packets| json!({"packets": packets, "captured_bytes": packets * 28});
    assert_eq!(bins[0], json!({"before": bucket(1), "after": null}));
    assert_eq!(bins[1], json!({"before": bucket(1), "after": bucket(2)}));
    assert_eq!(bins[2], json!({"before": bucket(0), "after": bucket(0)}));
    assert_eq!(bins[31], json!({"before": bucket(1), "after": bucket(0)}));
    assert_eq!(bins[32], json!({"before": null, "after": bucket(0)}));
    assert_eq!(bins[62], json!({"before": null, "after": bucket(1)}));
    for side in ["before", "after"] {
        assert_eq!(
            bins.iter()
                .filter_map(|bin| bin[side]["packets"].as_u64())
                .sum::<u64>(),
            3
        );
        assert_eq!(
            bins.iter()
                .filter_map(|bin| bin[side]["captured_bytes"].as_u64())
                .sum::<u64>(),
            84
        );
    }

    let text = compare(root, before, after, &["--format", "text"]);
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Elapsed packet timeline · 32768000 ns per interval"));
    assert!(text.contains(
        "-32768000 ns · Before 1 packets, 28 captured bytes · After outside timestamped coverage"
    ));
    assert!(text.contains(
        "+0 ns · Before 1 packets, 28 captured bytes · After 2 packets, 56 captured bytes"
    ));

    let untimed = "untimed.pcap";
    fs::write(
        root.join(untimed),
        raw_capture_at([(2100, 1_000_000, &frame)]),
    )
    .unwrap();
    let output = compare(root, before, untimed, &[]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["after"]["packets"], 1);
    assert_eq!(report["after"]["timestamped_packets"], 0);
    assert!(report["timeline"]["after_origin_unix_ns"].is_null());
    assert!(
        report["timeline"]["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|bin| bin["after"].is_null())
    );
}

#[test]
fn compares_average_rates_over_the_full_window_and_reports_rate_only_conversations() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = "baseline.pcap";
    let after = "current.pcap";
    let flows = || [1012, 1012, 1013].map(|port| raw_transport_packet(17, port, false));
    fs::write(root.join(before), raw_capture(flows())).unwrap();
    let read_report = |spacing| {
        fs::write(root.join(after), raw_capture_with_spacing(flows(), spacing)).unwrap();
        let output = compare(root, before, after, &[]);
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let report = read_report(2);
    assert_eq!(report["totals_delta"]["packets"], 0);
    assert_eq!(
        report["average_rates"],
        json!({
            "before": {"packets_per_second": 1.5, "captured_bytes_per_second": 42.0},
            "after": {"packets_per_second": 0.75, "captured_bytes_per_second": 21.0},
            "delta": {"packets_per_second": -0.75, "captured_bytes_per_second": -21.0}
        })
    );
    assert_eq!(report["conversation_changes"], json!([]));
    assert_eq!(report["transport_changes"], json!([]));
    let ip = &report["rate_only_changes"]["ip_conversations"][0];
    assert_eq!(ip["average_rates"], report["average_rates"]);
    let pairs = report["rate_only_changes"]["transport_conversations"]
        .as_array()
        .unwrap();
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0]["a"], "192.0.2.1:1012");
    assert_eq!(
        pairs[0]["average_rates"]["delta"],
        json!({"packets_per_second": -0.5, "captured_bytes_per_second": -14.0})
    );
    let text = compare(root, before, after, &["--format", "text"]);
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Average change · -0.750 packets/s, -21.000 captured bytes/s"));
    assert!(text.contains("TCP and UDP conversations with rate-only changes · 2"));

    let same = read_report(1);
    assert_eq!(same["rate_only_changes"]["ip_conversations"], json!([]));
    assert_eq!(
        same["rate_only_changes"]["transport_conversations"],
        json!([])
    );
    let zero_span = read_report(0);
    assert_eq!(zero_span["after"]["timestamp_span_ns"], "0");
    assert_eq!(
        zero_span["average_rates"]["before"]["packets_per_second"],
        1.5
    );
    assert!(zero_span["average_rates"]["after"].is_null());
    assert!(zero_span["average_rates"]["delta"].is_null());
    assert_eq!(
        zero_span["rate_only_changes"]["transport_conversations"],
        json!([])
    );
}

#[test]
fn compares_only_packets_in_the_half_open_elapsed_window() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = "baseline.pcap";
    let after = "current.pcap";
    let udp = raw_transport_packet(17, 1012, false);
    let tcp = raw_transport_packet(6, 1012, false);
    let other = raw_transport_packet(17, 64200, false);
    fs::write(
        root.join(before),
        raw_capture_at([(11, 0, &other), (12, 0, &udp), (13, 0, &other)]),
    )
    .unwrap();
    let mut frames = vec![
        (21, 5, &other),
        (22, 5, &udp),
        (22, 10, &tcp),
        (23, 5, &other),
    ];
    fs::write(root.join(after), raw_capture_at(frames.iter().copied())).unwrap();
    let options = ["--elapsed-ns", "1000000000:2000000000"];
    let report_for = |options: &[&str]| {
        let output = compare(root, before, after, options);
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let report = report_for(&options);
    assert_eq!(report["window"]["start_offset_ns"], "1000000000");
    assert_eq!(report["window"]["end_offset_ns"], "2000000000");
    assert_eq!(report["window"]["before"]["start_unix_ns"], "12000000000");
    assert_eq!(report["window"]["before"]["end_unix_ns"], "13000000000");
    assert_eq!(report["window"]["after"]["start_unix_ns"], "22000005000");
    assert_eq!(report["window"]["after"]["span_covers_window"], true);
    assert_eq!(report["before"]["packets"], 1);
    assert_eq!(report["after"]["packets"], 2);
    assert_eq!(report["totals_delta"]["packets"], 1);
    assert_eq!(
        report["average_rates"]["after"]["captured_bytes_per_second"],
        68.0
    );
    assert_eq!(report["protocol_changes"][0]["number"], 6);
    let changes = report["transport_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["protocol"], 6);
    assert_eq!(changes[0]["a"], "192.0.2.1:1012");
    assert_eq!(changes[0]["delta"]["captured_bytes"], 40);
    assert_eq!(
        changes[0]["average_rates"]["before"]["packets_per_second"],
        0.0
    );

    let text = compare(
        root,
        before,
        after,
        &["--elapsed-ns", "1000000000:2000000000", "--format", "text"],
    );
    assert!(text.status.success(), "{text:?}");
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Elapsed interval · +1000000000 to +2000000000 ns"));
    assert!(text.contains("Average rates over the selected elapsed interval"));

    let uncovered = report_for(&["--elapsed-ns", "1000000000:3000000000"]);
    assert_eq!(uncovered["window"]["after"]["span_covers_window"], false);
    assert!(uncovered["totals_delta"].is_null());
    assert!(uncovered["transport_changes"][0]["delta"].is_null());

    frames.push((24, 1_000_000, &tcp));
    fs::write(root.join(after), raw_capture_at(frames)).unwrap();
    let untimed = report_for(&options);
    assert_eq!(untimed["window"]["after"]["untimestamped_packets"], 1);
    assert_eq!(untimed["after"]["packets"], 2);
    assert!(untimed["totals_delta"].is_null());
    assert!(untimed["average_rates"]["after"].is_null());
    assert!(untimed["transport_changes"][0]["delta"].is_null());
    for range in [
        "1:1",
        "1",
        "1:2:3",
        "0:170141183460469231731687303715884105727",
    ] {
        let invalid = compare(root, before, after, &["--elapsed-ns", range]);
        assert!(!invalid.status.success(), "{range}: {invalid:?}");
        assert!(invalid.stdout.is_empty());
    }
}

#[test]
fn tracking_limit_keeps_observed_transport_deltas_but_cannot_prove_missing_pairs() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = "many.pcap";
    let after = "two.pcap";
    fs::write(
        root.join(before),
        raw_capture((1..=4097).map(|port| raw_transport_packet(17, port, false))),
    )
    .unwrap();
    fs::write(
        root.join(after),
        raw_capture([
            raw_transport_packet(17, 1, true),
            raw_transport_packet(17, 4097, false),
        ]),
    )
    .unwrap();
    let output = compare(root, before, after, &[]);
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["before"]["transport_untracked_packets"], 1);
    let changes = report["transport_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 4097);
    let tracked = &changes[4095];
    assert_eq!(tracked["a"], "192.0.2.1:1");
    assert_eq!(tracked["delta"], json!({"packets": 0, "captured_bytes": 0}));
    let untracked = changes.last().unwrap();
    assert_eq!(untracked["a"], "192.0.2.1:4097");
    assert!(untracked["before"].is_null());
    assert!(untracked["delta"].is_null());
    assert_eq!(untracked["after"]["a_to_b"]["packets"], 1);
}

#[test]
fn compares_protocols_and_withholds_tcp_flag_deltas_when_headers_are_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let before = "before.pcap";
    let after = "after.pcap";
    let tcp = |flags| {
        let mut packet = [0_u8; 40];
        packet[0] = 0x45;
        packet[3] = 40;
        packet[9] = 6;
        packet[32] = 0x50;
        packet[33] = flags;
        packet
    };
    let mut udp = [0_u8; 28];
    udp[..4].copy_from_slice(&[0x45, 0, 0, 28]);
    udp[9] = 17;
    udp[25] = 8;
    fs::write(root.join(before), raw_capture([tcp(0x02), tcp(0x12)])).unwrap();
    let mut packets: Vec<_> = [tcp(0x02), tcp(0x04), tcp(0x04)].map(Vec::from).into();
    packets.push(udp.to_vec());
    let complete = raw_capture(&packets);
    fs::write(root.join(after), &complete).unwrap();
    let inspect = || {
        let output = compare(root, before, after, &[]);
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let report = inspect();
    assert_eq!(
        report["protocol_changes"],
        json!([
            {"number": 6, "before": {"packets": 2, "captured_bytes": 80},
             "after": {"packets": 3, "captured_bytes": 120}, "delta": {"packets": 1, "captured_bytes": 40}},
            {"number": 17, "before": {"packets": 0, "captured_bytes": 0},
             "after": {"packets": 1, "captured_bytes": 28}, "delta": {"packets": 1, "captured_bytes": 28}}
        ])
    );
    assert_eq!(
        report["tcp_flag_comparison"]["delta"],
        json!({
            "syn_without_ack": 0, "syn_ack": -1, "rst": 2, "fin": 0
        })
    );
    let text = compare(root, before, after, &["--format", "text"]);
    assert!(text.status.success(), "{text:?}");
    assert!(
        String::from_utf8(text.stdout)
            .unwrap()
            .contains("RST · Before 0 · After 2 · Change +2")
    );

    packets[2][7] = 1; // The second reset is in a later fragment.
    fs::write(root.join(after), raw_capture(&packets)).unwrap();
    let report = inspect();
    assert_eq!(report["after"]["fragmented_packets"], 1);
    assert_eq!(report["tcp_flag_comparison"]["after"]["decoded_headers"], 2);
    assert_eq!(report["tcp_flag_comparison"]["after"]["rst"], 1);
    assert!(report["tcp_flag_comparison"]["delta"].is_null());
    assert_eq!(report["protocol_changes"][0]["delta"]["captured_bytes"], 40);
    assert_eq!(report["after"]["packets_without_ports"], 1);
    let udp = &report["transport_changes"][0];
    assert_eq!(udp["protocol"], 17);
    assert_eq!(udp["after"]["a_to_b"]["packets"], 1);
    assert!(udp["delta"].is_null());
    assert_eq!(udp["average_rates"]["before"]["packets_per_second"], 0.0);
    assert!(udp["average_rates"]["after"].is_null());
    assert!(udp["average_rates"]["delta"].is_null());
    assert!(report["average_rates"]["after"].is_object());

    packets[2][7] = 0;
    let mut unresolved_v6 = vec![0_u8; 40];
    unresolved_v6[0] = 0x60;
    unresolved_v6[5] = 8;
    unresolved_v6[6] = 60;
    packets.push(unresolved_v6);
    fs::write(root.join(after), raw_capture(&packets)).unwrap();
    let report = inspect();
    assert_eq!(report["after"]["unresolved_protocol_packets"], 1);
    assert_eq!(report["tcp_flag_comparison"]["after"]["decoded_headers"], 3);
    assert!(report["tcp_flag_comparison"]["delta"].is_null());
    assert_eq!(report["after"]["packets_without_ports"], 0);
    assert!(report["transport_changes"][0]["delta"].is_null());

    fs::write(root.join(after), &complete[..complete.len() - 1]).unwrap();
    let report = inspect();
    assert_eq!(report["after"]["complete"], false);
    assert_eq!(report["protocol_changes"][0]["before"]["packets"], 2);
    assert_eq!(report["protocol_changes"][0]["after"]["packets"], 3);
    assert!(report["protocol_changes"][0]["delta"].is_null());
    assert!(report["tcp_flag_comparison"]["delta"].is_null());
}
