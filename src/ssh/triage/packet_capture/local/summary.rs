use super::{super::ClassicPcap, Format};
use anyhow::{Result, ensure};
use std::{
    collections::BTreeSet,
    io::{Read, Seek},
};

mod pcapng;

const MAX_RECORDS: u32 = 1_000_000;
const NANOS_PER_SECOND: u64 = 1_000_000_000;

#[derive(Default)]
pub struct Summary {
    pub packets: u32,
    pub captured_bytes: u64,
    pub truncated_packets: u32,
    pub timestamped_packets: u32,
    pub interfaces: u32,
    pub link_types: BTreeSet<u16>,
    pub issue: Option<String>,
    earliest: Option<i128>,
    latest: Option<i128>,
}

impl Summary {
    pub(super) fn scan(file: &mut (impl Read + Seek), bytes: u64, format: &Format) -> Self {
        let mut summary = Self::default();
        let result = file
            .rewind()
            .map_err(anyhow::Error::from)
            .and_then(|()| match format {
                Format::Pcap => summary.pcap(file, bytes),
                Format::PcapNg => pcapng::scan(&mut summary, file, bytes),
            });
        summary.issue = result.err().map(|error| format!("{error:#}"));
        summary
    }

    pub fn span_ns(&self) -> Option<u128> {
        Some(self.latest?.abs_diff(self.earliest?))
    }

    fn pcap(&mut self, file: &mut (impl Read + Seek), bytes: u64) -> Result<()> {
        let mut header = [0; 24];
        file.read_exact(&mut header)?;
        let format = ClassicPcap::parse(&header)?;
        self.interface(format.link_type);
        let fraction_limit = if format.nanoseconds {
            1_000_000_000
        } else {
            1_000_000
        };
        let mut remaining = bytes - 24;
        while remaining > 0 {
            check_record_limit(self.packets)?;
            let [seconds, fraction, captured, original] =
                format.read_packet(file, &mut remaining)?;
            let timestamp = (fraction < fraction_limit).then(|| {
                i128::from(seconds) * i128::from(NANOS_PER_SECOND)
                    + i128::from(fraction)
                        * i128::from(NANOS_PER_SECOND / u64::from(fraction_limit))
            });
            self.packet(captured, original, timestamp);
        }
        Ok(())
    }

    fn interface(&mut self, link_type: u16) {
        self.interfaces += 1;
        self.link_types.insert(link_type);
    }

    fn packet(&mut self, captured: u32, original: u32, timestamp: Option<i128>) {
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
                    summary.packets,
                    summary.captured_bytes,
                    summary.truncated_packets
                ),
                (2, 3, 1)
            );
            assert_eq!(summary.link_types, [1].into());
            assert_eq!(
                summary.span_ns(),
                Some(if fraction == 500_000 {
                    500_000_000
                } else {
                    500_000_003
                })
            );
            bytes.pop();
            let partial = scan(&bytes, Format::Pcap);
            assert_eq!(partial.packets, 1);
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
            });
            block(bytes, enc32, 6, |data| {
                data.extend([0, 0, 1_000_000, 4, 8, 0].into_iter().flat_map(enc32))
            });
            block(bytes, enc32, 3, |data| {
                data.extend([6, 0].into_iter().flat_map(enc32))
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
                summary.packets,
                summary.timestamped_packets,
                summary.captured_bytes
            ),
            (3, 2, 9)
        );
        assert_eq!(summary.truncated_packets, 2);
        assert_eq!(summary.interfaces, 2);
        assert_eq!(summary.link_types, [1, 276].into());
        assert_eq!(summary.span_ns(), Some(500_000_000));

        *bytes.last_mut().unwrap() ^= 4;
        let partial = scan(&bytes, Format::PcapNg);
        assert_eq!(partial.packets, 2);
        assert!(partial.issue.unwrap().contains("block lengths disagree"));
    }
}
