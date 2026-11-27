use super::classic_little_endian;
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    borrow::Cow,
    collections::BTreeSet,
    fs::File,
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

mod summary;
use summary::{Bucket, CaptureCounters, PacketStats, Summary};

pub struct LocalCapture {
    pub path: PathBuf,
    pub bytes: u64,
    format: Format,
    pub summary: Summary,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Format {
    Pcap,
    PcapNg,
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u8,
    format: &'a Format,
    file_name: Cow<'a, str>,
    file_bytes: u64,
    #[serde(flatten)]
    stats: StatsReport,
    interfaces: usize,
    interface_details: Vec<InterfaceReport<'a>>,
    timeline: Option<TimelineReport>,
    link_types: &'a BTreeSet<u16>,
    complete: bool,
    issue: Option<&'a str>,
}

#[derive(Serialize)]
struct TimelineReport {
    start_unix_ns: String,
    bucket_width_ns: String,
    buckets: Vec<Bucket>,
}

#[derive(Serialize)]
struct InterfaceReport<'a> {
    section: Option<u32>,
    interface_id: u32,
    name: Option<&'a str>,
    link_type: u16,
    snap_length: Option<u32>,
    capture_statistics: Option<CountersReport<'a>>,
    #[serde(flatten)]
    stats: StatsReport,
}

#[derive(Serialize)]
struct CountersReport<'a> {
    timestamp_unix_ns: String,
    #[serde(flatten)]
    values: &'a CaptureCounters,
}

#[derive(Serialize)]
struct StatsReport {
    packets: u32,
    captured_bytes: u64,
    truncated_packets: u32,
    timestamped_packets: u32,
    timestamp_span_ns: Option<String>,
}

impl From<&PacketStats> for StatsReport {
    fn from(stats: &PacketStats) -> Self {
        Self {
            packets: stats.packets,
            captured_bytes: stats.captured_bytes,
            truncated_packets: stats.truncated_packets,
            timestamped_packets: stats.timestamped_packets,
            // Use a decimal string to preserve precision outside 53-bit JSON readers.
            timestamp_span_ns: stats.span_ns().map(|span| span.to_string()),
        }
    }
}

impl LocalCapture {
    pub fn open(path: PathBuf) -> Result<Self> {
        ensure!(path.is_absolute(), "Choose an absolute capture file path");
        let file = File::open(&path).context("Opening the capture file")?;
        let metadata = file.metadata().context("Reading capture file metadata")?;
        ensure!(metadata.is_file(), "Choose a capture file");
        let bytes = metadata.len();
        let mut file = BufReader::with_capacity(64 * 1024, file);
        let format = read_format(&mut file, bytes)?;
        let summary = Summary::scan(&mut file, bytes, &format);
        Ok(Self {
            path,
            bytes,
            format,
            summary,
        })
    }

    pub fn format_label(&self) -> &'static str {
        match self.format {
            Format::Pcap => "PCAP",
            Format::PcapNg => "PCAPNG",
        }
    }

    /// Write the bounded packet summary as a versioned JSON report.
    pub fn write_json(&self, output: impl Write) -> io::Result<()> {
        let summary = &self.summary;
        let report = Report {
            schema_version: 1,
            format: &self.format,
            file_name: self.path.file_name().unwrap_or_default().to_string_lossy(),
            file_bytes: self.bytes,
            stats: (&summary.stats).into(),
            interfaces: summary.interfaces.len(),
            interface_details: summary
                .interfaces
                .iter()
                .map(|interface| InterfaceReport {
                    section: interface.section,
                    interface_id: interface.interface_id,
                    name: interface.name.as_deref(),
                    link_type: interface.link_type,
                    snap_length: interface.snap_length,
                    capture_statistics: interface.capture_statistics.as_ref().map(|sample| {
                        CountersReport {
                            timestamp_unix_ns: sample.timestamp_unix_ns.to_string(),
                            values: &sample.counters,
                        }
                    }),
                    stats: (&interface.stats).into(),
                })
                .collect(),
            timeline: summary.timeline.as_ref().and_then(|timeline| {
                Some(TimelineReport {
                    start_unix_ns: timeline.start_unix_ns()?.to_string(),
                    bucket_width_ns: timeline.bucket_width_ns().to_string(),
                    buckets: timeline.buckets().collect(),
                })
            }),
            link_types: &summary.link_types,
            complete: summary.issue.is_none(),
            issue: summary.issue.as_deref(),
        };
        let mut output = BufWriter::new(output);
        serde_json::to_writer(&mut output, &report).map_err(io::Error::other)?;
        output.write_all(b"\n")?;
        output.flush()
    }
}

fn read_format(file: &mut (impl Read + Seek), bytes: u64) -> Result<Format> {
    let mut magic = [0; 4];
    file.read_exact(&mut magic)
        .context("Reading the capture file header")?;
    if classic_little_endian(magic).is_some() {
        file.read_exact(&mut [0; 20])
            .context("Incomplete PCAP file header")?;
        return Ok(Format::Pcap);
    }
    ensure!(
        magic == [0x0a, 0x0d, 0x0d, 0x0a],
        "Choose a PCAP or PCAPNG capture file"
    );

    let mut length = [0; 4];
    let mut endianness = [0; 4];
    file.read_exact(&mut length)
        .context("Incomplete PCAPNG section header")?;
    file.read_exact(&mut endianness)
        .context("Incomplete PCAPNG section header")?;
    let section_header = u64::from(match endianness {
        [0x4d, 0x3c, 0x2b, 0x1a] => u32::from_le_bytes(length),
        [0x1a, 0x2b, 0x3c, 0x4d] => u32::from_be_bytes(length),
        _ => bail!("Invalid PCAPNG byte order"),
    });
    ensure!(
        section_header >= 28 && section_header <= bytes && section_header.is_multiple_of(4),
        "Invalid PCAPNG section header length"
    );
    let mut trailer = [0; 4];
    file.seek(SeekFrom::Start(section_header - 4))?;
    file.read_exact(&mut trailer)
        .context("Incomplete PCAPNG section header")?;
    ensure!(length == trailer, "PCAPNG section header lengths disagree");
    Ok(Format::PcapNg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn detect(bytes: &[u8]) -> Result<Format> {
        read_format(&mut Cursor::new(bytes), bytes.len() as u64)
    }

    #[test]
    fn recognizes_standard_pcap_magics_and_requires_the_complete_header() {
        for magic in [
            [0xd4, 0xc3, 0xb2, 0xa1],
            [0xa1, 0xb2, 0xc3, 0xd4],
            [0x4d, 0x3c, 0xb2, 0xa1],
            [0xa1, 0xb2, 0x3c, 0x4d],
        ] {
            let mut header = [0; 24];
            header[..4].copy_from_slice(&magic);
            assert_eq!(detect(&header).unwrap(), Format::Pcap);
            assert!(detect(&header[..23]).is_err());
        }
        assert!(detect(b"This is a text file.").is_err());
    }

    #[test]
    fn checks_both_byte_orders_and_the_pcapng_section_trailer() {
        for encode in [u32::to_le_bytes, u32::to_be_bytes] {
            let mut header = [0; 32];
            header[..4].copy_from_slice(&[0x0a, 0x0d, 0x0d, 0x0a]);
            header[4..8].copy_from_slice(&encode(32));
            header[8..12].copy_from_slice(&encode(0x1a2b3c4d));
            header[28..32].copy_from_slice(&encode(32));
            assert_eq!(detect(&header).unwrap(), Format::PcapNg);
            assert!(detect(&header[..31]).is_err());
            header[28..32].copy_from_slice(&encode(28));
            assert!(detect(&header).is_err());
            header[4..8].copy_from_slice(&encode(29));
            assert!(detect(&header).is_err());
            header[4..8].copy_from_slice(&encode(32));
            header[8..12].fill(0);
            assert!(detect(&header).is_err());
        }
    }
}
