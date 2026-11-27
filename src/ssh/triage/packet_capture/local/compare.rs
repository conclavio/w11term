use super::{LocalCapture, summary::Traffic};
use crate::prompts::sanitize_single_line;
use serde::{Serialize, Serializer};
use std::{
    cmp::Reverse,
    collections::BTreeMap,
    io::{self, BufWriter, Write},
    net::IpAddr,
};

mod protocols;
use protocols::{ProtocolChange, TcpFlagComparison};

#[derive(Serialize)]
pub struct CaptureComparison {
    schema_version: u8,
    pub before: CaptureSide,
    pub after: CaptureSide,
    pub totals_delta: Option<CountsDelta>,
    pub protocol_changes: Vec<ProtocolChange>,
    pub tcp_flag_comparison: Option<TcpFlagComparison>,
    pub conversation_changes: Vec<ConversationChange>,
}

#[derive(Serialize)]
pub struct CaptureSide {
    pub file_name: String,
    pub format: &'static str,
    pub packets: u32,
    pub captured_bytes: u64,
    pub truncated_packets: u32,
    pub timestamped_packets: u32,
    #[serde(serialize_with = "serialize_span")]
    pub timestamp_span_ns: Option<u128>,
    pub unclassified_packets: u32,
    pub untracked_packets: u32,
    pub unresolved_protocol_packets: u32,
    pub fragmented_packets: u32,
    pub complete: bool,
    pub issue: Option<String>,
}

#[derive(Serialize)]
pub struct CountsDelta {
    pub packets: i64,
    pub captured_bytes: i64,
    pub truncated_packets: i64,
}

#[derive(Serialize)]
pub struct ConversationChange {
    pub a: IpAddr,
    pub b: IpAddr,
    pub before: Option<DirectionalTraffic>,
    pub after: Option<DirectionalTraffic>,
    pub delta: Option<TrafficDelta>,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DirectionalTraffic {
    pub a_to_b: Traffic,
    pub b_to_a: Traffic,
}

impl DirectionalTraffic {
    pub fn total(self) -> Traffic {
        Traffic {
            packets: self.a_to_b.packets + self.b_to_a.packets,
            captured_bytes: self.a_to_b.captured_bytes + self.b_to_a.captured_bytes,
        }
    }
}

#[derive(Serialize)]
pub struct TrafficDelta {
    pub packets: i64,
    pub captured_bytes: i64,
}

impl TrafficDelta {
    fn between(before: Traffic, after: Traffic) -> Self {
        Self {
            packets: i64::from(after.packets) - i64::from(before.packets),
            captured_bytes: bytes_delta(before.captured_bytes, after.captured_bytes),
        }
    }
}

impl CaptureComparison {
    pub fn new(before: &LocalCapture, after: &LocalCapture) -> Self {
        let sources = [before, after];
        let [before, after] = sources.map(CaptureSide::from);
        let complete = before.complete && after.complete;
        let (protocol_changes, tcp_flag_comparison) = protocols::compare(
            &sources[0].summary.ip_protocols,
            &sources[1].summary.ip_protocols,
            complete,
        );
        let totals_delta = complete.then(|| CountsDelta {
            packets: i64::from(after.packets) - i64::from(before.packets),
            captured_bytes: bytes_delta(before.captured_bytes, after.captured_bytes),
            truncated_packets: i64::from(after.truncated_packets)
                - i64::from(before.truncated_packets),
        });
        let missing_is_zero =
            [&before, &after].map(|side| side.complete && side.untracked_packets == 0);
        let mut pairs: BTreeMap<_, [Option<DirectionalTraffic>; 2]> = BTreeMap::new();
        for (index, source) in sources.into_iter().enumerate() {
            for pair in source.summary.conversations.pairs() {
                pairs.entry((pair.a, pair.b)).or_default()[index] = Some(DirectionalTraffic {
                    a_to_b: pair.a_to_b,
                    b_to_a: pair.b_to_a,
                });
            }
        }
        let mut conversation_changes: Vec<_> = pairs
            .into_iter()
            .filter_map(|((a, b), [before, after])| {
                let before =
                    before.or_else(|| missing_is_zero[0].then(DirectionalTraffic::default));
                let after = after.or_else(|| missing_is_zero[1].then(DirectionalTraffic::default));
                if before == after {
                    return None;
                }
                let delta = before
                    .zip(after)
                    .filter(|_| complete)
                    .map(|(before, after)| TrafficDelta::between(before.total(), after.total()));
                Some(ConversationChange {
                    a,
                    b,
                    before,
                    after,
                    delta,
                })
            })
            .collect();
        conversation_changes.sort_unstable_by_key(|change| {
            let rank = change.delta.as_ref().map(|delta| {
                (
                    delta.captured_bytes.unsigned_abs(),
                    delta.packets.unsigned_abs(),
                )
            });
            (Reverse(rank), change.a, change.b)
        });
        Self {
            schema_version: 1,
            before,
            after,
            totals_delta,
            protocol_changes,
            tcp_flag_comparison,
            conversation_changes,
        }
    }

    pub fn write_json(&self, output: impl Write) -> io::Result<()> {
        let mut output = BufWriter::new(output);
        serde_json::to_writer(&mut output, self).map_err(io::Error::other)?;
        output.write_all(b"\n")?;
        output.flush()
    }

    pub fn write_report(&self, output: impl Write) -> io::Result<()> {
        let mut output = BufWriter::new(output);
        writeln!(output, "Packet capture comparison")?;
        for (role, side) in [("Before", &self.before), ("After", &self.after)] {
            writeln!(
                output,
                "\n{role} · {} · {}",
                sanitize_single_line(&side.file_name, usize::MAX),
                side.format
            )?;
            writeln!(
                output,
                "Packets {} · {} captured bytes · {} truncated",
                side.packets, side.captured_bytes, side.truncated_packets
            )?;
            if let Some(span) = side.timestamp_span_ns {
                writeln!(
                    output,
                    "Timestamp span {span} ns · {} of {} packets timestamped",
                    side.timestamped_packets, side.packets
                )?;
            } else {
                writeln!(output, "No usable timestamps")?;
            }
            writeln!(
                output,
                "Unclassified packets {} · Untracked packets {}",
                side.unclassified_packets, side.untracked_packets
            )?;
            writeln!(
                output,
                "Unresolved IP protocols {} · Fragmented IP packets {}",
                side.unresolved_protocol_packets, side.fragmented_packets
            )?;
            if let Some(issue) = &side.issue {
                writeln!(
                    output,
                    "Partial scan · {}",
                    sanitize_single_line(issue, usize::MAX)
                )?;
            } else {
                writeln!(output, "Complete scan")?;
            }
        }
        if let Some(delta) = &self.totals_delta {
            writeln!(
                output,
                "\nCapture change · {:+} packets · {:+} captured bytes · {:+} truncated",
                delta.packets, delta.captured_bytes, delta.truncated_packets
            )?;
        } else {
            writeln!(output, "\nCapture deltas unavailable for a partial scan")?;
        }
        writeln!(
            output,
            "\nIP protocol changes · {}",
            self.protocol_changes.len()
        )?;
        for change in &self.protocol_changes {
            writeln!(output, "{}", change.summary())?;
        }
        if let Some(flags) = &self.tcp_flag_comparison {
            for line in flags.report_lines() {
                writeln!(output, "{line}")?;
            }
        }
        writeln!(
            output,
            "\nIP conversation changes · {}",
            self.conversation_changes.len()
        )?;
        for change in &self.conversation_changes {
            writeln!(output, "\n{} ↔ {}", change.a, change.b)?;
            for (role, traffic) in [("Before", change.before), ("After", change.after)] {
                if let Some(traffic) = traffic {
                    writeln!(
                        output,
                        "{role} · A to B {} packets, {} bytes · B to A {} packets, {} bytes",
                        traffic.a_to_b.packets,
                        traffic.a_to_b.captured_bytes,
                        traffic.b_to_a.packets,
                        traffic.b_to_a.captured_bytes
                    )?;
                } else {
                    writeln!(output, "{role} · Pair coverage unavailable")?;
                }
            }
            if let Some(delta) = &change.delta {
                writeln!(
                    output,
                    "Change · {:+} packets · {:+} captured bytes",
                    delta.packets, delta.captured_bytes
                )?;
            } else {
                writeln!(output, "Change · Unavailable")?;
            }
        }
        output.flush()
    }
}

impl From<&LocalCapture> for CaptureSide {
    fn from(source: &LocalCapture) -> Self {
        let summary = &source.summary;
        let stats = &summary.stats;
        let conversations = &summary.conversations;
        Self {
            file_name: source
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            format: source.format_label(),
            packets: stats.packets,
            captured_bytes: stats.captured_bytes,
            truncated_packets: stats.truncated_packets,
            timestamped_packets: stats.timestamped_packets,
            timestamp_span_ns: stats.span_ns(),
            unclassified_packets: conversations.unclassified_packets,
            untracked_packets: conversations.untracked_packets,
            unresolved_protocol_packets: summary.ip_protocols.unresolved_protocol_packets,
            fragmented_packets: summary.ip_protocols.fragmented_packets,
            complete: summary.issue.is_none(),
            issue: summary.issue.clone(),
        }
    }
}

fn bytes_delta(before: u64, after: u64) -> i64 {
    // Counts are bounded by one million records of at most u32::MAX bytes each.
    after as i64 - before as i64
}

fn serialize_span<S: Serializer>(span: &Option<u128>, serializer: S) -> Result<S::Ok, S::Error> {
    span.map(|ns| ns.to_string()).serialize(serializer)
}
