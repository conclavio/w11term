use super::{Capture, Plan, SyscallOptions};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

const SCHEMA_VERSION: u8 = 1;
const MAX_REPORT_BYTES: u64 = 256 * 1024;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyscallCaptureReportReview {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub host_key_fingerprint: String,
    pub program: String,
    pub directory: String,
    pub options: SyscallOptions,
    pub strace_arguments: Vec<String>,
}

impl From<&Plan> for SyscallCaptureReportReview {
    fn from(plan: &Plan) -> Self {
        Self {
            host: plan.host.clone(),
            user: plan.user.clone(),
            port: plan.port,
            host_key_fingerprint: plan.host_key_fingerprint.clone(),
            program: plan.program.clone(),
            directory: plan.directory.clone(),
            options: plan.options.clone(),
            strace_arguments: plan.strace_arguments.clone(),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ReportKind {
    SyscallCapture,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyscallCaptureReport {
    schema_version: u8,
    kind: ReportKind,
    pub file_name: String,
    pub bytes: u64,
    pub sha256: String,
    pub stop_reason: String,
    pub command_exit_status: Option<u8>,
    pub reviewed: SyscallCaptureReportReview,
}

impl Capture {
    pub fn save_report(&self, path: &Path) -> Result<()> {
        let report = SyscallCaptureReport {
            schema_version: SCHEMA_VERSION,
            kind: ReportKind::SyscallCapture,
            file_name: self
                .path
                .file_name()
                .context("The trace has no file name")?
                .to_string_lossy()
                .into_owned(),
            bytes: self.bytes,
            sha256: self.sha256.clone(),
            stop_reason: self.stop_reason.into(),
            command_exit_status: self.command_exit_status,
            reviewed: (*self.reviewed).clone(),
        };
        crate::transcript::save_with(path, false, |file| {
            serde_json::to_writer_pretty(&mut *file, &report)?;
            file.write_all(b"\n")
        })
        .context("Saving new syscall capture report")
    }
}

/// Verify that the selected trace contains the bytes named in the report.
pub fn verify_syscall_capture_report(trace: &Path, report: &Path) -> Result<SyscallCaptureReport> {
    let file = File::open(report).context("Opening the syscall capture report")?;
    ensure!(
        file.metadata()?.is_file(),
        "Choose a syscall capture report file"
    );
    let mut bytes = Vec::new();
    file.take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("Reading the syscall capture report")?;
    ensure!(
        bytes.len() as u64 <= MAX_REPORT_BYTES,
        "Syscall capture report exceeds 256 KiB"
    );
    let mut report: SyscallCaptureReport =
        serde_json::from_slice(&bytes).context("Reading the syscall capture report JSON")?;
    ensure!(
        report.schema_version == SCHEMA_VERSION,
        "Unsupported syscall capture report version {}",
        report.schema_version
    );
    let maximum = report.reviewed.options.clone().validate()?.max_bytes();
    ensure!(
        (1..=maximum).contains(&report.bytes),
        "Syscall capture report has an invalid byte count"
    );
    match (report.stop_reason.as_str(), report.command_exit_status) {
        ("command-exited", Some(_)) | ("stopped" | "time-limit", None) => {}
        ("byte-budget", None) if report.bytes == maximum => {}
        _ => anyhow::bail!("Syscall capture report has an invalid capture outcome"),
    }
    let expected = crate::sftp::normalize_sha256(&report.sha256)
        .context("Invalid syscall capture report SHA-256")?;
    report.sha256 = crate::sftp::local_sha256_with_size(
        trace,
        Some(report.bytes),
        &crate::sftp::TransferCancel::default(),
    )?;
    ensure!(
        report.sha256 == expected,
        "Trace SHA-256 differs from the syscall capture report"
    );
    Ok(report)
}
