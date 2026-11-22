use super::MAX_BYTES;
use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, BufWriter, Read, Seek, Write},
    path::Path,
    time::Duration,
};
use tokio::time::Instant;
use wezterm_term::TerminalSize;

const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LINE: usize = 512;
const OUTPUT_CHUNK: usize = 64 * 1024;
const MAX_EVENTS: u64 = 1_000_000;
const MAX_US: u64 = 366 * 24 * 60 * 60 * 1_000_000;

#[derive(Serialize, Deserialize)]
#[serde(remote = "TerminalSize", deny_unknown_fields)]
struct Geometry {
    cols: usize,
    rows: usize,
    pixel_width: usize,
    pixel_height: usize,
    dpi: u32,
}

fn validate_size(size: TerminalSize) -> Result<()> {
    ensure!(
        super::valid_grid(size.cols, size.rows)
            && size.pixel_width <= 65_535
            && size.pixel_height <= 65_535
            && size.dpi <= 65_535,
        "timed recording geometry exceeds the supported range"
    );
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Entry {
    Header {
        version: u8,
        #[serde(with = "Geometry")]
        size: TerminalSize,
    },
    Output {
        at_us: u64,
        len: usize,
    },
    Resize {
        at_us: u64,
        #[serde(with = "Geometry")]
        size: TerminalSize,
    },
    End {
        at_us: u64,
        events: u64,
        bytes: u64,
        sha256: String,
    },
}

fn hex_digest(hasher: Sha256) -> String {
    format!("{:x}", Bytes::copy_from_slice(&hasher.finalize()))
}

pub(super) struct TimelineWriter {
    index: BufWriter<File>,
    start: Instant,
    size: TerminalSize,
    line: Vec<u8>,
    index_bytes: u64,
    events: u64,
    hash: Sha256,
}

impl TimelineWriter {
    pub(super) fn new(file: File, size: TerminalSize) -> Result<Self> {
        validate_size(size)?;
        let mut this = Self {
            index: BufWriter::new(file),
            size,
            start: Instant::now(),
            line: Vec::new(),
            index_bytes: 0,
            events: 0,
            hash: Sha256::new(),
        };
        this.write_line(Entry::Header { version: 2, size })?;
        this.index.flush()?;
        Ok(this)
    }

    fn write_line(&mut self, entry: Entry) -> Result<()> {
        self.line.clear();
        serde_json::to_writer(&mut self.line, &entry)?;
        self.line.push(b'\n');
        ensure!(
            self.line.len() <= MAX_LINE,
            "timed recording index line is too long"
        );
        let bytes = self.index_bytes + self.line.len() as u64;
        let reserve = if matches!(entry, Entry::End { .. }) {
            0
        } else {
            MAX_LINE as u64
        };
        ensure!(
            bytes + reserve <= MAX_INDEX_BYTES,
            "timed recording index reached 64 MiB"
        );
        self.index.write_all(&self.line)?;
        self.index_bytes = bytes;
        Ok(())
    }

    fn time_us(&self, at: Instant) -> Result<u64> {
        let micros = at.duration_since(self.start).as_micros();
        ensure!(
            micros <= u128::from(MAX_US),
            "timed recording exceeds the duration limit"
        );
        Ok(micros as u64)
    }

    fn event(&mut self, entry: Entry) -> Result<()> {
        ensure!(
            self.events < MAX_EVENTS,
            "timed recording reached its event limit"
        );
        self.write_line(entry)?;
        self.events += 1;
        Ok(())
    }

    pub(super) fn output(&mut self, data: &[u8]) -> Result<()> {
        let at_us = self.time_us(Instant::now())?;
        for chunk in data.chunks(OUTPUT_CHUNK) {
            self.event(Entry::Output {
                at_us,
                len: chunk.len(),
            })?;
        }
        self.hash.update(data);
        Ok(())
    }

    pub(super) fn resize(&mut self, size: TerminalSize) -> Result<()> {
        if size != self.size {
            validate_size(size)?;
            self.event(Entry::Resize {
                at_us: self.time_us(Instant::now())?,
                size,
            })?;
            self.size = size;
        }
        Ok(())
    }

    pub(super) fn finish(mut self, bytes: u64, stopped: Instant) -> Result<()> {
        let sha256 = hex_digest(std::mem::take(&mut self.hash));
        self.write_line(Entry::End {
            at_us: self.time_us(stopped)?,
            events: self.events,
            bytes,
            sha256,
        })?;
        self.index.flush()?;
        self.index.get_ref().sync_all()?;
        Ok(())
    }
}

fn open_read(path: &Path, limit: u64) -> Result<File> {
    let mut options = File::options();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
        // Keep the validated pair stable throughout playback.
        options.share_mode(FILE_SHARE_READ.0);
    }
    let file = options
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit,
        "invalid or oversized recording file {}",
        path.display()
    );
    Ok(file)
}

struct Timeline {
    reader: BufReader<File>,
    line: Vec<u8>,
    initial: TerminalSize,
    events: u64,
    bytes: u64,
    last_us: u64,
    done: bool,
}

impl Timeline {
    fn open(path: &Path) -> Result<Self> {
        Self::from_file(open_read(
            &path.with_extension("vt.jsonl"),
            MAX_INDEX_BYTES,
        )?)
    }

    fn from_file(file: File) -> Result<Self> {
        let mut this = Self {
            reader: BufReader::new(file),
            line: Vec::new(),
            initial: TerminalSize::default(),
            events: 0,
            bytes: 0,
            last_us: 0,
            done: false,
        };
        let Entry::Header { version: 2, size } = this.read_line()? else {
            bail!("timed recording is missing a version 2 header");
        };
        validate_size(size)?;
        this.initial = size;
        Ok(this)
    }

    fn read_line(&mut self) -> Result<Entry> {
        self.line.clear();
        let len = (&mut self.reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut self.line)?;
        ensure!(len <= MAX_LINE, "timed recording index line is too long");
        ensure!(
            self.line.last() == Some(&b'\n'),
            "incomplete timed recording index"
        );
        serde_json::from_slice(&self.line).context("invalid timed recording index entry")
    }

    fn next(&mut self) -> Result<Entry> {
        let entry = self.read_line()?;
        let at_us = match &entry {
            Entry::Header { .. } => bail!("unexpected second timed recording header"),
            Entry::End {
                at_us,
                events,
                bytes,
                sha256,
            } => {
                ensure!(
                    *at_us >= self.last_us && *at_us <= MAX_US,
                    "invalid timed recording duration"
                );
                ensure!(
                    *events == self.events && *bytes == self.bytes,
                    "timed recording footer counts do not match"
                );
                ensure!(
                    sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid timed recording SHA-256"
                );
                ensure!(
                    self.reader.fill_buf()?.is_empty(),
                    "trailing timed recording entries after footer"
                );
                self.done = true;
                return Ok(entry);
            }
            Entry::Output { at_us, len } => {
                ensure!(
                    (1..=OUTPUT_CHUNK).contains(len),
                    "invalid timed recording output length"
                );
                self.bytes += *len as u64;
                ensure!(self.bytes <= MAX_BYTES, "timed recording exceeds 256 MiB");
                *at_us
            }
            Entry::Resize { at_us, size } => {
                validate_size(*size)?;
                *at_us
            }
        };
        ensure!(
            at_us >= self.last_us && at_us <= MAX_US,
            "invalid timed recording timestamp"
        );
        ensure!(
            self.events < MAX_EVENTS,
            "timed recording exceeds its event limit"
        );
        self.last_us = at_us;
        self.events += 1;
        Ok(entry)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReplayEvent {
    Output(Vec<u8>),
    Resize(TerminalSize),
}

pub(crate) struct TimedReplay {
    raw: File,
    timeline: Timeline,
    duration: Duration,
}

impl TimedReplay {
    /// Read only the bounded header when choosing the initial window size.
    pub(crate) fn read_initial_size(path: &Path) -> Result<TerminalSize> {
        Ok(Timeline::open(path)?.initial)
    }

    /// Validate the complete pair before any recorded bytes reach the terminal.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let mut raw = open_read(path, MAX_BYTES)?;
        let mut timeline = Timeline::open(path)?;
        let (digest, duration) = loop {
            if let Entry::End { at_us, sha256, .. } = timeline.next()? {
                break (sha256, Duration::from_micros(at_us));
            }
        };
        ensure!(
            timeline.bytes == raw.metadata()?.len(),
            "timed recording length does not match the VT capture"
        );
        let mut hash = Sha256::new();
        let mut buf = [0; OUTPUT_CHUNK];
        let mut remaining = timeline.bytes;
        while remaining > 0 {
            let len = remaining.min(buf.len() as u64) as usize;
            raw.read_exact(&mut buf[..len])?;
            hash.update(&buf[..len]);
            remaining -= len as u64;
        }
        ensure!(
            hex_digest(hash).eq_ignore_ascii_case(&digest),
            "timed recording SHA-256 does not match the VT capture"
        );
        raw.rewind()?;
        let mut index = timeline.reader.into_inner();
        index.rewind()?;
        Ok(Self {
            raw,
            timeline: Timeline::from_file(index)?,
            duration,
        })
    }

    pub(crate) fn initial_size(&self) -> TerminalSize {
        self.timeline.initial
    }

    pub(crate) fn duration(&self) -> Duration {
        self.duration
    }

    pub(crate) fn next_event(&mut self) -> Result<Option<(Duration, ReplayEvent)>> {
        if self.timeline.done {
            return Ok(None);
        }
        let (at_us, event) = match self.timeline.next()? {
            Entry::Output { at_us, len } => {
                let mut bytes = vec![0; len];
                self.raw.read_exact(&mut bytes)?;
                (at_us, ReplayEvent::Output(bytes))
            }
            Entry::Resize { at_us, size } => (at_us, ReplayEvent::Resize(size)),
            Entry::End { .. } => return Ok(None),
            Entry::Header { .. } => unreachable!("Timeline::next rejects repeated headers"),
        };
        Ok(Some((Duration::from_micros(at_us), event)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::Recorder;
    use std::fs;

    fn sample(path: &Path) {
        let mut recording = Recorder::start_timed(path, TerminalSize::default()).unwrap();
        recording.write(b"A").unwrap();
        recording.write(b"B").unwrap();
        recording.finish().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn writer_preserves_raw_bytes_legacy_metadata_and_timed_geometry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.vt");
        let initial = TerminalSize {
            cols: 116,
            rows: 33,
            ..TerminalSize::default()
        };
        let changed = TerminalSize {
            pixel_width: 928,
            pixel_height: 594,
            dpi: 144,
            ..initial
        };
        let mut recording = Recorder::start_timed(&path, initial).unwrap();
        tokio::time::advance(Duration::from_micros(317)).await;
        recording.write(b"\x1b\0\xff").unwrap();
        recording.write(b"").unwrap();
        recording.resize(initial).unwrap();
        tokio::time::advance(Duration::from_millis(98)).await;
        recording.resize(changed).unwrap();
        recording.resize(changed).unwrap();
        recording.write(b"second").unwrap();
        tokio::time::advance(Duration::from_secs(2)).await;
        recording.finish().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"\x1b\0\xffsecond");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(path.with_extension("vt.json")).unwrap()
            )
            .unwrap(),
            serde_json::json!({"version":1,"cols":116,"rows":33,"output_only":true})
        );
        assert_eq!(TimedReplay::read_initial_size(&path).unwrap(), initial);
        let mut replay = TimedReplay::open(&path).unwrap();
        assert_eq!(replay.initial_size(), initial);
        assert_eq!(replay.duration(), Duration::from_micros(2_098_317));
        assert_eq!(
            replay.next_event().unwrap(),
            Some((
                Duration::from_micros(317),
                ReplayEvent::Output(b"\x1b\0\xff".to_vec())
            ))
        );
        let t = Duration::from_micros(98_317);
        assert_eq!(
            replay.next_event().unwrap(),
            Some((t, ReplayEvent::Resize(changed)))
        );
        assert_eq!(
            replay.next_event().unwrap(),
            Some((t, ReplayEvent::Output(b"second".to_vec())))
        );
        assert_eq!(replay.next_event().unwrap(), None);
        assert_eq!(replay.next_event().unwrap(), None);
        let silent = dir.path().join("silent.vt");
        let writer = Recorder::start_timed(&silent, initial).unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        writer.finish().unwrap();
        let mut replay = TimedReplay::open(&silent).unwrap();
        assert_eq!(replay.duration(), Duration::from_secs(5));
        assert_eq!(replay.next_event().unwrap(), None);
    }

    #[test]
    fn verifies_chunk_bounds_empty_captures_and_same_length_substitutions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.vt");
        let mut recording = Recorder::start_timed(&path, TerminalSize::default()).unwrap();
        let bytes = vec![0xfe; OUTPUT_CHUNK + 7];
        recording.write(&bytes).unwrap();
        recording.finish().unwrap();
        let mut replay = TimedReplay::open(&path).unwrap();
        let (at, first) = replay.next_event().unwrap().unwrap();
        assert_eq!(first, ReplayEvent::Output(bytes[..OUTPUT_CHUNK].to_vec()));
        assert_eq!(
            replay.next_event().unwrap(),
            Some((at, ReplayEvent::Output(vec![0xfe; 7])))
        );
        drop(replay);
        fs::write(&path, vec![0; bytes.len()]).unwrap();
        assert!(TimedReplay::open(&path).is_err());

        let empty = dir.path().join("empty.vt");
        Recorder::start_timed(&empty, TerminalSize::default())
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(
            TimedReplay::open(&empty).unwrap().next_event().unwrap(),
            None
        );
        let large = File::options().write(true).open(&empty).unwrap();
        large.set_len(MAX_BYTES + 1).unwrap();
        drop(large);
        assert!(TimedReplay::open(&empty).is_err());
    }

    #[test]
    fn rejects_invalid_or_incomplete_indexes_before_emitting_any_output() {
        use serde_json::{Value, json};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.vt");
        sample(&path);
        let index = path.with_extension("vt.jsonl");
        let input = fs::read_to_string(&index).unwrap();
        let rows: Vec<Value> = input
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        for (row, field, replacement) in [
            (0, "version", json!(3)),
            (
                0,
                "size",
                json!({"cols":4096,"rows":2048,"pixel_width":0,"pixel_height":0,"dpi":0}),
            ),
            (1, "at_us", json!(MAX_US + 1)),
            (1, "len", json!(0)),
            (1, "len", json!(OUTPUT_CHUNK + 1)),
            (1, "kind", json!("input")),
            (1, "ignored", json!(true)),
            (3, "bytes", json!(1)),
            (3, "events", json!(1)),
            (3, "sha256", json!("f".repeat(64))),
            (3, "at_us", json!(MAX_US + 1)),
        ] {
            let mut altered = rows.clone();
            altered[row][field] = replacement;
            let mut encoded = altered
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            encoded.push('\n');
            fs::write(&index, encoded).unwrap();
            assert!(
                TimedReplay::open(&path).is_err(),
                "accepted invalid {row}:{field}"
            );
        }
        let header = format!("{}\n", rows[0]);
        let footer = format!("{}\n", rows[3]);
        for invalid in [
            String::new(),
            header.clone(),
            input.trim_end().to_string(),
            format!("{input}\n"),
            format!("{header}{header}{footer}"),
            format!("{header}{}\n", " ".repeat(MAX_LINE + 1)),
            format!(
                "{header}{{\"kind\":\"output\",\"at_us\":2,\"len\":1}}\n{{\"kind\":\"output\",\"at_us\":1,\"len\":1}}\n{footer}"
            ),
        ] {
            fs::write(&index, invalid).unwrap();
            assert!(TimedReplay::open(&path).is_err());
        }
    }

    #[test]
    fn timed_start_rolls_back_only_new_paths_and_failed_recordings_have_no_footer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture.vt");
        let legacy = path.with_extension("vt.json");
        let index = path.with_extension("vt.jsonl");
        for collision in [&index, &legacy, &path] {
            fs::write(collision, b"existing").unwrap();
            assert!(Recorder::start_timed(&path, TerminalSize::default()).is_err());
            for candidate in [&index, &legacy, &path] {
                if candidate == collision {
                    assert_eq!(fs::read(candidate).unwrap(), b"existing");
                } else {
                    assert!(!candidate.exists());
                }
            }
            fs::remove_file(collision).unwrap();
        }
        let invalid = TerminalSize {
            rows: 0,
            ..TerminalSize::default()
        };
        assert!(Recorder::start_timed(&path, invalid).is_err());
        assert!(!path.exists() && !legacy.exists() && !index.exists());
        let mut writer = Recorder::start_timed(&path, TerminalSize::default()).unwrap();
        writer.write(b"first").unwrap();
        assert!(writer.resize(invalid).is_err());
        assert!(writer.finish().is_err());
        assert!(TimedReplay::open(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"first");
    }
}
