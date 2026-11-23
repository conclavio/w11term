//! Output-only VT captures. Input and authentication answers are never recorded.
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
};
use wezterm_term::TerminalSize;

mod timed;
use timed::TimelineWriter;
pub(crate) use timed::{ReplayEvent, TimedReplay};

const MAX_BYTES: u64 = 256 * 1024 * 1024;

fn valid_grid(cols: usize, rows: usize) -> bool {
    (1..=4096).contains(&cols) && (1..=2048).contains(&rows) && cols * rows <= 500_000
}

pub(crate) fn check_raw_file(path: &Path) -> Result<()> {
    let metadata = path
        .metadata()
        .with_context(|| format!("opening {}", path.display()))?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_BYTES,
        "Choose a terminal recording of at most 256 MiB"
    );
    Ok(())
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= limit,
        "Invalid or oversized recording file {}",
        path.display()
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "Recording file exceeds its size limit"
    );
    Ok(bytes)
}

pub(crate) struct RawView {
    pub bytes: Vec<u8>,
    pub grid: Option<(usize, usize)>,
}

impl RawView {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            grid: raw_grid(path)?,
            bytes: read_bounded(path, MAX_BYTES)?,
        })
    }
}

pub(crate) fn raw_grid(path: &Path) -> Result<Option<(usize, usize)>> {
    let sidecar = path.with_extension("vt.json");
    if !sidecar.try_exists()? {
        return Ok(None);
    }
    #[derive(serde::Deserialize)]
    struct Grid {
        version: u32,
        cols: usize,
        rows: usize,
    }
    let grid: Grid = serde_json::from_slice(&read_bounded(&sidecar, 64 * 1024)?)
        .context("reading VT recording geometry")?;
    ensure!(
        grid.version == 1,
        "Unsupported VT recording geometry version"
    );
    ensure!(
        valid_grid(grid.cols, grid.rows),
        "Invalid VT recording geometry"
    );
    Ok(Some((grid.cols, grid.rows)))
}

pub struct Recorder {
    file: BufWriter<File>,
    pub path: PathBuf,
    pub bytes: u64,
    timing: Option<TimelineWriter>,
    failed: bool,
}
impl Recorder {
    pub fn start(path: &Path, size: TerminalSize) -> Result<Self> {
        Self::create(path, size, false)
    }

    pub fn start_timed(path: &Path, size: TerminalSize) -> Result<Self> {
        Self::create(path, size, true)
    }

    fn create(path: &Path, size: TerminalSize, timed: bool) -> Result<Self> {
        let mut created = Vec::new();
        let mut create_file = |path: &Path| -> Result<File> {
            let file = File::options()
                .write(true)
                .create_new(true)
                .open(path)
                .with_context(|| format!("creating new recording file {}", path.display()))?;
            created.push(path.to_owned());
            Ok(file)
        };
        let result = (|| -> Result<Self> {
            let file = create_file(path)?;
            let mut sidecar = create_file(&path.with_extension("vt.json"))?;
            serde_json::to_writer_pretty(
                &mut sidecar,
                &serde_json::json!({"version":1,"cols":size.cols,"rows":size.rows,"output_only":true}),
            )?;
            let timing = if timed {
                Some(TimelineWriter::new(
                    create_file(&path.with_extension("vt.jsonl"))?,
                    size,
                )?)
            } else {
                None
            };
            Ok(Self {
                file: BufWriter::new(file),
                path: path.into(),
                bytes: 0,
                timing,
                failed: false,
            })
        })();
        if result.is_err() {
            for path in created.into_iter().rev() {
                let _ = std::fs::remove_file(path);
            }
        }
        result
    }
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(!self.failed, "VT recording has failed");
        let result = (|| -> Result<()> {
            ensure!(
                self.bytes
                    .checked_add(bytes.len() as u64)
                    .is_some_and(|len| len <= MAX_BYTES),
                "VT recording reached 256 MiB limit"
            );
            if let Some(timing) = &mut self.timing {
                timing.output(bytes)?;
            }
            self.file.write_all(bytes)?;
            self.bytes += bytes.len() as u64;
            Ok(())
        })();
        self.failed = result.is_err();
        result
    }

    pub fn resize(&mut self, size: TerminalSize) -> Result<()> {
        ensure!(!self.failed, "VT recording has failed");
        if let Some(timing) = &mut self.timing {
            let result = timing.resize(size);
            self.failed = result.is_err();
            return result;
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        let stopped = tokio::time::Instant::now();
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
        if let Some(timing) = self.timing {
            ensure!(!self.failed, "timed recording is incomplete");
            timing.finish(self.bytes, stopped)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_a_recording_preserves_existing_capture_or_sidecar() {
        let directory = tempfile::tempdir().unwrap();
        let capture = directory.path().join("session.vt");
        let metadata = capture.with_extension("vt.json");

        std::fs::write(&capture, b"existing capture").unwrap();
        assert!(Recorder::start(&capture, Default::default()).is_err());
        assert_eq!(std::fs::read(&capture).unwrap(), b"existing capture");
        assert!(!metadata.exists());

        std::fs::remove_file(&capture).unwrap();
        std::fs::write(&metadata, b"existing metadata").unwrap();
        assert!(Recorder::start(&capture, Default::default()).is_err());
        assert_eq!(std::fs::read(&metadata).unwrap(), b"existing metadata");
        assert!(!capture.exists(), "failed start left a partial capture");
    }
}
