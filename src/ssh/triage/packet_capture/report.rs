use super::{Capture, PacketCaptureOptions, Plan, Rotation, RotationResult};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    fs::File,
    io::{Read, Write},
    path::Path,
};

const SCHEMA_VERSION: u8 = 1;
const MAX_REPORT_BYTES: u64 = 256 * 1024;

#[derive(Deserialize)]
struct ReportDigest {
    schema_version: u8,
    bytes: u64,
    sha256: String,
}

pub struct CaptureReportVerification {
    pub bytes: u64,
    pub sha256: String,
}

pub fn verify_packet_capture_report(
    capture: &Path,
    report: &Path,
) -> Result<CaptureReportVerification> {
    let file = File::open(report).context("Opening the packet capture report")?;
    ensure!(
        file.metadata()?.is_file(),
        "Choose a packet capture report file"
    );
    let mut data = Vec::new();
    file.take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut data)
        .context("Reading the packet capture report")?;
    ensure!(
        data.len() as u64 <= MAX_REPORT_BYTES,
        "Packet capture report exceeds 256 KiB"
    );
    let digest: ReportDigest =
        serde_json::from_slice(&data).context("Reading the packet capture report JSON")?;
    ensure!(
        digest.schema_version == SCHEMA_VERSION,
        "Unsupported packet capture report version {}",
        digest.schema_version
    );
    let maximum = u64::from(PacketCaptureOptions::MAX_MIB) * 1024 * 1024;
    ensure!(
        (24..=maximum).contains(&digest.bytes),
        "Packet capture report has an invalid byte count"
    );
    let expected = crate::sftp::normalize_sha256(&digest.sha256)
        .context("Invalid packet capture report SHA-256")?;
    let sha256 = crate::sftp::local_sha256_with_size(
        capture,
        Some(digest.bytes),
        &crate::sftp::TransferCancel::default(),
    )?;
    ensure!(
        sha256 == expected,
        "Capture SHA-256 differs from the packet capture report"
    );
    Ok(CaptureReportVerification {
        bytes: digest.bytes,
        sha256,
    })
}

#[derive(Clone, Serialize)]
pub(super) struct Reviewed {
    host: String,
    user: String,
    port: u16,
    host_key_fingerprint: String,
    options: PacketCaptureOptions,
    scratch_directory: String,
    scratch_filesystem: String,
    effective_packet_limit: Option<u32>,
    rotation: Option<Rotation>,
    tcpdump_arguments: Vec<String>,
}

impl From<&Plan> for Reviewed {
    fn from(plan: &Plan) -> Self {
        Self {
            host: plan.host.clone(),
            user: plan.user.clone(),
            port: plan.port,
            host_key_fingerprint: plan.host_key_fingerprint.clone(),
            options: plan.options.clone(),
            scratch_directory: plan.scratch_directory.clone(),
            scratch_filesystem: plan.scratch_filesystem.clone(),
            effective_packet_limit: plan.effective_packet_limit,
            rotation: plan.rotation,
            tcpdump_arguments: plan.tcpdump_arguments.clone(),
        }
    }
}

#[derive(Serialize)]
struct Report<'a> {
    schema_version: u8,
    file_name: Cow<'a, str>,
    bytes: u64,
    sha256: &'a str,
    packets: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    rotation: Option<&'a RotationResult>,
    stop_reason: &'static str,
    received_by_filter: Option<u64>,
    dropped_by_kernel: Option<u64>,
    diagnostics: &'a [String],
    reviewed: &'a Reviewed,
}

impl Capture {
    pub fn save_report(&self, path: &Path) -> Result<()> {
        let report = Report {
            schema_version: SCHEMA_VERSION,
            file_name: self
                .path
                .file_name()
                .context("The capture has no file name")?
                .to_string_lossy(),
            bytes: self.bytes,
            sha256: &self.sha256,
            packets: self.packets,
            rotation: self.rotation.as_ref(),
            stop_reason: self.stop_reason,
            received_by_filter: self.received_by_filter,
            dropped_by_kernel: self.dropped_by_kernel,
            diagnostics: &self.diagnostics,
            reviewed: &self.reviewed,
        };
        crate::transcript::save_with(path, false, |file| {
            serde_json::to_writer_pretty(&mut *file, &report)?;
            file.write_all(b"\n")
        })
        .context("Saving new packet capture report")
    }
}
