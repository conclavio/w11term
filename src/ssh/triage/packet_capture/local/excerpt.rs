use super::{
    ConversationEndpoints, Format, LocalCapture, SourceStamp, read_format,
    summary::{check_record_limit, ip, pcapng},
};
use crate::ssh::triage::packet_capture::ClassicPcap;
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    net::SocketAddr,
    ops::Range,
    path::{Path, PathBuf},
};

pub struct CaptureExcerptPlan {
    source: PathBuf,
    stamp: SourceStamp,
    format: Format,
    time: Option<ExcerptTime>,
    endpoints: Option<ConversationEndpoints>,
}

enum ExcerptTime {
    Elapsed(Range<i128>),
    Absolute(Range<i128>),
}

impl ExcerptTime {
    fn bounds(&self) -> &Range<i128> {
        match self {
            Self::Elapsed(bounds) | Self::Absolute(bounds) => bounds,
        }
    }
}

#[derive(Serialize)]
pub struct CaptureExcerpt {
    pub path: PathBuf,
    pub packets: u32,
}

impl CaptureExcerptPlan {
    pub fn elapsed(source: PathBuf, bounds: Range<i128>) -> Result<Self> {
        Self::prepare(source, Some(ExcerptTime::Elapsed(bounds)), None, None)
    }

    pub fn conversation(
        source: PathBuf,
        endpoints: ConversationEndpoints,
        elapsed: Option<Range<i128>>,
    ) -> Result<Self> {
        Self::prepare(
            source,
            elapsed.map(ExcerptTime::Elapsed),
            Some(endpoints),
            None,
        )
    }

    pub(super) fn absolute(
        source: PathBuf,
        bounds: Range<i128>,
        expected: SourceStamp,
    ) -> Result<Self> {
        Self::prepare(
            source,
            Some(ExcerptTime::Absolute(bounds)),
            None,
            Some(expected),
        )
    }

    pub(super) fn conversation_absolute(
        source: PathBuf,
        endpoints: ConversationEndpoints,
        bounds: Option<Range<i128>>,
        expected: SourceStamp,
    ) -> Result<Self> {
        Self::prepare(
            source,
            bounds.map(ExcerptTime::Absolute),
            Some(endpoints),
            Some(expected),
        )
    }

    fn prepare(
        source: PathBuf,
        time: Option<ExcerptTime>,
        endpoints: Option<ConversationEndpoints>,
        expected: Option<SourceStamp>,
    ) -> Result<Self> {
        ensure!(
            source.is_absolute(),
            "Choose an absolute source capture path"
        );
        ensure!(
            time.as_ref().is_none_or(|time| !time.bounds().is_empty()),
            "Choose a nonempty capture interval"
        );
        if let Some(endpoints) = endpoints {
            validate_endpoints(endpoints)?;
        }
        let (mut file, stamp) = open_source(&source, expected)?;
        let format = read_format(&mut file, stamp.bytes)?;
        Ok(Self {
            source,
            stamp,
            format,
            time,
            endpoints,
        })
    }

    pub fn source(&self) -> &Path {
        &self.source
    }

    pub fn extension(&self) -> &'static str {
        match self.format {
            Format::Pcap => "pcap",
            Format::PcapNg => "pcapng",
        }
    }

    pub fn save(self, destination: PathBuf) -> Result<CaptureExcerpt> {
        ensure!(
            destination.is_absolute() && destination.file_name().is_some(),
            "Choose an absolute excerpt file path"
        );
        let (mut source, _) = open_source(&self.source, Some(self.stamp))?;
        let mut staged = tempfile::NamedTempFile::new_in(
            destination
                .parent()
                .context("Choose an excerpt directory")?,
        )?;
        let mut output = BufWriter::new(staged.as_file_mut());
        let mut origin = None;
        let selected = |timestamp: Option<i128>, packet: Option<&ip::Packet>| {
            // The elapsed clock starts at the first timestamped record in the
            // file, even when that record belongs to another conversation.
            self.time.as_ref().is_none_or(|time| {
                timestamp.is_some_and(|timestamp| {
                    let coordinate = match time {
                        ExcerptTime::Absolute(_) => timestamp,
                        ExcerptTime::Elapsed(_) => timestamp - *origin.get_or_insert(timestamp),
                    };
                    time.bounds().contains(&coordinate)
                })
            }) && self.endpoints.is_none_or(|endpoints| {
                packet.is_some_and(|packet| matches_pair(endpoints, packet))
            })
        };
        let packets = match self.format {
            Format::Pcap => write_classic(
                &mut source,
                self.stamp.bytes,
                &mut output,
                self.endpoints.is_some(),
                selected,
            ),
            Format::PcapNg => {
                pcapng::write_excerpt(&mut source, self.stamp.bytes, &mut output, selected)
            }
        }
        .context("Reading source packets for the excerpt")?;
        ensure!(
            packets != 0,
            if self.endpoints.is_some() {
                "No packets match the selected conversation and time limits"
            } else {
                "No timestamped packets fall within the selected interval"
            }
        );
        ensure!(
            self.stamp == SourceStamp::from(&source.get_ref().metadata()?),
            "The source capture changed while saving the excerpt"
        );
        output.flush()?;
        drop(output);
        staged.as_file().sync_all()?;
        staged
            .persist_noclobber(&destination)
            .map_err(|error| error.error)
            .with_context(|| format!("Saving {}", destination.display()))?;
        Ok(CaptureExcerpt {
            path: destination,
            packets,
        })
    }
}

impl LocalCapture {
    pub fn prepare_excerpt(&self, bounds: Range<i128>) -> Result<CaptureExcerptPlan> {
        CaptureExcerptPlan::absolute(self.path.clone(), bounds, self.source_stamp)
    }

    pub fn prepare_conversation_excerpt(
        &self,
        endpoints: ConversationEndpoints,
        bounds: Option<Range<i128>>,
    ) -> Result<CaptureExcerptPlan> {
        CaptureExcerptPlan::conversation_absolute(
            self.path.clone(),
            endpoints,
            bounds,
            self.source_stamp,
        )
    }
}

fn validate_endpoints(endpoints: ConversationEndpoints) -> Result<()> {
    let (a, b) = match endpoints {
        ConversationEndpoints::Ip { a, b } => (a, b),
        ConversationEndpoints::Transport { protocol, a, b } => {
            ensure!(
                matches!(protocol, 6 | 17),
                "Choose a TCP or UDP conversation"
            );
            ensure!(
                [a, b]
                    .into_iter()
                    .all(|address| !matches!(address, SocketAddr::V6(v6) if v6.scope_id() != 0)),
                "Use IPv6 addresses without interface scope identifiers"
            );
            (a.ip(), b.ip())
        }
    };
    ensure!(
        a.is_ipv4() == b.is_ipv4(),
        "Choose two addresses from the same IP version"
    );
    Ok(())
}

fn matches_pair(endpoints: ConversationEndpoints, packet: &ip::Packet) -> bool {
    let (source, destination) = packet.addresses;
    let both_directions = |a, b| packet.addresses == (a, b) || packet.addresses == (b, a);
    match endpoints {
        ConversationEndpoints::Ip { a, b } => both_directions(a, b),
        ConversationEndpoints::Transport { protocol, a, b } => {
            packet.protocol == Some(protocol)
                && packet.ports.is_some_and(|(src_port, dst_port)| {
                    let pair = (
                        SocketAddr::new(source, src_port),
                        SocketAddr::new(destination, dst_port),
                    );
                    pair == (a, b) || pair == (b, a)
                })
        }
    }
}

fn open_source(
    path: &Path,
    expected: Option<SourceStamp>,
) -> Result<(BufReader<File>, SourceStamp)> {
    let file = File::open(path).context("Opening the source capture")?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Choose a source capture file");
    let stamp = SourceStamp::from(&metadata);
    ensure!(
        expected.is_none_or(|expected| expected == stamp),
        "The source capture changed. Reopen it before saving an excerpt."
    );
    Ok((BufReader::with_capacity(64 * 1024, file), stamp))
}

fn write_classic(
    source: &mut (impl Read + Seek),
    bytes: u64,
    output: &mut impl Write,
    read_ip: bool,
    mut selected: impl FnMut(Option<i128>, Option<&ip::Packet>) -> bool,
) -> Result<u32> {
    let mut header = [0; 24];
    source.read_exact(&mut header)?;
    let format = ClassicPcap::parse(&header)?;
    output.write_all(&header)?;
    let mut remaining = bytes - header.len() as u64;
    let mut records = 0;
    let mut packets = 0;
    let mut prefix = [0; ip::PREFIX_BYTES];
    let prefix = if read_ip {
        &mut prefix[..]
    } else {
        &mut prefix[..0]
    };
    while remaining != 0 {
        check_record_limit(records)?;
        let start = bytes - remaining;
        let ([seconds, fraction, _, _], kept) =
            format.read_packet(source, &mut remaining, prefix)?;
        records += 1;
        let decoded = read_ip
            .then(|| ip::decode(format.link_type, &prefix[..kept]))
            .flatten();
        if selected(format.timestamp_ns(seconds, fraction), decoded.as_ref()) {
            source.seek(SeekFrom::Start(start))?;
            let length = bytes - remaining - start;
            let copied = io::copy(&mut (&mut *source).take(length), output)?;
            ensure!(copied == length, "The source packet was truncated");
            packets += 1;
        }
    }
    Ok(packets)
}
