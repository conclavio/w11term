//! Output-only VT captures. Input and authentication answers are never recorded.
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};
pub struct Recorder {
    file: BufWriter<File>,
    pub path: PathBuf,
    pub bytes: u64,
}
impl Recorder {
    pub fn start(path: &Path, size: wezterm_term::TerminalSize) -> Result<Self> {
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(path)
            .context("creating new VT capture (existing files are never overwritten)")?;
        let metadata = path.with_extension("vt.json");
        if let Err(error) = (|| -> Result<()> {
            let mut sidecar = File::options()
                .write(true)
                .create_new(true)
                .open(metadata)?;
            serde_json::to_writer_pretty(
                &mut sidecar,
                &serde_json::json!({"version":1,"cols":size.cols,"rows":size.rows,"output_only":true}),
            )?;
            Ok(())
        })() {
            drop(file);
            let _ = std::fs::remove_file(path);
            return Err(error);
        }
        Ok(Self {
            file: BufWriter::new(file),
            path: path.into(),
            bytes: 0,
        })
    }
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.bytes + bytes.len() as u64 <= 256 * 1024 * 1024,
            "VT recording reached 256 MiB limit"
        );
        self.file.write_all(bytes)?;
        self.bytes += bytes.len() as u64;
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
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
