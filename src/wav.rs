//! One bounded PCM interpretation for validation and native memory playback.
use anyhow::{Result, ensure};

pub struct ValidatedSound(Vec<u8>);

impl ValidatedSound {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let pcm = parse_pcm(bytes)?;
        let padding = pcm.samples.len() & 1;
        let mut canonical = Vec::with_capacity(44 + pcm.samples.len() + padding);
        canonical.extend_from_slice(b"RIFF");
        canonical
            .extend_from_slice(&(36 + pcm.samples.len() as u32 + padding as u32).to_le_bytes());
        canonical.extend_from_slice(b"WAVEfmt \x10\0\0\0");
        canonical.extend_from_slice(&pcm.format[..16]);
        canonical.extend_from_slice(b"data");
        canonical.extend_from_slice(&(pcm.samples.len() as u32).to_le_bytes());
        canonical.extend_from_slice(pcm.samples);
        if padding != 0 {
            canonical.push(0);
        }
        Ok(Self(canonical))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

pub fn validate(bytes: &[u8]) -> Result<()> {
    parse_pcm(bytes).map(|_| ())
}

struct Pcm<'a> {
    format: &'a [u8],
    samples: &'a [u8],
}

fn parse_pcm(bytes: &[u8]) -> Result<Pcm<'_>> {
    ensure!(
        bytes.len() >= 44
            && bytes.len() <= crate::assets::MAX_FILE
            && &bytes[..4] == b"RIFF"
            && &bytes[8..12] == b"WAVE",
        "sound is not a bounded RIFF/WAVE file"
    );
    let u32_at = |i| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
    ensure!(
        u64::from(u32_at(4)) + 8 == bytes.len() as u64,
        "RIFF size must match the complete file"
    );
    let mut offset = 12usize;
    let mut format = None;
    let mut samples = None;
    let mut byte_rate = 0usize;
    let mut alignment = 0usize;
    while offset < bytes.len() {
        ensure!(bytes.len() - offset >= 8, "truncated WAV chunk header");
        let size = u32_at(offset + 4) as usize;
        let start = offset + 8;
        ensure!(size <= bytes.len() - start, "invalid WAV chunk size");
        let end = start + size;
        let padded_end = end
            .checked_add(size & 1)
            .ok_or_else(|| anyhow::anyhow!("WAV size overflow"))?;
        ensure!(padded_end <= bytes.len(), "missing WAV chunk padding");
        let chunk = &bytes[start..end];
        match &bytes[offset..offset + 4] {
            b"fmt " => {
                ensure!(
                    format.is_none() && samples.is_none(),
                    "duplicate or misplaced WAV format"
                );
                ensure!(
                    size == 16 || (size == 18 && chunk[16..18] == [0, 0]),
                    "invalid PCM format size"
                );
                let u16_at = |i| u16::from_le_bytes(chunk[i..i + 2].try_into().unwrap());
                let rate = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
                let channels = u16_at(2);
                let bits = u16_at(14);
                ensure!(
                    u16_at(0) == 1
                        && (1..=2).contains(&channels)
                        && (8000..=96000).contains(&rate)
                        && matches!(bits, 8 | 16),
                    "only mono/stereo PCM WAV, 8/16 bit, 8–96 kHz is supported"
                );
                alignment = channels as usize * bits as usize / 8;
                byte_rate = (rate as usize)
                    .checked_mul(alignment)
                    .ok_or_else(|| anyhow::anyhow!("WAV byte rate overflow"))?;
                ensure!(
                    u16_at(12) as usize == alignment && u32_at(start + 8) as usize == byte_rate,
                    "PCM block alignment or byte rate is inconsistent"
                );
                format = Some(chunk);
            }
            b"data" => {
                ensure!(
                    format.is_some() && samples.is_none(),
                    "WAV needs one data chunk after its format"
                );
                ensure!(
                    size > 0 && size.is_multiple_of(alignment),
                    "WAV data must contain whole sample frames"
                );
                ensure!(
                    size <= byte_rate
                        .checked_mul(5)
                        .ok_or_else(|| anyhow::anyhow!("WAV duration overflow"))?,
                    "sound exceeds five-second duration limit"
                );
                samples = Some(chunk);
            }
            _ => {} // Bounded ancillary chunks are discarded when canonicalizing.
        }
        offset = padded_end;
    }
    Ok(Pcm {
        format: format.ok_or_else(|| anyhow::anyhow!("WAV is missing format"))?,
        samples: samples.ok_or_else(|| anyhow::anyhow!("WAV is missing audio data"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend((body.len() as u32).to_le_bytes());
        v.extend(body);
        if body.len() & 1 != 0 {
            v.push(0);
        }
        v
    }
    fn format(rate: u32, channels: u16, bits: u16) -> Vec<u8> {
        let alignment = channels * bits / 8;
        let mut v = 1u16.to_le_bytes().to_vec();
        v.extend(channels.to_le_bytes());
        v.extend(rate.to_le_bytes());
        v.extend((rate * u32::from(alignment)).to_le_bytes());
        v.extend(alignment.to_le_bytes());
        v.extend(bits.to_le_bytes());
        chunk(b"fmt ", &v)
    }
    fn wav(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut v = b"RIFF\0\0\0\0WAVE".to_vec();
        for c in chunks {
            v.extend(c);
        }
        let len = v.len() as u32 - 8;
        v[4..8].copy_from_slice(&len.to_le_bytes());
        v
    }
    #[test]
    fn canonical_pcm_has_exact_sizes_samples_and_five_second_boundary() {
        for rate in [8000, 44100, 96000] {
            for channels in [1, 2] {
                for bits in [8, 16] {
                    let count = rate as usize * channels as usize * bits as usize / 8 * 5;
                    let samples = vec![0x42; count];
                    let fmt = format(rate, channels, bits);
                    let raw = wav(&[
                        chunk(b"JUNK", b"odd"),
                        fmt.clone(),
                        chunk(b"data", &samples),
                        chunk(b"LIST", b"meta"),
                    ]);
                    let sound = ValidatedSound::parse(&raw).unwrap();
                    let canonical = sound.as_bytes();
                    assert_eq!(canonical.len(), 44 + count);
                    assert_eq!(&canonical[20..36], &fmt[8..24]);
                    assert_eq!(&canonical[44..], samples);
                    assert_eq!(
                        u32::from_le_bytes(canonical[4..8].try_into().unwrap()) as usize,
                        canonical.len() - 8
                    );
                    assert_eq!(
                        u32::from_le_bytes(canonical[40..44].try_into().unwrap()) as usize,
                        count
                    );
                    assert!(validate(canonical).is_ok());
                    assert!(
                        validate(&wav(&[
                            fmt,
                            chunk(
                                b"data",
                                &vec![0; count + channels as usize * bits as usize / 8]
                            )
                        ]))
                        .is_err()
                    );
                }
            }
        }
        let raw = wav(&[format(8000, 1, 8), chunk(b"data", &[128])]);
        assert_eq!(ValidatedSound::parse(&raw).unwrap().as_bytes(), raw);
    }
    #[test]
    fn rejects_ambiguous_headers_chunks_padding_and_partial_samples() {
        let fmt = format(8000, 1, 8);
        let data = chunk(b"data", &[128; 8]);
        let valid = wav(&[fmt.clone(), data.clone()]);
        for size in [0, 4, u32::MAX] {
            let mut v = valid.clone();
            v[4..8].copy_from_slice(&size.to_le_bytes());
            assert!(validate(&v).is_err());
        }
        for chunks in [
            vec![data.clone(), fmt.clone()],
            vec![fmt.clone(), fmt.clone(), data.clone()],
            vec![fmt.clone(), data.clone(), data.clone()],
            vec![fmt.clone(), data.clone(), format(96000, 2, 16)],
            vec![fmt.clone()],
            vec![fmt.clone(), chunk(b"data", &[])],
            vec![format(8000, 2, 16), chunk(b"data", &[0; 3])],
            vec![
                format(8000, 1, 8),
                format(96000, 2, 16),
                chunk(b"data", &vec![0; 1_920_000]),
            ],
        ] {
            assert!(validate(&wav(&chunks)).is_err());
        }
        for offset in [20, 22, 24, 28, 32, 34] {
            let mut v = valid.clone();
            v[offset] = 0xff;
            assert!(validate(&v).is_err(), "field {offset}");
        }
        for len in 0..valid.len() {
            assert!(validate(&valid[..len]).is_err(), "truncation {len}");
        }
        let mut missing_pad = chunk(b"data", &[128]);
        missing_pad.pop();
        assert!(validate(&wav(&[fmt.clone(), missing_pad])).is_err());
        let mut trailing = valid;
        trailing.push(0);
        let len = trailing.len() as u32 - 8;
        trailing[4..8].copy_from_slice(&len.to_le_bytes());
        assert!(validate(&trailing).is_err());
        let mut extended = fmt;
        extended[4..8].copy_from_slice(&18u32.to_le_bytes());
        extended.extend([0, 0]);
        assert!(validate(&wav(&[extended.clone(), data.clone()])).is_ok());
        extended[24] = 1;
        assert!(validate(&wav(&[extended, data])).is_err());
    }
}
