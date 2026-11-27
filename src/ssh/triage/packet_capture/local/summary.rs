use super::{super::ClassicPcap, Format};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    io::{Read, Seek},
};

mod conversations;
mod pcapng;
mod timeline;
pub use conversations::{Conversations, Traffic};
pub use timeline::{Bucket, Timeline};

const MAX_RECORDS: u32 = 1_000_000;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

#[derive(Default)]
pub struct Summary {
    pub stats: PacketStats,
    pub interfaces: Vec<InterfaceSummary>,
    pub link_types: BTreeSet<u16>,
    pub issue: Option<String>,
    pub timeline: Option<Timeline>,
    pub conversations: Conversations,
}

pub struct InterfaceSummary {
    pub section: Option<u32>,
    pub interface_id: u32,
    pub link_type: u16,
    pub snap_length: Option<u32>,
    pub name: Option<String>,
    pub stats: PacketStats,
    pub capture_statistics: Option<CounterSnapshot>,
}

pub struct CounterSnapshot {
    ticks: u64,
    pub timestamp_unix_ns: i128,
    pub counters: CaptureCounters,
}

#[derive(Default, Serialize)]
pub struct CaptureCounters {
    pub received: Option<u64>,
    pub interface_dropped: Option<u64>,
    pub filter_accepted: Option<u64>,
    pub os_dropped: Option<u64>,
    pub delivered: Option<u64>,
}

#[derive(Default)]
pub struct PacketStats {
    pub packets: u32,
    pub captured_bytes: u64,
    pub truncated_packets: u32,
    pub timestamped_packets: u32,
    earliest: Option<i128>,
    latest: Option<i128>,
}

impl Summary {
    pub(super) fn scan(file: &mut (impl Read + Seek), bytes: u64, format: &Format) -> Self {
        let mut summary = Self::default();
        let mut conversations = conversations::Scan::default();
        let result = file
            .rewind()
            .map_err(anyhow::Error::from)
            .and_then(|()| match format {
                Format::Pcap => summary.pcap(file, bytes, &mut conversations),
                Format::PcapNg => pcapng::scan(&mut summary, file, bytes, &mut conversations),
            });
        summary.issue = result.err().map(|error| format!("{error:#}"));
        summary.conversations = conversations.finish();
        summary
    }

    fn pcap(
        &mut self,
        file: &mut (impl Read + Seek),
        bytes: u64,
        conversations: &mut conversations::Scan,
    ) -> Result<()> {
        let mut header = [0; 24];
        file.read_exact(&mut header)?;
        let format = ClassicPcap::parse(&header)?;
        self.interface(InterfaceSummary {
            section: None,
            interface_id: 0,
            link_type: format.link_type,
            snap_length: Some(format.snap),
            name: None,
            stats: PacketStats::default(),
            capture_statistics: None,
        });
        let fraction_limit = if format.nanoseconds {
            1_000_000_000
        } else {
            1_000_000
        };
        let mut remaining = bytes - 24;
        let mut prefix = [0; conversations::PREFIX_BYTES];
        while remaining > 0 {
            check_record_limit(self.stats.packets)?;
            let ([seconds, fraction, captured, original], kept) =
                format.read_packet(file, &mut remaining, &mut prefix)?;
            let timestamp = (fraction < fraction_limit).then(|| {
                i128::from(seconds) * i128::from(NANOS_PER_SECOND)
                    + i128::from(fraction)
                        * i128::from(NANOS_PER_SECOND / u64::from(fraction_limit))
            });
            self.packet(0, captured, original, timestamp);
            conversations.record(
                conversations::decode(format.link_type, &prefix[..kept]),
                captured,
            );
        }
        Ok(())
    }

    fn interface(&mut self, interface: InterfaceSummary) {
        self.link_types.insert(interface.link_type);
        self.interfaces.push(interface);
    }

    fn packet(&mut self, interface: usize, captured: u32, original: u32, timestamp: Option<i128>) {
        self.stats.record(captured, original, timestamp);
        self.interfaces[interface]
            .stats
            .record(captured, original, timestamp);
        if let Some(timestamp) = timestamp {
            self.timeline
                .get_or_insert_with(Timeline::default)
                .record(timestamp, captured);
        }
    }
}

impl PacketStats {
    pub fn span_ns(&self) -> Option<u128> {
        Some(self.latest?.abs_diff(self.earliest?))
    }

    fn record(&mut self, captured: u32, original: u32, timestamp: Option<i128>) {
        self.packets += 1;
        self.captured_bytes += u64::from(captured);
        self.truncated_packets += u32::from(captured < original);
        if let Some(timestamp) = timestamp {
            self.timestamped_packets += 1;
            self.earliest = Some(
                self.earliest
                    .map_or(timestamp, |current| current.min(timestamp)),
            );
            self.latest = Some(
                self.latest
                    .map_or(timestamp, |current| current.max(timestamp)),
            );
        }
    }
}

fn check_record_limit(records: u32) -> Result<()> {
    ensure!(
        records < MAX_RECORDS,
        "Reached the scan limit of {MAX_RECORDS} records or blocks"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    type Encode16 = fn(u16) -> [u8; 2];
    type Encode32 = fn(u32) -> [u8; 4];

    fn scan(bytes: &[u8], format: Format) -> Summary {
        Summary::scan(&mut Cursor::new(bytes), bytes.len() as u64, &format)
    }

    #[test]
    fn classic_pcap_counts_complete_packets_and_normalizes_timestamp_precision() {
        for (magic, enc16, enc32, fraction) in [
            (
                [0xd4, 0xc3, 0xb2, 0xa1],
                u16::to_le_bytes as fn(u16) -> [u8; 2],
                u32::to_le_bytes as fn(u32) -> [u8; 4],
                500_000,
            ),
            (
                [0xa1, 0xb2, 0x3c, 0x4d],
                u16::to_be_bytes as fn(u16) -> [u8; 2],
                u32::to_be_bytes as fn(u32) -> [u8; 4],
                500_000_003,
            ),
        ] {
            let mut bytes = magic.to_vec();
            bytes.extend(enc16(2));
            bytes.extend(enc16(4));
            bytes.extend([0; 8]);
            bytes.extend(enc32(64));
            bytes.extend(enc32(1));
            for (subseconds, captured, original) in [(fraction, 2, 4), (0, 1, 1)] {
                bytes.extend(
                    [10, subseconds, captured, original]
                        .into_iter()
                        .flat_map(enc32),
                );
                bytes.extend(vec![0; captured as usize]);
            }
            let summary = scan(&bytes, Format::Pcap);
            assert!(summary.issue.is_none());
            assert_eq!(
                (
                    summary.stats.packets,
                    summary.stats.captured_bytes,
                    summary.stats.truncated_packets
                ),
                (2, 3, 1)
            );
            assert_eq!(summary.link_types, [1].into());
            assert_eq!(summary.interfaces[0].snap_length, Some(64));
            assert_eq!(summary.interfaces[0].section, None);
            assert_eq!(summary.interfaces[0].stats.packets, 2);
            assert_eq!(
                summary.stats.span_ns(),
                Some(if fraction == 500_000 {
                    500_000_000
                } else {
                    500_000_003
                })
            );
            bytes.pop();
            let partial = scan(&bytes, Format::Pcap);
            assert_eq!(partial.stats.packets, 1);
            assert!(partial.issue.unwrap().contains("packet is incomplete"));
        }
    }

    fn section(
        bytes: &mut Vec<u8>,
        big: bool,
        blocks: impl FnOnce(&mut Vec<u8>, Encode16, Encode32),
    ) {
        let enc16: Encode16 = if big {
            u16::to_be_bytes
        } else {
            u16::to_le_bytes
        };
        let enc32: Encode32 = if big {
            u32::to_be_bytes
        } else {
            u32::to_le_bytes
        };
        block(bytes, enc32, 0x0a0d0d0a, |data| {
            data.extend(enc32(0x1a2b3c4d));
            data.extend(enc16(1));
            data.extend(enc16(0));
            data.extend([0xff; 8]);
        });
        blocks(bytes, enc16, enc32);
    }

    fn block(
        bytes: &mut Vec<u8>,
        enc32: fn(u32) -> [u8; 4],
        kind: u32,
        body: impl FnOnce(&mut Vec<u8>),
    ) {
        let mut data = Vec::new();
        body(&mut data);
        let length = (data.len() + 12) as u32;
        bytes.extend(enc32(kind));
        bytes.extend(enc32(length));
        bytes.extend(data);
        bytes.extend(enc32(length));
    }

    #[test]
    fn pcapng_uses_section_endianness_and_interface_clocks_and_retains_untimed_packets() {
        let mut bytes = Vec::new();
        section(&mut bytes, false, |bytes, enc16, enc32| {
            block(bytes, enc32, 1, |data| {
                data.extend(enc16(1));
                data.extend(enc16(0));
                data.extend(enc32(4));
                let name = "eth\u{202e}0\n".as_bytes();
                data.extend(enc16(2));
                data.extend(enc16(name.len() as u16));
                data.extend(name);
            });
            block(bytes, enc32, 6, |data| {
                data.extend([0, 0, 1_000_000, 4, 8, 0].into_iter().flat_map(enc32))
            });
            block(bytes, enc32, 3, |data| {
                data.extend([6, 0].into_iter().flat_map(enc32))
            });
            block(bytes, enc32, 5, |data| {
                data.extend([0, 0, 2_000_000].into_iter().flat_map(enc32));
                for (code, value) in [(4, 50_u64), (5, 0), (6, 20), (7, 5), (8, 15)] {
                    data.extend(enc16(code));
                    data.extend(enc16(8));
                    data.extend(value.to_le_bytes());
                }
            });
            block(bytes, enc32, 5, |data| {
                data.extend([0, 0, 1_500_000].into_iter().flat_map(enc32));
                data.extend(enc16(7));
                data.extend(enc16(8));
                data.extend(1_u64.to_le_bytes());
            });
            // A later ISB that carries only timestamps does not replace a counter snapshot.
            block(bytes, enc32, 5, |data| {
                data.extend([0, 0, 3_000_000].into_iter().flat_map(enc32));
            });
        });
        section(&mut bytes, true, |bytes, enc16, enc32| {
            block(bytes, enc32, 1, |data| {
                data.extend(enc16(276));
                data.extend(enc16(0));
                data.extend(enc32(0));
                data.extend(enc16(9));
                data.extend(enc16(1));
                data.extend([0x81, 0, 0, 0]); // binary ticks of half a second
                data.extend(enc16(14));
                data.extend(enc16(8));
                data.extend((-1_i64).to_be_bytes());
            });
            block(bytes, enc32, 0x123, |data| data.extend([0; 4]));
            block(bytes, enc32, 5, |data| {
                data.extend([0, 0, 9].into_iter().flat_map(enc32));
                data.extend(enc16(7));
                data.extend(enc16(8));
                data.extend(0x0102_0304_0506_0708_u64.to_be_bytes());
            });
            block(bytes, enc32, 2, |data| {
                data.extend(enc16(0)); // interface zero in this section
                data.extend(enc16(0xffff));
                data.extend([0, 5, 1, 1, 0].into_iter().flat_map(enc32));
            });
        });
        let summary = scan(&bytes, Format::PcapNg);
        assert!(summary.issue.is_none(), "{:?}", summary.issue);
        assert_eq!(
            (
                summary.stats.packets,
                summary.stats.timestamped_packets,
                summary.stats.captured_bytes
            ),
            (3, 2, 9)
        );
        assert_eq!(summary.stats.truncated_packets, 2);
        assert_eq!(summary.interfaces.len(), 2);
        let first = &summary.interfaces[0];
        let second = &summary.interfaces[1];
        assert_eq!((first.section, first.interface_id), (Some(1), 0));
        assert_eq!((second.section, second.interface_id), (Some(2), 0));
        assert_eq!(first.name.as_deref(), Some("eth0"));
        assert_eq!((first.stats.packets, first.stats.captured_bytes), (2, 8));
        assert_eq!((second.stats.packets, second.stats.captured_bytes), (1, 1));
        let first_stats = first.capture_statistics.as_ref().unwrap();
        let second_stats = second.capture_statistics.as_ref().unwrap();
        assert_eq!(first_stats.timestamp_unix_ns, 2_000_000_000);
        assert_eq!(first_stats.counters.received, Some(50));
        assert_eq!(first_stats.counters.interface_dropped, Some(0));
        assert_eq!(first_stats.counters.filter_accepted, Some(20));
        assert_eq!(first_stats.counters.os_dropped, Some(5));
        assert_eq!(first_stats.counters.delivered, Some(15));
        assert_eq!(second_stats.timestamp_unix_ns, 3_500_000_000);
        assert_eq!(
            second_stats.counters.os_dropped,
            Some(0x0102_0304_0506_0708)
        );
        assert_eq!(second_stats.counters.interface_dropped, None);
        assert_eq!(second.snap_length, None);
        assert_eq!(summary.link_types, [1, 276].into());
        assert_eq!(summary.stats.span_ns(), Some(500_000_000));

        *bytes.last_mut().unwrap() ^= 4;
        let partial = scan(&bytes, Format::PcapNg);
        assert_eq!(partial.stats.packets, 2);
        assert_eq!(partial.interfaces[1].stats.packets, 0);
        assert!(partial.issue.unwrap().contains("block lengths disagree"));
    }

    #[test]
    fn pcapng_reads_conversations_from_all_packet_blocks_within_their_validated_boundaries() {
        let mut bytes = Vec::new();
        let ip = [
            0x45, 0, 0, 20, 0, 0, 0, 0, 64, 0, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2,
        ];
        section(&mut bytes, false, |bytes, enc16, enc32| {
            block(bytes, enc32, 1, |data| {
                data.extend(enc16(101));
                data.extend(enc16(0));
                data.extend(enc32(0));
            });
            block(bytes, enc32, 6, |data| {
                data.extend([0, 0, 1, 20, 20].into_iter().flat_map(enc32));
                data.extend(ip);
            });
            block(bytes, enc32, 3, |data| {
                data.extend(enc32(20));
                data.extend(ip);
            });
        });
        section(&mut bytes, true, |bytes, enc16, enc32| {
            block(bytes, enc32, 1, |data| {
                data.extend(enc16(276));
                data.extend(enc16(0));
                data.extend(enc32(0));
            });
            block(bytes, enc32, 2, |data| {
                data.extend(enc16(0));
                data.extend(enc16(0xffff));
                data.extend([0, 9, 40, 40].into_iter().flat_map(enc32));
                data.extend([0x08, 0x00]);
                data.extend([0; 18]);
                data.extend(&ip[..12]);
                data.extend(&ip[16..20]);
                data.extend(&ip[12..16]);
            });
            block(bytes, enc32, 6, |data| {
                data.extend([0, 0, 10, 39, 40].into_iter().flat_map(enc32));
                data.extend([0x08, 0x00]);
                data.extend([0; 18]);
                data.extend(ip); // The last destination octet is outside caplen.
            });
        });
        let summary = scan(&bytes, Format::PcapNg);
        assert!(summary.issue.is_none());
        assert_eq!(summary.stats.packets, 4);
        assert_eq!(summary.conversations.unclassified_packets, 1);
        assert_eq!(summary.conversations.tracked_pairs, 1);
        let pair = &summary.conversations.top[0];
        assert_eq!(
            (pair.a.to_string(), pair.b.to_string()),
            ("10.0.0.1".into(), "10.0.0.2".into())
        );
        assert_eq!((pair.a_to_b.packets, pair.a_to_b.captured_bytes), (2, 40));
        assert_eq!((pair.b_to_a.packets, pair.b_to_a.captured_bytes), (1, 40));

        bytes.truncate(bytes.len() - 72); // Discard the short packet block.
        *bytes.last_mut().unwrap() ^= 1; // Corrupt the previous block trailer.
        let partial = scan(&bytes, Format::PcapNg);
        assert!(partial.issue.unwrap().contains("block lengths disagree"));
        assert_eq!(partial.stats.packets, 2);
        assert_eq!(partial.conversations.top[0].b_to_a.packets, 0);
    }
}
