use super::{CaptureExcerptPlan, LocalCapture, SourceStamp, ip_protocol_label, summary::Traffic};
use crate::prompts::sanitize_single_line;
use anyhow::{Context, Result, ensure};
use serde::{Serialize, Serializer};
use std::{
    cmp::Reverse,
    collections::BTreeMap,
    fmt,
    io::{self, BufWriter, Write},
    net::{IpAddr, SocketAddr},
    num::NonZeroU128,
    ops::Range,
    path::PathBuf,
};

mod protocols;
mod rates;
mod timeline;
mod window;
use protocols::{ProtocolChange, TcpFlagComparison};
use rates::AverageRateComparison;
use timeline::TimelineComparison;
use window::ComparedWindow;

#[derive(Serialize)]
pub struct CaptureComparison {
    schema_version: u8,
    pub before: CaptureSide,
    pub after: CaptureSide,
    pub totals_delta: Option<CountsDelta>,
    pub average_rates: AverageRateComparison,
    pub timeline: Option<TimelineComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<ComparedWindow>,
    pub protocol_changes: Vec<ProtocolChange>,
    pub tcp_flag_comparison: Option<TcpFlagComparison>,
    pub conversation_changes: Vec<ConversationChange>,
    pub transport_changes: Vec<ConversationChange>,
    pub rate_only_changes: RateOnlyChanges,
    #[serde(skip)]
    source_stamps: [SourceStamp; 2],
}

#[derive(Serialize)]
pub struct RateOnlyChanges {
    pub ip_conversations: Vec<ConversationChange>,
    pub transport_conversations: Vec<ConversationChange>,
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
    pub packets_without_ports: u32,
    pub transport_untracked_packets: u32,
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
    #[serde(flatten)]
    pub endpoints: ConversationEndpoints,
    pub before: Option<DirectionalTraffic>,
    pub after: Option<DirectionalTraffic>,
    pub delta: Option<TrafficDelta>,
    pub average_rates: Option<AverageRateComparison>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(untagged)]
pub enum ConversationEndpoints {
    Ip {
        a: IpAddr,
        b: IpAddr,
    },
    Transport {
        protocol: u8,
        a: SocketAddr,
        b: SocketAddr,
    },
}

impl fmt::Display for ConversationEndpoints {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ip { a, b } => write!(f, "{a} ↔ {b}"),
            Self::Transport { protocol, a, b } => {
                write!(f, "{} · {a} ↔ {b}", ip_protocol_label(*protocol))
            }
        }
    }
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
        Self::from_scans([before, after], None)
    }

    pub fn compare_elapsed(paths: [PathBuf; 2], offsets: Range<i128>) -> Result<Self> {
        Self::scan_elapsed(paths, offsets, [None; 2])
    }

    pub fn prepare_excerpt(
        &self,
        source: usize,
        path: PathBuf,
        bounds: Range<i128>,
    ) -> Result<CaptureExcerptPlan> {
        let stamp = *self
            .source_stamps
            .get(source)
            .context("Choose the Before or After capture")?;
        CaptureExcerptPlan::absolute(path, bounds, stamp)
    }

    pub fn prepare_conversation_excerpt(
        &self,
        source: usize,
        path: PathBuf,
        endpoints: ConversationEndpoints,
        bounds: Option<Range<i128>>,
    ) -> Result<CaptureExcerptPlan> {
        let stamp = *self
            .source_stamps
            .get(source)
            .context("Choose the Before or After capture")?;
        CaptureExcerptPlan::conversation_absolute(path, endpoints, bounds, stamp)
    }

    pub fn rescan_elapsed(&self, paths: [PathBuf; 2], offsets: Range<i128>) -> Result<Self> {
        Self::scan_elapsed(paths, offsets, self.source_stamps.map(Some))
    }

    fn scan_elapsed(
        paths: [PathBuf; 2],
        offsets: Range<i128>,
        stamps: [Option<SourceStamp>; 2],
    ) -> Result<Self> {
        ensure!(
            offsets.start < offsets.end,
            "Choose a nonempty elapsed interval"
        );
        let [before_path, after_path] = paths;
        let before = LocalCapture::scan(before_path, Some(offsets.clone()), stamps[0])
            .context("Scanning the before capture")?;
        let after = LocalCapture::scan(after_path, Some(offsets.clone()), stamps[1])
            .context("Scanning the after capture")?;
        let sources = [&before, &after];
        let window = ComparedWindow::new(sources, offsets)?;
        Ok(Self::from_scans(sources, Some(window)))
    }

    fn from_scans(sources: [&LocalCapture; 2], window: Option<ComparedWindow>) -> Self {
        let [before, after] = sources.map(CaptureSide::from);
        let sides = [&before, &after];
        let scan_complete = sides.map(|side| side.complete);
        let comparable = window.as_ref().map_or(scan_complete, |window| {
            [&window.before, &window.after]
                .map(|side| side.span_covers_window && side.untimestamped_packets == 0)
        });
        let comparable = std::array::from_fn(|i| scan_complete[i] && comparable[i]);
        let complete = comparable == [true; 2];
        let (protocol_changes, tcp_flag_comparison) = protocols::compare(
            &sources[0].summary.ip_protocols,
            &sources[1].summary.ip_protocols,
            complete,
        );
        let timeline = TimelineComparison::new(
            sources.map(|source| source.summary.relative_timeline.as_ref()),
        );
        let totals_delta = complete.then(|| CountsDelta {
            packets: i64::from(after.packets) - i64::from(before.packets),
            captured_bytes: bytes_delta(before.captured_bytes, after.captured_bytes),
            truncated_packets: i64::from(after.truncated_packets)
                - i64::from(before.truncated_packets),
        });
        let rate_spans = window.as_ref().map_or_else(
            || sides.map(CaptureSide::average_rate_span),
            |window| {
                comparable.map(|covered| {
                    NonZeroU128::new(window.end_offset_ns.abs_diff(window.start_offset_ns))
                        .filter(|_| covered)
                })
            },
        );
        let average_rates = AverageRateComparison::new(
            sides.map(|side| {
                Some(Traffic {
                    packets: side.packets,
                    captured_bytes: side.captured_bytes,
                })
            }),
            rate_spans,
        );
        let (conversation_changes, ip_rate_only) = compare_pairs(
            sources.map(|source| {
                source.summary.conversations.pairs().map(|pair| {
                    (
                        ConversationEndpoints::Ip {
                            a: pair.a,
                            b: pair.b,
                        },
                        DirectionalTraffic {
                            a_to_b: pair.a_to_b,
                            b_to_a: pair.b_to_a,
                        },
                    )
                })
            }),
            std::array::from_fn(|i| comparable[i] && sides[i].untracked_packets == 0),
            complete,
            rate_spans,
        );
        let (transport_changes, transport_rate_only) = compare_pairs(
            sources.map(|source| {
                source.summary.transport_conversations.pairs().map(|pair| {
                    (
                        ConversationEndpoints::Transport {
                            protocol: pair.protocol,
                            a: pair.a,
                            b: pair.b,
                        },
                        DirectionalTraffic {
                            a_to_b: pair.a_to_b,
                            b_to_a: pair.b_to_a,
                        },
                    )
                })
            }),
            std::array::from_fn(|i| {
                comparable[i]
                    && sides[i].has_port_coverage()
                    && sides[i].transport_untracked_packets == 0
            }),
            complete && sides.into_iter().all(CaptureSide::has_port_coverage),
            std::array::from_fn(|i| rate_spans[i].filter(|_| sides[i].has_port_coverage())),
        );
        Self {
            schema_version: 1,
            before,
            after,
            totals_delta,
            average_rates,
            timeline,
            window,
            protocol_changes,
            tcp_flag_comparison,
            conversation_changes,
            transport_changes,
            rate_only_changes: RateOnlyChanges {
                ip_conversations: ip_rate_only,
                transport_conversations: transport_rate_only,
            },
            source_stamps: sources.map(|source| source.source_stamp),
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
        if let Some(window) = &self.window {
            writeln!(
                output,
                "Elapsed interval · {:+} to {:+} ns",
                window.start_offset_ns, window.end_offset_ns
            )?;
        }
        for (index, (role, side)) in [("Before", &self.before), ("After", &self.after)]
            .into_iter()
            .enumerate()
        {
            writeln!(
                output,
                "\n{role} · {} · {}",
                sanitize_single_line(&side.file_name, usize::MAX),
                side.format
            )?;
            if let Some(window) = &self.window {
                let side = [&window.before, &window.after][index];
                writeln!(
                    output,
                    "Unix interval · {} to {} ns",
                    side.start_unix_ns, side.end_unix_ns
                )?;
                writeln!(
                    output,
                    "{} · {} untimestamped packets in source",
                    side.coverage_label(),
                    side.untimestamped_packets
                )?;
            }
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
            } else if self.window.is_some() {
                writeln!(output, "No observed packets in interval")?;
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
            writeln!(
                output,
                "TCP/UDP packets without ports {} · Untracked TCP/UDP packets {}",
                side.packets_without_ports, side.transport_untracked_packets
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
        } else if self.window.is_some() {
            writeln!(
                output,
                "\nCapture deltas unavailable for incomplete interval coverage"
            )?;
        } else {
            writeln!(output, "\nCapture deltas unavailable for a partial scan")?;
        }
        writeln!(output, "\n{}", self.average_rates_label())?;
        for line in self.average_rates.report_lines() {
            writeln!(output, "{line}")?;
        }
        if let Some(timeline) = &self.timeline {
            timeline.write_report(&mut output)?;
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
        write_conversation_changes(
            &mut output,
            "IP conversation changes",
            &self.conversation_changes,
        )?;
        write_conversation_changes(
            &mut output,
            "TCP and UDP conversation changes",
            &self.transport_changes,
        )?;
        for (heading, changes) in [
            (
                "IP conversations with rate-only changes",
                &self.rate_only_changes.ip_conversations,
            ),
            (
                "TCP and UDP conversations with rate-only changes",
                &self.rate_only_changes.transport_conversations,
            ),
        ] {
            if !changes.is_empty() {
                write_conversation_changes(&mut output, heading, changes)?;
            }
        }
        output.flush()
    }

    pub fn average_rates_label(&self) -> &'static str {
        if self.window.is_some() {
            "Average rates over the selected elapsed interval"
        } else {
            "Average rates over each recorded timestamp span"
        }
    }
}

fn compare_pairs<T: IntoIterator<Item = (ConversationEndpoints, DirectionalTraffic)>>(
    sources: [T; 2],
    missing_is_zero: [bool; 2],
    deltas_available: bool,
    rate_spans: [Option<NonZeroU128>; 2],
) -> (Vec<ConversationChange>, Vec<ConversationChange>) {
    let mut pairs: BTreeMap<_, [Option<DirectionalTraffic>; 2]> = BTreeMap::new();
    for (index, source) in sources.into_iter().enumerate() {
        for (endpoints, traffic) in source {
            pairs.entry(endpoints).or_default()[index] = Some(traffic);
        }
    }
    let mut changes: Vec<_> = pairs
        .into_iter()
        .filter_map(|(endpoints, [before, after])| {
            let before = before.or_else(|| missing_is_zero[0].then(DirectionalTraffic::default));
            let after = after.or_else(|| missing_is_zero[1].then(DirectionalTraffic::default));
            let totals = [before, after].map(|traffic| traffic.map(DirectionalTraffic::total));
            let average_rates = AverageRateComparison::new(totals, rate_spans);
            if before == after && !average_rates.changed() {
                return None;
            }
            let delta = totals[0]
                .zip(totals[1])
                .filter(|_| deltas_available)
                .map(|(before, after)| TrafficDelta::between(before, after));
            Some(ConversationChange {
                endpoints,
                before,
                after,
                delta,
                average_rates: average_rates.has_observation().then_some(average_rates),
            })
        })
        .collect();
    changes.sort_unstable_by_key(|change| {
        let rank = change.delta.as_ref().map(|delta| {
            (
                delta.captured_bytes.unsigned_abs(),
                delta.packets.unsigned_abs(),
            )
        });
        (Reverse(rank), change.endpoints)
    });
    changes
        .into_iter()
        .partition(|change| change.before != change.after)
}

fn write_conversation_changes(
    mut output: impl Write,
    heading: &str,
    changes: &[ConversationChange],
) -> io::Result<()> {
    writeln!(output, "\n{heading} · {}", changes.len())?;
    for change in changes {
        writeln!(output, "\n{}", change.endpoints)?;
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
        if let Some(rates) = change.average_rates {
            for line in rates.report_lines() {
                writeln!(output, "{line}")?;
            }
        }
    }
    Ok(())
}

impl CaptureSide {
    fn average_rate_span(&self) -> Option<NonZeroU128> {
        if self.complete && self.timestamped_packets == self.packets {
            self.timestamp_span_ns.and_then(NonZeroU128::new)
        } else {
            None
        }
    }

    fn has_port_coverage(&self) -> bool {
        self.complete && self.packets_without_ports == 0 && self.unresolved_protocol_packets == 0
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
            packets_without_ports: summary.transport_conversations.packets_without_ports,
            transport_untracked_packets: summary.transport_conversations.untracked_packets,
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
