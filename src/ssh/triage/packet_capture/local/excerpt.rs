use super::{
    Format, LocalCapture, SourceStamp, read_format,
    summary::{check_record_limit, pcapng},
};
use crate::ssh::triage::packet_capture::ClassicPcap;
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    ops::Range,
    path::{Path, PathBuf},
};

pub struct CaptureExcerptPlan {
    source: PathBuf,
    stamp: SourceStamp,
    format: Format,
    bounds: Range<i128>,
    elapsed: bool,
}

#[derive(Serialize)]
pub struct CaptureExcerpt {
    pub path: PathBuf,
    pub packets: u32,
}

impl CaptureExcerptPlan {
    pub fn elapsed(source: PathBuf, bounds: Range<i128>) -> Result<Self> {
        Self::prepare(source, bounds, true, None)
    }

    pub(super) fn absolute(
        source: PathBuf,
        bounds: Range<i128>,
        expected: SourceStamp,
    ) -> Result<Self> {
        Self::prepare(source, bounds, false, Some(expected))
    }

    fn prepare(
        source: PathBuf,
        bounds: Range<i128>,
        elapsed: bool,
        expected: Option<SourceStamp>,
    ) -> Result<Self> {
        ensure!(
            source.is_absolute(),
            "Choose an absolute source capture path"
        );
        ensure!(
            bounds.start < bounds.end,
            "Choose a nonempty capture interval"
        );
        let (mut file, stamp) = open_source(&source, expected)?;
        let format = read_format(&mut file, stamp.bytes)?;
        Ok(Self {
            source,
            stamp,
            format,
            bounds,
            elapsed,
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
        let selected = |timestamp: i128| {
            let coordinate = if self.elapsed {
                timestamp - *origin.get_or_insert(timestamp)
            } else {
                timestamp
            };
            self.bounds.contains(&coordinate)
        };
        let packets = match self.format {
            Format::Pcap => write_classic(&mut source, self.stamp.bytes, &mut output, selected),
            Format::PcapNg => {
                pcapng::write_excerpt(&mut source, self.stamp.bytes, &mut output, selected)
            }
        }
        .context("Reading source packets for the excerpt")?;
        ensure!(
            packets != 0,
            "No timestamped packets fall within the selected interval"
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
    mut selected: impl FnMut(i128) -> bool,
) -> Result<u32> {
    let mut header = [0; 24];
    source.read_exact(&mut header)?;
    let format = ClassicPcap::parse(&header)?;
    output.write_all(&header)?;
    let mut remaining = bytes - header.len() as u64;
    let mut records = 0;
    let mut packets = 0;
    while remaining != 0 {
        check_record_limit(records)?;
        let start = bytes - remaining;
        let ([seconds, fraction, _, _], _) = format.read_packet(source, &mut remaining, &mut [])?;
        records += 1;
        if format
            .timestamp_ns(seconds, fraction)
            .is_some_and(&mut selected)
        {
            source.seek(SeekFrom::Start(start))?;
            let length = bytes - remaining - start;
            let copied = io::copy(&mut (&mut *source).take(length), output)?;
            ensure!(copied == length, "The source packet was truncated");
            packets += 1;
        }
    }
    Ok(packets)
}
