use super::{
    CpuProfileOptions, CpuProfileSummary, CpuWorkloadOutcome, EVENT,
    launch::{CpuLaunchOptions, CpuLaunchPlan},
    raw,
};
use crate::{
    cpu_profile::MAX_FILE_BYTES,
    sftp::{TransferCancel, local_sha256_with_size, normalize_sha256},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{fs::File, io::Read, path::Path};

const SCHEMA_VERSION: u8 = 1;
const MAX_REPORT_BYTES: u64 = 256 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Kind {
    CpuProfileCapture,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub host_key_fingerprint: String,
    pub event: String,
    pub target: Target,
    pub raw_perf_data_requested: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Target {
    Process {
        process_name: String,
        process_start_ticks: u64,
        options: CpuProfileOptions,
    },
    Command {
        program: String,
        directory: String,
        options: CpuLaunchOptions,
    },
}

impl Review {
    pub(super) fn process(
        (host, user, port, fingerprint): (&str, &str, u16, &str),
        options: CpuProfileOptions,
        process_name: String,
        process_start_ticks: u64,
        raw_perf_data_requested: bool,
    ) -> Self {
        Self {
            host: host.into(),
            user: user.into(),
            port,
            host_key_fingerprint: fingerprint.into(),
            event: EVENT.into(),
            target: Target::Process {
                process_name,
                process_start_ticks,
                options,
            },
            raw_perf_data_requested,
        }
    }

    pub(super) fn command(plan: &CpuLaunchPlan, raw_perf_data_requested: bool) -> Self {
        Self {
            host: plan.host.clone(),
            user: plan.user.clone(),
            port: plan.port,
            host_key_fingerprint: plan.host_key_fingerprint.clone(),
            event: EVENT.into(),
            target: Target::Command {
                program: plan.program.clone(),
                directory: plan.directory.clone(),
                options: plan.options.clone(),
            },
            raw_perf_data_requested,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub file_name: String,
    pub bytes: u64,
    pub sha256: String,
}

impl Artifact {
    pub(super) fn new(path: &Path, bytes: u64, sha256: String) -> Self {
        Self {
            file_name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            bytes,
            sha256,
        }
    }

    fn validate(&self, label: &str, limit: u64) -> Result<()> {
        ensure!(
            !self.file_name.is_empty() && (1..=limit).contains(&self.bytes),
            "CPU profile report has invalid {label} file details"
        );
        normalize_sha256(&self.sha256).with_context(|| format!("Invalid {label} SHA-256"))?;
        Ok(())
    }

    fn verify(&self, path: &Path, label: &str) -> Result<String> {
        let expected =
            normalize_sha256(&self.sha256).with_context(|| format!("Invalid {label} SHA-256"))?;
        let found = local_sha256_with_size(path, Some(self.bytes), &TransferCancel::default())
            .with_context(|| format!("Checking the {label} file"))?;
        ensure!(
            found == expected,
            "{label} SHA-256 differs from the CPU profile report"
        );
        Ok(found)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CpuProfileCaptureReport {
    schema_version: u8,
    kind: Kind,
    pub folded: Artifact,
    pub reviewed: Review,
    pub samples: u64,
    pub distinct_stacks: usize,
    pub unresolved_frames: u64,
    pub stopped_by_user: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workload: Option<CpuWorkloadOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_perf_data: Option<Artifact>,
}

impl CpuProfileCaptureReport {
    pub(super) fn new(folded: Artifact, reviewed: Review, summary: &CpuProfileSummary) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            kind: Kind::CpuProfileCapture,
            folded,
            reviewed,
            samples: summary.samples,
            distinct_stacks: summary.distinct_stacks,
            unresolved_frames: summary.unresolved_frames,
            stopped_by_user: summary.stopped_by_user,
            workload: summary.workload,
            raw_perf_data: summary
                .perf_data
                .as_ref()
                .map(|raw| Artifact::new(&raw.path, raw.bytes as u64, raw.sha256.clone())),
        }
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SCHEMA_VERSION,
            "Unsupported CPU profile report version {}",
            self.schema_version
        );
        self.folded.validate("Folded profile", MAX_FILE_BYTES)?;
        ensure!(
            self.distinct_stacks > 0 && self.distinct_stacks as u64 <= self.samples,
            "CPU profile report has invalid sample counts"
        );
        match &self.reviewed.target {
            Target::Process { options, .. } => {
                options.validate()?;
                ensure!(
                    self.workload.is_none(),
                    "PID profiles have no command outcome"
                );
            }
            Target::Command { options, .. } => {
                options.validate()?;
                let outcome = self
                    .workload
                    .context("Command outcome missing from CPU profile report")?;
                ensure!(
                    matches!(outcome, CpuWorkloadOutcome::Stopped) == self.stopped_by_user,
                    "CPU profile report has an inconsistent command outcome"
                );
            }
        }
        if let Some(raw) = &self.raw_perf_data {
            ensure!(
                self.reviewed.raw_perf_data_requested,
                "CPU profile report lists an unexpected raw perf recording"
            );
            raw.validate("Raw perf recording", raw::MAX_BYTES as u64)?;
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let mut contents = serde_json::to_string_pretty(self)?;
        contents.push('\n');
        ensure!(
            contents.len() as u64 <= MAX_REPORT_BYTES,
            "CPU profile report exceeds 256 KiB"
        );
        crate::transcript::save(path, &contents, false).context("Saving new CPU profile report")
    }

    /// Check a raw recording against the digest stored in the selected report.
    pub fn verify_raw_recording(&self, path: &Path) -> Result<()> {
        let artifact = self
            .raw_perf_data
            .as_ref()
            .context("The CPU profile report has no saved raw perf recording")?;
        artifact.validate("Raw perf recording", raw::MAX_BYTES as u64)?;
        artifact.verify(path, "Raw perf recording").map(|_| ())
    }
}

/// Verify the bytes of the selected folded profile and, when supplied, its raw recording.
pub fn verify_cpu_profile_report(
    profile: &Path,
    report: &Path,
    perf_data: Option<&Path>,
) -> Result<CpuProfileCaptureReport> {
    let file = File::open(report).context("Opening the CPU profile report")?;
    ensure!(
        file.metadata()?.is_file(),
        "Choose a CPU profile report file"
    );
    let mut contents = Vec::new();
    file.take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut contents)
        .context("Reading the CPU profile report")?;
    ensure!(
        contents.len() as u64 <= MAX_REPORT_BYTES,
        "CPU profile report exceeds 256 KiB"
    );
    let mut report: CpuProfileCaptureReport =
        serde_json::from_slice(&contents).context("Reading the CPU profile report JSON")?;
    report.validate()?;
    report.folded.sha256 = report.folded.verify(profile, "Folded profile")?;
    if let Some(path) = perf_data {
        let artifact = report
            .raw_perf_data
            .as_mut()
            .context("The CPU profile report has no saved raw perf recording")?;
        artifact.sha256 = artifact.verify(path, "Raw perf recording")?;
    }
    Ok(report)
}
