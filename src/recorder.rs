//! Output-only VT captures. Input and authentication answers are never recorded.
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};
use wezterm_term::TerminalSize;

mod timed;
use timed::TimelineWriter;
pub(crate) use timed::{ReplayEvent, TimedReplay};

const MAX_BYTES: u64 = 256 * 1024 * 1024;

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
