use super::{RECORD_KIB, Target, diagnostic_destination};
use crate::ssh::triage::DiagnosticStdout;
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tempfile::NamedTempFile;
use tokio::io::AsyncWriteExt;

pub(super) const MAX_BYTES: usize = RECORD_KIB * 1024;
const HEADER_BYTES: usize = 128;

#[derive(Serialize)]
pub struct PerfDataArtifact {
    pub path: PathBuf,
    pub bytes: usize,
    pub sha256: String,
}

pub(super) struct Staged {
    temporary: NamedTempFile,
    artifact: PerfDataArtifact,
}

impl Staged {
    pub fn save(self) -> Result<PerfDataArtifact> {
        self.temporary
            .persist_noclobber(&self.artifact.path)
            .map_err(|error| error.error)
            .with_context(|| format!("Saving {}", self.artifact.path.display()))?;
        Ok(self.artifact)
    }
}

struct Receiver {
    temporary: NamedTempFile,
    local: tokio::fs::File,
    destination: PathBuf,
    header: Vec<u8>,
    expected: Option<PerfDataArtifact>,
    remaining: usize,
    hash: Sha256,
    transcript: Vec<u8>,
    transcript_limit: usize,
}

impl Receiver {
    fn new(destination: PathBuf, transcript_limit: usize) -> Result<Self> {
        let parent = diagnostic_destination(&destination, "raw perf recording")?;
        let temporary = NamedTempFile::new_in(parent).context("Staging the raw perf recording")?;
        let local = tokio::fs::File::from_std(temporary.reopen()?);
        Ok(Self {
            temporary,
            local,
            destination,
            header: Vec::new(),
            expected: None,
            remaining: 0,
            hash: Sha256::new(),
            transcript: Vec::new(),
            transcript_limit,
        })
    }

    async fn finish(mut self) -> Result<(Vec<u8>, Staged)> {
        let artifact = self.expected.context("Missing raw perf recording header")?;
        ensure!(self.remaining == 0, "The raw perf recording was truncated");
        ensure!(
            crate::sftp::digest_hex(self.hash) == artifact.sha256,
            "The raw perf recording did not match its SHA-256 digest"
        );
        self.local
            .flush()
            .await
            .context("Writing the raw perf recording")?;
        self.local
            .sync_all()
            .await
            .context("Flushing the raw perf recording")?;
        drop(self.local);
        Ok((
            self.transcript,
            Staged {
                temporary: self.temporary,
                artifact,
            },
        ))
    }
}

impl DiagnosticStdout for Receiver {
    async fn write(&mut self, mut data: &[u8]) -> Result<()> {
        if self.expected.is_none() {
            let newline = data.iter().position(|&byte| byte == b'\n');
            let end = newline.unwrap_or(data.len());
            ensure!(
                self.header.len() + end < HEADER_BYTES,
                "Raw perf recording header exceeds its byte limit"
            );
            self.header.extend_from_slice(&data[..end]);
            let Some(end) = newline else {
                return Ok(());
            };
            let (size, hash) = std::str::from_utf8(&self.header)?
                .strip_prefix("w11term-perf-data-v1\t")
                .and_then(|line| line.split_once('\t'))
                .context("Invalid raw perf recording header")?;
            let bytes: usize = size
                .trim()
                .parse()
                .context("Invalid raw perf recording size")?;
            ensure!(
                (1..=MAX_BYTES).contains(&bytes),
                "Raw perf recording exceeds its 32 MiB limit or is empty"
            );
            self.expected = Some(PerfDataArtifact {
                path: self.destination.clone(),
                bytes,
                sha256: crate::sftp::normalize_sha256(hash)?,
            });
            self.remaining = bytes;
            data = &data[end + 1..];
        }
        let (raw, text) = data.split_at(self.remaining.min(data.len()));
        if !raw.is_empty() {
            self.local
                .write_all(raw)
                .await
                .context("Writing the raw perf recording")?;
            self.hash.update(raw);
            self.remaining -= raw.len();
        }
        ensure!(
            self.transcript.len() + text.len() <= self.transcript_limit,
            "Remote CPU profile text exceeds its byte limit"
        );
        self.transcript.extend_from_slice(text);
        Ok(())
    }
}

impl Target {
    pub(super) async fn read_profile_output(
        &mut self,
        operation: &str,
        command: String,
        deadline: Duration,
        transcript_limit: usize,
        perf_data: Option<PathBuf>,
    ) -> Result<(Vec<u8>, Vec<u8>, Option<Staged>)> {
        let Some(destination) = perf_data else {
            let (stdout, stderr) = self
                .run_diagnostic(operation, command, deadline, transcript_limit)
                .await?;
            return Ok((stdout, stderr, None));
        };
        let mut receiver =
            tokio::task::spawn_blocking(move || Receiver::new(destination, transcript_limit))
                .await??;
        let stderr = self
            .run_diagnostic_to(
                operation,
                command,
                deadline,
                transcript_limit + MAX_BYTES + HEADER_BYTES,
                &mut receiver,
                None,
            )
            .await?;
        let (stdout, recording) = receiver.finish().await?;
        Ok((stdout, stderr, Some(recording)))
    }
}

pub(super) fn distinct_destinations(folded: &Path, raw: &Path) -> Result<()> {
    let raw_dir = diagnostic_destination(raw, "raw perf recording")?.canonicalize()?;
    let folded_dir = diagnostic_destination(folded, "folded CPU profile")?.canonicalize()?;
    ensure!(
        raw_dir != folded_dir
            || !raw
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .eq_ignore_ascii_case(&folded.file_name().unwrap_or_default().to_string_lossy()),
        "Choose different destinations for the folded and raw CPU profiles"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn streams_fragmented_recording_and_separates_profile_text() {
        let directory = tempfile::tempdir().unwrap();
        let dest = directory.path().join("run.perf.data");
        let data = b"PERFILE2\0\n\r\0stdout";
        let mut digest = Sha256::new();
        digest.update(data);
        let hash = crate::sftp::digest_hex(digest);
        let header = format!("w11term-perf-data-v1\t{}\t{hash}\n", data.len());
        let bytes = [header.as_bytes(), data, b"transcript"].concat();
        let mut receiver = Receiver::new(dest.clone(), 10).unwrap();
        for segment in bytes.chunks(7) {
            receiver.write(segment).await.unwrap();
        }
        let (text, artifact) = receiver.finish().await.unwrap();
        assert_eq!(text, b"transcript");
        assert!(!dest.exists());
        let artifact = artifact.save().unwrap();
        assert_eq!(artifact.sha256, hash);
        assert_eq!(std::fs::read(dest).unwrap(), data);
    }

    #[tokio::test]
    async fn incomplete_corrupted_and_oversize_recordings_leave_no_file() {
        let directory = tempfile::tempdir().unwrap();
        let dest = directory.path().join("run.perf.data");
        for (length, data) in [(4, b"ab".as_slice()), (2, b"ab"), (MAX_BYTES + 1, b"")] {
            let mut receiver = Receiver::new(dest.clone(), 3).unwrap();
            let wire = [
                format!("w11term-perf-data-v1\t{length}\t{}\n", "0".repeat(64)).as_bytes(),
                data,
            ]
            .concat();
            if receiver.write(&wire).await.is_ok() {
                assert!(receiver.finish().await.is_err());
            } else {
                assert!(length > MAX_BYTES);
            }
            assert!(!dest.exists());
        }
    }
}
