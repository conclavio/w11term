use super::{
    CaptureStop, CpuCallGraph, CpuProfileSummary, END_MARKER, EXTRA_WORK_SECONDS, PROBE_SECONDS,
    RECORD_KIB, RUNNER, Recording, SSH_MARGIN_SECONDS, STACKS_KIB, SUPERVISOR, Target, fold, quote,
    report, transfer_seconds, validate_sampling,
};
use crate::ssh::triage::capture_job::{Kind, validate_job};
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const WORKER: &str = concat!(
    include_str!("../cpu-perf-data.sh"),
    include_str!("../cpu-launch-worker.sh")
);
const REVIEW_BYTES: usize = 16 * 1024;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CpuLaunchOptions {
    pub program: String,
    pub arguments: Vec<String>,
    pub directory: String,
    pub seconds: u8,
    pub frequency: u8,
    pub call_graph: CpuCallGraph,
}

impl CpuLaunchOptions {
    pub fn validate(&self) -> Result<()> {
        validate_sampling(self.seconds, self.frequency)?;
        ensure!(!self.program.trim().is_empty(), "Enter a Linux executable");
        ensure!(
            self.directory.is_empty() || self.directory.starts_with('/'),
            "The remote working directory must be an absolute Linux path"
        );
        ensure!(
            self.program.len() <= MAX_PATH_BYTES && self.directory.len() <= MAX_PATH_BYTES,
            "An executable or directory path exceeds 4096 bytes"
        );
        ensure!(self.arguments.len() <= 64, "Use up to 64 command arguments");
        let values = [&self.program, &self.directory]
            .into_iter()
            .chain(&self.arguments);
        let mut bytes = 0;
        for value in values {
            ensure!(!value.contains('\0'), "Linux arguments cannot contain NUL");
            bytes += value.len();
            ensure!(
                bytes <= 8192,
                "Command paths and arguments exceed 8192 bytes"
            );
        }
        Ok(())
    }

    fn worker_seconds(&self) -> u64 {
        u64::from(self.seconds) + EXTRA_WORK_SECONDS
    }
}

#[derive(Clone, Serialize)]
pub struct CpuLaunchPlan {
    pub connection_generation: u64,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub host_key_fingerprint: String,
    pub program: String,
    pub directory: String,
    pub options: CpuLaunchOptions,
    pub deadline_seconds: u64,
    pub max_recording_bytes: usize,
    pub max_stack_output_bytes: usize,
    #[serde(skip)]
    identity: String,
    pub review_id: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(tag = "status", content = "code", rename_all = "kebab-case")]
pub enum CpuWorkloadOutcome {
    Exited(u8),
    TimeLimit,
    Stopped,
}

impl std::fmt::Display for CpuWorkloadOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited(code) => write!(f, "Command exited with status {code}"),
            Self::TimeLimit => f.write_str("Command reached its time limit"),
            Self::Stopped => f.write_str("Command stopped by request"),
        }
    }
}

impl Target {
    pub fn preview_cpu_launch_blocking(
        mut self,
        options: CpuLaunchOptions,
    ) -> Result<CpuLaunchPlan> {
        crate::ssh::NETWORK.block_on(self.preview_cpu_launch(options))
    }

    pub fn save_cpu_launch_blocking(
        self,
        plan: &CpuLaunchPlan,
        path: &Path,
    ) -> Result<CpuProfileSummary> {
        self.save_cpu_launch_artifacts_blocking(plan, path, None)
    }

    pub fn save_cpu_launch_artifacts_blocking(
        self,
        plan: &CpuLaunchPlan,
        path: &Path,
        perf_data: Option<&Path>,
    ) -> Result<CpuProfileSummary> {
        self.save_cpu_launch_artifacts_with_stop_blocking(plan, path, perf_data, None)
    }

    pub fn save_cpu_launch_artifacts_with_stop_blocking(
        self,
        plan: &CpuLaunchPlan,
        path: &Path,
        perf_data: Option<&Path>,
        stop: Option<&CaptureStop>,
    ) -> Result<CpuProfileSummary> {
        let (host, user, port, fingerprint) = self.link.verified_endpoint();
        ensure!(
            plan.connection_generation == self.generation
                && (host, user, port, fingerprint)
                    == (
                        plan.host.as_str(),
                        plan.user.as_str(),
                        plan.port,
                        plan.host_key_fingerprint.as_str()
                    ),
            "The reviewed SSH connection changed. Review the command again."
        );
        Recording::check_destinations(path, perf_data, None)?;
        crate::ssh::NETWORK
            .block_on(self.capture_cpu_launch(plan, perf_data.map(Path::to_owned), stop))?
            .save(path)
    }

    pub(crate) async fn preview_cpu_launch(
        &mut self,
        options: CpuLaunchOptions,
    ) -> Result<CpuLaunchPlan> {
        let (stdout, _) = self
            .run_diagnostic(
                "Linux command profile review",
                command(&options, None, false)?,
                Duration::from_secs(PROBE_SECONDS + SSH_MARGIN_SECONDS),
                REVIEW_BYTES,
            )
            .await?;
        let (resolved, remainder) = command_header(&stdout)?;
        ensure!(remainder.is_empty(), "Unexpected CPU command review output");
        let (host, user, port, fingerprint) = self.link.verified_endpoint();
        let mut plan = CpuLaunchPlan {
            connection_generation: self.generation,
            host: host.into(),
            user: user.into(),
            port,
            host_key_fingerprint: fingerprint.into(),
            program: resolved.program,
            directory: resolved.directory,
            deadline_seconds: options.worker_seconds() + SSH_MARGIN_SECONDS,
            options,
            max_recording_bytes: RECORD_KIB * 1024,
            max_stack_output_bytes: STACKS_KIB * 1024,
            identity: resolved.identity,
            review_id: String::new(),
        };
        let mut digest = Sha256::new_with_prefix(serde_json::to_vec(&plan)?);
        digest.update(plan.identity.as_bytes());
        plan.review_id = crate::sftp::digest_hex(digest);
        Ok(plan)
    }

    pub(crate) async fn capture_reviewed_cpu_launch(
        mut self,
        pane: u64,
        options: CpuLaunchOptions,
        review_id: &str,
        job: &str,
        perf_data: Option<PathBuf>,
    ) -> Result<Recording> {
        let job = validate_job(job)?;
        let review_id = crate::sftp::normalize_sha256(review_id)?;
        let link = self.link.clone();
        let stop = CaptureStop::default();
        let _active =
            link.capture_jobs()
                .register(Kind::CpuCommands, self.generation, pane, job, &stop)?;
        let plan = self.preview_cpu_launch(options).await?;
        ensure!(
            plan.review_id == review_id,
            "The reviewed executable, directory, or capture settings changed. Review the command again."
        );
        ensure!(
            !stop.is_requested(),
            "CPU command capture stopped before launch"
        );
        self.capture_cpu_launch(&plan, perf_data, Some(&stop)).await
    }

    pub(crate) fn stop_cpu_command_capture(&self, pane: u64, job: &str) -> Result<()> {
        self.link
            .capture_jobs()
            .stop(Kind::CpuCommands, self.generation, pane, validate_job(job)?)
    }

    async fn capture_cpu_launch(
        mut self,
        plan: &CpuLaunchPlan,
        perf_data: Option<PathBuf>,
        stop: Option<&CaptureStop>,
    ) -> Result<Recording> {
        let raw = perf_data.is_some();
        let (stdout, stderr, perf_data) = self
            .read_profile_output(
                "Linux command CPU profile",
                command(&plan.options, Some(plan), raw)?,
                Duration::from_secs(
                    plan.options.worker_seconds() + transfer_seconds(raw) + SSH_MARGIN_SECONDS,
                ),
                STACKS_KIB * 1024 + REVIEW_BYTES + END_MARKER.len(),
                perf_data,
                stop,
            )
            .await?;
        let (resolved, remainder) = command_header(&stdout)?;
        ensure!(
            resolved.directory == plan.directory
                && resolved.program == plan.program
                && resolved.identity == plan.identity,
            "The Linux command profile target changed"
        );
        let (outcome, stacks) = workload_result(remainder)?;
        let stacks = stacks
            .strip_suffix(END_MARKER)
            .context("The CPU profile response is incomplete")?;
        let root = resolved
            .program
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&resolved.program);
        let stderr = String::from_utf8_lossy(&stderr);
        let folded = fold(&String::from_utf8_lossy(stacks), None, root)
            .with_context(|| {
                let diagnostics = crate::events::display_field(&stderr);
                format!("{outcome}. Reading perf script samples. {diagnostics}")
                    .trim_end()
                    .to_owned()
            })
            .map(|mut folded| {
                folded.summary.stopped_by_user = matches!(outcome, CpuWorkloadOutcome::Stopped);
                folded.summary.workload = Some(outcome);
                folded.summary.diagnostics = stderr
                    .lines()
                    .map(crate::events::display_field)
                    .filter(|line| !line.is_empty())
                    .collect();
                folded
            });
        let reviewed = report::Review::command(plan, raw);
        Ok(Recording {
            folded,
            perf_data,
            reviewed,
        })
    }
}

fn command(options: &CpuLaunchOptions, plan: Option<&CpuLaunchPlan>, raw: bool) -> Result<String> {
    options.validate()?;
    let (mode, deadline, directory, program, identity) = match plan {
        Some(plan) => (
            if raw { "launch-raw" } else { "launch" },
            options.worker_seconds() + transfer_seconds(raw),
            plan.directory.as_str(),
            plan.program.as_str(),
            plan.identity.as_str(),
        ),
        None => ("inspect", PROBE_SECONDS, "", "", ""),
    };
    let mut command = format!(
        "/bin/sh -c {} w11term-cpu-profile {} {deadline} {mode} {} {} {} {} {RECORD_KIB} {STACKS_KIB} {} {} {} {} {}",
        quote(SUPERVISOR),
        quote(WORKER),
        quote(RUNNER),
        options.seconds,
        options.frequency,
        options.call_graph.perf_argument(),
        quote(&options.directory),
        quote(&options.program),
        quote(directory),
        quote(program),
        quote(identity),
    );
    for argument in &options.arguments {
        command.push(' ');
        command.push_str(&quote(argument));
    }
    ensure!(
        command.len() <= 48 * 1024,
        "The quoted remote command is too large"
    );
    Ok(command)
}

struct ResolvedCommand {
    directory: String,
    program: String,
    identity: String,
}

fn command_header(bytes: &[u8]) -> Result<(ResolvedCommand, &[u8])> {
    let end = bytes
        .iter()
        .position(|&byte| byte == b'\n')
        .filter(|&end| end < REVIEW_BYTES)
        .context("Missing CPU command profile header")?;
    let header = std::str::from_utf8(&bytes[..end])?
        .strip_prefix("w11term-cpu-launch-v2\t")
        .context("Invalid CPU command profile header")?;
    let fields: Vec<_> = header.split('\t').collect();
    let [directory, program, identity] = fields.as_slice() else {
        anyhow::bail!("Invalid CPU command profile header");
    };
    let path = |value| -> Result<String> {
        let path = String::from_utf8(STANDARD.decode(value)?)?;
        ensure!(
            path.starts_with('/') && !path.contains('\0') && path.len() <= MAX_PATH_BYTES,
            "Invalid resolved Linux path"
        );
        Ok(path)
    };
    let identity = String::from_utf8(STANDARD.decode(identity)?)?;
    ensure!(
        !identity.is_empty() && identity.len() <= 160 && !identity.chars().any(char::is_control),
        "Invalid CPU command executable identity"
    );
    Ok((
        ResolvedCommand {
            directory: path(directory)?,
            program: path(program)?,
            identity,
        },
        &bytes[end + 1..],
    ))
}

fn workload_result(bytes: &[u8]) -> Result<(CpuWorkloadOutcome, &[u8])> {
    let end = bytes
        .iter()
        .position(|&byte| byte == b'\n')
        .filter(|&end| end <= 8)
        .context("Missing CPU workload outcome")?;
    let text = std::str::from_utf8(&bytes[..end])?;
    let outcome = match text {
        "timeout" => CpuWorkloadOutcome::TimeLimit,
        "stopped" => CpuWorkloadOutcome::Stopped,
        _ => CpuWorkloadOutcome::Exited(
            text.strip_prefix("exit\t")
                .context("Invalid CPU workload outcome")?
                .parse()?,
        ),
    };
    Ok((outcome, &bytes[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_literal_arguments_and_rejects_nul_and_relative_directories() {
        let mut options = CpuLaunchOptions {
            program: "./cpu workload".into(),
            arguments: vec!["quote'and $text".into(), String::new(), "-Fliteral".into()],
            directory: "/tmp/benchmark".into(),
            seconds: 2,
            frequency: 99,
            call_graph: CpuCallGraph::Fp,
        };
        let line = command(&options, None, false).unwrap();
        assert!(line.ends_with("'quote'\\''and $text' '' '-Fliteral'"));
        options.directory = "relative".into();
        assert!(command(&options, None, false).is_err());
        options.directory.clear();
        options.arguments.push("bad\0arg".into());
        assert!(command(&options, None, false).is_err());
    }

    #[test]
    fn distinguishes_a_literal_exit_124_from_a_timeout_and_folds_children() {
        assert!(matches!(
            workload_result(b"exit\t124\n").unwrap().0,
            CpuWorkloadOutcome::Exited(124)
        ));
        assert!(matches!(
            workload_result(b"timeout\n").unwrap().0,
            CpuWorkloadOutcome::TimeLimit
        ));
        assert!(workload_result(b"exit\t999\n").is_err());
        let input =
            "42 12.50: cpu-clock:u: ab original (prog)\n43 12.50: cpu-clock:u: ab worker (prog)\n";
        let result = fold(input, None, "prog").unwrap();
        assert_eq!(result.stacks["prog;original"], 1);
        assert_eq!(result.stacks["prog;worker"], 1);
    }
}
