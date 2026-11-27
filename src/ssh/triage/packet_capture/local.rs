use super::classic_little_endian;
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

pub struct LocalCapture {
    pub path: PathBuf,
    pub bytes: u64,
    format: Format,
}

#[derive(Debug, PartialEq, Eq)]
enum Format {
    Pcap,
    PcapNg,
}

impl LocalCapture {
    pub fn open(path: PathBuf) -> Result<Self> {
        ensure!(path.is_absolute(), "Choose an absolute capture file path");
        let mut file = File::open(&path).context("Opening the capture file")?;
        let metadata = file.metadata().context("Reading capture file metadata")?;
        ensure!(metadata.is_file(), "Choose a capture file");
        let bytes = metadata.len();
        let format = read_format(&mut file, bytes)?;
        Ok(Self {
            path,
            bytes,
            format,
        })
    }

    pub fn format_label(&self) -> &'static str {
        match self.format {
            Format::Pcap => "PCAP",
            Format::PcapNg => "PCAPNG",
        }
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
