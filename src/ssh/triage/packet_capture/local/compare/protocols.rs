use super::{
    super::summary::{IpProtocols, TcpFlags, Traffic, ip_protocol_label},
    TrafficDelta,
};
use serde::Serialize;
use std::{cmp::Reverse, collections::BTreeSet};

#[derive(Serialize)]
pub struct ProtocolChange {
    pub number: u8,
    pub before: Traffic,
    pub after: Traffic,
    pub delta: Option<TrafficDelta>,
}

impl ProtocolChange {
    pub fn summary(&self) -> String {
        let mut summary = format!(
            "{} · {} → {} packets · {} → {} captured bytes",
            ip_protocol_label(self.number),
            self.before.packets,
            self.after.packets,
            self.before.captured_bytes,
            self.after.captured_bytes,
        );
        if let Some(delta) = &self.delta {
            use std::fmt::Write as _;
            let _ = write!(
                summary,
                " · Change {:+} packets, {:+} bytes",
                delta.packets, delta.captured_bytes
            );
        }
        summary
    }
}

#[derive(Serialize)]
pub struct TcpFlagComparison {
    pub before: TcpFlagSide,
    pub after: TcpFlagSide,
    pub delta: Option<TcpFlagDelta>,
}

#[derive(Serialize)]
pub struct TcpFlagSide {
    pub tcp_packets: u32,
    #[serde(flatten)]
    pub flags: TcpFlags,
}

#[derive(Serialize)]
pub struct TcpFlagDelta {
    pub syn_without_ack: i64,
    pub syn_ack: i64,
    pub rst: i64,
    pub fin: i64,
}

impl TcpFlagComparison {
    pub fn report_lines(&self) -> Vec<String> {
        let before = self.before.flags;
        let after = self.after.flags;
        let delta = self.delta.as_ref();
        let mut lines = vec![format!(
            "TCP headers decoded · Before {} of {} · After {} of {}",
            before.decoded_headers,
            self.before.tcp_packets,
            after.decoded_headers,
            self.after.tcp_packets,
        )];
        lines.extend(
            [
                (
                    "SYN without ACK",
                    before.syn_without_ack,
                    after.syn_without_ack,
                    delta.map(|d| d.syn_without_ack),
                ),
                (
                    "SYN-ACK",
                    before.syn_ack,
                    after.syn_ack,
                    delta.map(|d| d.syn_ack),
                ),
                ("RST", before.rst, after.rst, delta.map(|d| d.rst)),
                ("FIN", before.fin, after.fin, delta.map(|d| d.fin)),
            ]
            .map(|(name, before, after, delta)| {
                if let Some(delta) = delta {
                    format!("{name} · Before {before} · After {after} · Change {delta:+}")
                } else {
                    format!("{name} · Before {before} · After {after}")
                }
            }),
        );
        if delta.is_none() {
            lines.push("TCP flag deltas unavailable for this coverage".into());
        }
        lines
    }
}

pub(super) fn compare(
    before: &IpProtocols,
    after: &IpProtocols,
    complete: bool,
) -> (Vec<ProtocolChange>, Option<TcpFlagComparison>) {
    let numbers: BTreeSet<_> = before
        .by_number
        .keys()
        .chain(after.by_number.keys())
        .copied()
        .collect();
    let mut changes = Vec::new();
    for number in numbers {
        let before = before.by_number.get(&number).copied().unwrap_or_default();
        let after = after.by_number.get(&number).copied().unwrap_or_default();
        if before != after {
            changes.push(ProtocolChange {
                number,
                before,
                after,
                delta: complete.then(|| TrafficDelta::between(before, after)),
            });
        }
    }
    changes.sort_unstable_by_key(|change| {
        (
            Reverse(
                change
                    .delta
                    .as_ref()
                    .map(|delta| delta.captured_bytes.unsigned_abs()),
            ),
            change.number,
        )
    });
    let [first, second] = [before, after].map(|source| TcpFlagSide {
        tcp_packets: source.tcp_packets(),
        flags: source.tcp_flags,
    });
    if first.tcp_packets == 0 && second.tcp_packets == 0 {
        return (changes, None);
    }
    let comparable = complete
        && before.unresolved_protocol_packets == 0
        && after.unresolved_protocol_packets == 0
        && first.flags.decoded_headers == first.tcp_packets
        && second.flags.decoded_headers == second.tcp_packets;
    let delta = comparable.then(|| {
        let difference = |before, after| i64::from(after) - i64::from(before);
        TcpFlagDelta {
            syn_without_ack: difference(first.flags.syn_without_ack, second.flags.syn_without_ack),
            syn_ack: difference(first.flags.syn_ack, second.flags.syn_ack),
            rst: difference(first.flags.rst, second.flags.rst),
            fin: difference(first.flags.fin, second.flags.fin),
        }
    });
    (
        changes,
        Some(TcpFlagComparison {
            before: first,
            after: second,
            delta,
        }),
    )
}
