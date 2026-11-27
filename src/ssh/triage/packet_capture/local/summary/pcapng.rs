use super::{
    CaptureCounters, ConversationScans, CounterSnapshot, InterfaceSummary, NANOS_PER_SECOND,
    PacketStats, Summary, check_record_limit, ip,
};
use anyhow::{Context, Result, bail, ensure};
use std::io::{Read, Seek};

const SECTION: [u8; 4] = [0x0a, 0x0d, 0x0d, 0x0a];
const MAX_INTERFACES: usize = 4096;
const MAX_OPTIONS_BYTES: u64 = 64 * 1024;
const MAX_NAME_BYTES: usize = 256;

#[derive(Clone, Copy)]
enum Order {
    Little,
    Big,
}

impl Order {
    fn read(magic: [u8; 4]) -> Result<Self> {
        match magic {
            [0x4d, 0x3c, 0x2b, 0x1a] => Ok(Self::Little),
            [0x1a, 0x2b, 0x3c, 0x4d] => Ok(Self::Big),
            _ => bail!("Invalid PCAPNG byte order"),
        }
    }

    fn u16(self, bytes: [u8; 2]) -> u16 {
        match self {
            Self::Little => u16::from_le_bytes(bytes),
            Self::Big => u16::from_be_bytes(bytes),
        }
    }

    fn u32(self, bytes: [u8; 4]) -> u32 {
        match self {
            Self::Little => u32::from_le_bytes(bytes),
            Self::Big => u32::from_be_bytes(bytes),
        }
    }

    fn u64(self, bytes: [u8; 8]) -> u64 {
        match self {
            Self::Little => u64::from_le_bytes(bytes),
            Self::Big => u64::from_be_bytes(bytes),
        }
    }
}

struct Interface {
    link_type: u16,
    snap: u32,
    name: Option<String>,
    resolution: u8,
    offset_seconds: i64,
}

impl Interface {
    fn timestamp_ns(&self, ticks: u64) -> i128 {
        let base = if self.resolution & 0x80 != 0 {
            2_u128
        } else {
            10
        };
        let ticks_ns = u128::from(ticks) * u128::from(NANOS_PER_SECOND);
        let ns = base
            .checked_pow(u32::from(self.resolution & 0x7f))
            .map_or(0, |divisor| ticks_ns / divisor);
        i128::from(self.offset_seconds) * i128::from(NANOS_PER_SECOND) + ns as i128
    }
}

enum Entry {
    Section(Order),
    Interface(Interface),
    Packet {
        interface: usize,
        captured: u32,
        original: u32,
        timestamp: Option<i128>,
        decoded: Option<ip::Packet>,
    },
    Statistics {
        interface: usize,
        snapshot: CounterSnapshot,
    },
    Other,
}

pub(super) fn scan(
    summary: &mut Summary,
    file: &mut (impl Read + Seek),
    mut remaining: u64,
    conversations: &mut ConversationScans,
) -> Result<()> {
    let mut order = None;
    let mut interfaces = Vec::new();
    let mut blocks = 0;
    let mut section_number = 0;
    let mut section_start = 0;
    while remaining > 0 {
        check_record_limit(blocks)?;
        ensure!(remaining >= 12, "Incomplete PCAPNG block header");
        let mut kind = [0; 4];
        let mut size = [0; 4];
        file.read_exact(&mut kind)?;
        file.read_exact(&mut size)?;
        let section = kind == SECTION;
        let (endian, header_size) = if section {
            ensure!(remaining >= 28, "Incomplete PCAPNG section header");
            let mut magic = [0; 4];
            file.read_exact(&mut magic)?;
            (Order::read(magic)?, 12)
        } else {
            (order.context("Missing PCAPNG section header")?, 8)
        };
        let total = u64::from(endian.u32(size));
        let minimum = if section { 28 } else { 12 };
        ensure!(
            total >= minimum && total <= remaining && total.is_multiple_of(4),
            "Incomplete or invalid PCAPNG block length"
        );
        let mut block = Block {
            file,
            order: endian,
            remaining: total - header_size - 4,
        };
        let entry = if section {
            let major = block.u16()?;
            let minor = block.u16()?;
            ensure!(
                major == 1 && matches!(minor, 0 | 2),
                "Unsupported PCAPNG section version {major}.{minor}"
            );
            Entry::Section(endian)
        } else {
            match endian.u32(kind) {
                1 => {
                    ensure!(
                        summary.interfaces.len() < MAX_INTERFACES,
                        "Reached the PCAPNG scan limit of {MAX_INTERFACES} interfaces"
                    );
                    Entry::Interface(block.interface()?)
                }
                2 => block.timed_packet(&interfaces, true)?,
                3 => block.simple_packet(&interfaces)?,
                5 => block.statistics(&interfaces)?,
                6 => block.timed_packet(&interfaces, false)?,
                _ => Entry::Other,
            }
        };
        block.finish(size)?;
        match entry {
            Entry::Section(endian) => {
                order = Some(endian);
                interfaces.clear();
                section_number += 1;
                section_start = summary.interfaces.len();
            }
            Entry::Interface(mut interface) => {
                summary.interface(InterfaceSummary {
                    section: Some(section_number),
                    interface_id: interfaces.len() as u32,
                    link_type: interface.link_type,
                    snap_length: (interface.snap != 0).then_some(interface.snap),
                    name: interface.name.take(),
                    stats: PacketStats::default(),
                    capture_statistics: None,
                });
                interfaces.push(interface);
            }
            Entry::Packet {
                interface,
                captured,
                original,
                timestamp,
                decoded,
            } => {
                summary.packet(
                    section_start + interface,
                    captured,
                    original,
                    timestamp,
                    decoded,
                    conversations,
                );
            }
            Entry::Statistics {
                interface,
                snapshot,
            } => {
                let latest = &mut summary.interfaces[section_start + interface].capture_statistics;
                if latest
                    .as_ref()
                    .is_none_or(|current| snapshot.ticks >= current.ticks)
                {
                    *latest = Some(snapshot);
                }
            }
            Entry::Other => (),
        }
        remaining -= total;
        blocks += 1;
    }
    Ok(())
}

struct Block<'a, T> {
    file: &'a mut T,
    order: Order,
    remaining: u64,
}

impl<T: Read + Seek> Block<'_, T> {
    fn read<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut field = [0; N];
        self.read_into(&mut field)?;
        Ok(field)
    }

    fn read_into(&mut self, field: &mut [u8]) -> Result<()> {
        let bytes = field.len() as u64;
        ensure!(self.remaining >= bytes, "Incomplete PCAPNG block");
        self.file.read_exact(field)?;
        self.remaining -= bytes;
        Ok(())
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(self.order.u16(self.read()?))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(self.order.u32(self.read()?))
    }

    fn ticks(&mut self) -> Result<u64> {
        Ok((u64::from(self.u32()?) << 32) | u64::from(self.u32()?))
    }

    fn option(&mut self) -> Result<Option<(u16, u64)>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let code = self.u16()?;
        let size = u64::from(self.u16()?);
        if code == 0 {
            ensure!(size == 0, "Invalid PCAPNG end-of-options length");
            return Ok(None);
        }
        ensure!(
            size.next_multiple_of(4) <= self.remaining,
            "Incomplete PCAPNG option"
        );
        Ok(Some((code, size)))
    }

    fn skip(&mut self, bytes: u64) -> Result<()> {
        ensure!(self.remaining >= bytes, "Incomplete PCAPNG block");
        self.file.seek_relative(bytes as i64)?;
        self.remaining -= bytes;
        Ok(())
    }

    fn finish(mut self, size: [u8; 4]) -> Result<()> {
        self.skip(self.remaining)?;
        let mut trailer = [0; 4];
        self.file.read_exact(&mut trailer)?;
        ensure!(size == trailer, "PCAPNG block lengths disagree");
        Ok(())
    }

    fn interface(&mut self) -> Result<Interface> {
        let link_type = self.u16()?;
        self.skip(2)?;
        let mut interface = Interface {
            link_type,
            snap: self.u32()?,
            name: None,
            resolution: 6,
            offset_seconds: 0,
        };
        ensure!(
            self.remaining <= MAX_OPTIONS_BYTES,
            "PCAPNG interface options exceed the summary scan limit"
        );
        while let Some((code, size)) = self.option()? {
            match (code, size) {
                (2, _) if interface.name.is_none() => interface.name = self.interface_name(size)?,
                (9, 1) => {
                    interface.resolution = self.read::<1>()?[0];
                    self.skip(3)?;
                }
                (14, 8) => interface.offset_seconds = self.order.u64(self.read()?) as i64,
                (9 | 14, _) => bail!("Invalid PCAPNG interface option length"),
                _ => self.skip(size.next_multiple_of(4))?,
            }
        }
        Ok(interface)
    }

    fn statistics(&mut self, interfaces: &[Interface]) -> Result<Entry> {
        let index = self.u32()? as usize;
        let interface = interfaces
            .get(index)
            .context("PCAPNG statistics refer to an unknown interface")?;
        let ticks = self.ticks()?;
        ensure!(
            self.remaining <= MAX_OPTIONS_BYTES,
            "PCAPNG statistics options exceed the summary scan limit"
        );
        let mut counters = CaptureCounters::default();
        let mut recorded = false;
        while let Some((code, size)) = self.option()? {
            let slot = match code {
                4 => &mut counters.received,
                5 => &mut counters.interface_dropped,
                6 => &mut counters.filter_accepted,
                7 => &mut counters.os_dropped,
                8 => &mut counters.delivered,
                _ => {
                    self.skip(size.next_multiple_of(4))?;
                    continue;
                }
            };
            ensure!(size == 8, "Invalid PCAPNG statistics counter length");
            let value = self.order.u64(self.read()?);
            *slot = Some(value);
            recorded = true;
        }
        Ok(if recorded {
            Entry::Statistics {
                interface: index,
                snapshot: CounterSnapshot {
                    ticks,
                    timestamp_unix_ns: interface.timestamp_ns(ticks),
                    counters,
                },
            }
        } else {
            Entry::Other
        })
    }

    fn interface_name(&mut self, size: u64) -> Result<Option<String>> {
        let mut buffer = [0; MAX_NAME_BYTES];
        let keep = size.min(MAX_NAME_BYTES as u64) as usize;
        self.read_into(&mut buffer[..keep])?;
        self.skip(size.next_multiple_of(4) - keep as u64)?;
        let mut name: String = String::from_utf8_lossy(&buffer[..keep])
            .chars()
            .filter(|&c| {
                !c.is_control()
                    && !matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
            .collect();
        if name.trim().is_empty() {
            return Ok(None);
        }
        if keep as u64 != size {
            name.push('…');
        }
        Ok(Some(name))
    }

    fn timed_packet(&mut self, interfaces: &[Interface], obsolete: bool) -> Result<Entry> {
        let id = if obsolete {
            let id = u32::from(self.u16()?);
            self.skip(2)?;
            id
        } else {
            self.u32()?
        };
        let interface = interfaces
            .get(id as usize)
            .context("PCAPNG packet refers to an unknown interface")?;
        let ticks = self.ticks()?;
        let captured = self.u32()?;
        let original = self.u32()?;
        ensure!(
            u64::from(captured).next_multiple_of(4) <= self.remaining,
            "Incomplete PCAPNG packet data"
        );
        Ok(Entry::Packet {
            interface: id as usize,
            captured,
            original,
            timestamp: Some(interface.timestamp_ns(ticks)),
            decoded: self.decode_ip(interface.link_type, captured)?,
        })
    }

    fn simple_packet(&mut self, interfaces: &[Interface]) -> Result<Entry> {
        let interface = interfaces
            .first()
            .context("PCAPNG packet refers to an unknown interface")?;
        let original = self.u32()?;
        let captured = if interface.snap == 0 {
            original
        } else {
            original.min(interface.snap)
        };
        ensure!(
            u64::from(captured).next_multiple_of(4) == self.remaining,
            "Invalid PCAPNG simple packet length"
        );
        Ok(Entry::Packet {
            interface: 0,
            captured,
            original,
            timestamp: None,
            decoded: self.decode_ip(interface.link_type, captured)?,
        })
    }

    fn decode_ip(&mut self, link_type: u16, captured: u32) -> Result<Option<ip::Packet>> {
        let mut prefix = [0; ip::PREFIX_BYTES];
        let kept = prefix.len().min(captured as usize);
        self.read_into(&mut prefix[..kept])?;
        Ok(ip::decode(link_type, &prefix[..kept]))
    }
}
