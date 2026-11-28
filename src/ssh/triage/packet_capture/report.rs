use super::{Capture, PacketCaptureOptions, Plan, Rotation, RotationResult};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{borrow::Cow, io::Write, path::Path};

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
            schema_version: 1,
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
