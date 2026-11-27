use super::{Target, diagnostic_destination, quote};
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{BufWriter, Write},
    num::NonZeroU32,
    path::{Path, PathBuf},
    time::Duration,
};

const SUPERVISOR: &str = include_str!("cpu-profile-supervisor.sh");
const WORKER: &str = concat!(
    include_str!("cpu-perf-data.sh"),
    include_str!("cpu-profile-worker.sh")
);
const MAX_SECONDS: u8 = 30;
const RECORD_KIB: usize = 32 * 1024;
const STACKS_KIB: usize = 4 * 1024;
const PROCESS_WIRE_BYTES: usize = 4096;
const EXTRA_WORK_SECONDS: u64 = 12;
const SSH_MARGIN_SECONDS: u64 = 6;
const PROBE_SECONDS: u64 = 4;
const END_MARKER: &[u8] = b"\nw11term-cpu-profile-end\n";
pub(crate) mod launch;
mod raw;
pub use launch::CpuWorkloadOutcome;
pub use raw::PerfDataArtifact;
pub const PERF_DATA_TRANSFER_SECONDS: u64 = 60;
pub(crate) const CONTROL_TIMEOUT_SECONDS: u64 =
    MAX_SECONDS as u64 + EXTRA_WORK_SECONDS + SSH_MARGIN_SECONDS + 20;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CpuProfileOptions {
    pub target_pid: NonZeroU32,
    pub seconds: u8,
    pub frequency: u8,
    pub call_graph: CpuCallGraph,
}

impl CpuProfileOptions {
    pub const DEFAULT_SECONDS: u8 = 10;
    pub const DEFAULT_FREQUENCY: u8 = 99;

    pub fn validate(self) -> Result<Self> {
        ensure!(
            i32::try_from(self.target_pid.get()).is_ok(),
            "Invalid Linux PID"
        );
        validate_sampling(self.seconds, self.frequency)?;
        Ok(self)
    }

    fn worker_seconds(self) -> u64 {
        u64::from(self.seconds) + EXTRA_WORK_SECONDS
    }
}

fn validate_sampling(seconds: u8, frequency: u8) -> Result<()> {
    ensure!(
        (1..=MAX_SECONDS).contains(&seconds),
        "CPU profile duration must be 1..={MAX_SECONDS} seconds"
    );
    ensure!(
        (1..=199).contains(&frequency),
        "CPU profile frequency must be 1..=199 Hz"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CpuCallGraph {
    #[default]
    Fp,
    Dwarf,
}

impl CpuCallGraph {
    fn perf_argument(self) -> &'static str {
        match self {
            Self::Fp => "fp",
            Self::Dwarf => "dwarf,8192",
        }
    }
}

#[derive(Clone, Serialize)]
pub struct CpuProfilePlan {
    pub connection_generation: u64,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub host_key_fingerprint: String,
    pub process_name: String,
    pub process_start_ticks: u64,
    pub options: CpuProfileOptions,
    pub event: &'static str,
    pub deadline_seconds: u64,
    pub max_recording_bytes: usize,
    pub max_stack_output_bytes: usize,
}

impl Target {
    pub fn preview_cpu_profile_blocking(
        self,
        options: CpuProfileOptions,
    ) -> Result<CpuProfilePlan> {
        crate::ssh::NETWORK.block_on(self.preview_cpu_profile(options))
    }

    pub fn save_cpu_profile_blocking(
        self,
        plan: &CpuProfilePlan,
        path: &Path,
    ) -> Result<CpuProfileSummary> {
        self.save_cpu_profile_artifacts_blocking(plan, path, None)
    }

    pub fn save_cpu_profile_artifacts_blocking(
        self,
        plan: &CpuProfilePlan,
        path: &Path,
        perf_data: Option<&Path>,
    ) -> Result<CpuProfileSummary> {
        let (host, user, port, fingerprint) = self.link.verified_endpoint();
        ensure!(
            plan.connection_generation == self.generation
                && (host, user, port, fingerprint)
                    == (
                        plan.host.as_str(),
                        plan.user.as_str(),
                        plan.port,
                        plan.host_key_fingerprint.as_str(),
                    ),
            "The reviewed SSH connection changed. Review the process again."
        );
        Recording::check_destinations(path, perf_data)?;
        crate::ssh::NETWORK
            .block_on(self.capture_cpu_profile(
                plan.options,
                plan.process_start_ticks,
                perf_data.map(Path::to_owned),
            ))?
            .save(path)
    }

    pub(crate) async fn preview_cpu_profile(
        mut self,
        options: CpuProfileOptions,
    ) -> Result<CpuProfilePlan> {
        let options = options.validate()?;
        let (bytes, _) = self
            .run_diagnostic(
                "Linux CPU profile preview",
                command(options, None, false),
                Duration::from_secs(PROBE_SECONDS + SSH_MARGIN_SECONDS),
                PROCESS_WIRE_BYTES,
            )
            .await?;
        let (ticks, name, remainder) = process_header(&bytes)?;
        ensure!(
            remainder.is_empty(),
            "Unexpected CPU profile preview output"
        );
        let (host, user, port, fingerprint) = self.link.verified_endpoint();
        Ok(CpuProfilePlan {
            connection_generation: self.generation,
            host: host.into(),
            user: user.into(),
            port,
            host_key_fingerprint: fingerprint.into(),
            process_name: crate::events::display_field(&name),
            process_start_ticks: ticks,
            options,
            event: "cpu-clock:u",
            deadline_seconds: options.worker_seconds() + SSH_MARGIN_SECONDS,
            max_recording_bytes: RECORD_KIB * 1024,
            max_stack_output_bytes: STACKS_KIB * 1024,
        })
    }

    pub(crate) async fn capture_cpu_profile(
        mut self,
        options: CpuProfileOptions,
        expected_ticks: u64,
        perf_data: Option<PathBuf>,
    ) -> Result<Recording> {
        let options = options.validate()?;
        let raw = perf_data.is_some();
        let deadline = options.worker_seconds() + transfer_seconds(raw);
        let (bytes, stderr, perf_data) = self
            .read_profile_output(
                "Linux CPU profile",
                command(options, Some(expected_ticks), raw),
                Duration::from_secs(deadline + SSH_MARGIN_SECONDS),
                STACKS_KIB * 1024 + PROCESS_WIRE_BYTES + END_MARKER.len(),
                perf_data,
            )
            .await?;
        let (ticks, name, remainder) = process_header(&bytes)?;
        ensure!(
            ticks == expected_ticks,
            "The CPU profile process identity changed"
        );
        let samples = remainder
            .strip_suffix(END_MARKER)
            .context("The CPU profile response is incomplete")?;
        let stderr = String::from_utf8_lossy(&stderr);
        let diagnostics = stderr
            .lines()
            .map(crate::events::display_field)
            .filter(|line| !line.is_empty())
            .collect();
        let folded = fold(&String::from_utf8_lossy(samples), options.target_pid, &name)
            .with_context(|| {
                let message = crate::events::display_field(&stderr);
                if message.is_empty() {
                    "Reading perf script samples".to_string()
                } else {
                    format!("Reading perf script samples ({message})")
                }
            })
            .map(|mut folded| {
                folded.summary.diagnostics = diagnostics;
                folded
            });
        Ok(Recording { folded, perf_data })
    }
}

fn transfer_seconds(raw: bool) -> u64 {
    u64::from(raw) * PERF_DATA_TRANSFER_SECONDS
}

fn command(options: CpuProfileOptions, expected_ticks: Option<u64>, raw: bool) -> String {
    let (mode, deadline) = match (expected_ticks, raw) {
        (Some(_), true) => (
            "capture-raw",
            options.worker_seconds() + PERF_DATA_TRANSFER_SECONDS,
        ),
        (Some(_), false) => ("capture", options.worker_seconds()),
        (None, _) => ("inspect", PROBE_SECONDS),
    };
    format!(
        "/bin/sh -c {} w11term-cpu-profile {} {deadline} {mode} {} {} {} {} {} {RECORD_KIB} {STACKS_KIB}",
        quote(SUPERVISOR),
        quote(WORKER),
        options.target_pid,
        expected_ticks.unwrap_or_default(),
        options.seconds,
        options.frequency,
        options.call_graph.perf_argument(),
    )
}

fn process_header(bytes: &[u8]) -> Result<(u64, String, &[u8])> {
    let boundary = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .context("Missing CPU profile process header")?;
    let header = &bytes[..boundary];
    let remainder = &bytes[boundary + 1..];
    ensure!(
        header.len() <= PROCESS_WIRE_BYTES,
        "CPU profile process header exceeds its byte limit"
    );
    let (ticks, name) = std::str::from_utf8(header)?
        .strip_prefix("w11term-cpu-profile-v1\t")
        .and_then(|header| header.split_once('\t'))
        .context("Invalid CPU profile process header")?;
    let name = STANDARD
        .decode(name)
        .context("Invalid profile process name")?;
    Ok((
        ticks.parse()?,
        String::from_utf8_lossy(&name).into(),
        remainder,
    ))
}

#[derive(Default)]
pub(crate) struct Folded {
    stacks: BTreeMap<String, u64>,
    summary: CpuProfileSummary,
}

#[derive(Default, Serialize)]
pub struct CpuProfileSummary {
    pub samples: u64,
    pub distinct_stacks: usize,
    pub unresolved_frames: u64,
    pub diagnostics: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workload: Option<CpuWorkloadOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub perf_data: Option<PerfDataArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub perf_data_warning: Option<String>,
}

pub(crate) struct Recording {
    folded: Result<Folded>,
    perf_data: Option<raw::Staged>,
}

impl Recording {
    pub(crate) fn check_destinations(folded: &Path, perf_data: Option<&Path>) -> Result<()> {
        if let Some(perf_data) = perf_data {
            raw::distinct_destinations(folded, perf_data)
        } else {
            Folded::check_destination(folded).map(|_| ())
        }
    }

    pub(crate) fn save(self, path: &Path) -> Result<CpuProfileSummary> {
        let folded = self.folded.and_then(|folded| folded.save(path));
        let Some(raw) = self.perf_data else {
            return folded;
        };
        match (folded, raw.save()) {
            (Ok(mut summary), Ok(raw)) => {
                summary.perf_data = Some(raw);
                Ok(summary)
            }
            (Ok(mut summary), Err(error)) => {
                summary.perf_data_warning =
                    Some(format!("Raw perf recording could not be saved. {error:#}"));
                Ok(summary)
            }
            (Err(error), Ok(raw)) => Err(error).with_context(|| {
                format!(
                    "Raw perf recording saved to {}. The folded profile could not be saved",
                    raw.path.display()
                )
            }),
            (Err(error), Err(raw_error)) => Err(error).context(format!(
                "Raw perf recording could not be saved. {raw_error:#}"
            )),
        }
    }
}

impl Folded {
    pub(crate) fn check_destination(path: &Path) -> Result<&Path> {
        diagnostic_destination(path, "folded CPU profile")
    }

    pub(crate) fn save(self, path: &Path) -> Result<CpuProfileSummary> {
        let directory = Self::check_destination(path)?;
        let mut staged = tempfile::NamedTempFile::new_in(directory)?;
        {
            let mut writer = BufWriter::new(staged.as_file_mut());
            for (stack, count) in self.stacks {
                writeln!(writer, "{stack} {count}")?;
            }
            writer.flush()?;
        }
        staged.as_file().sync_all()?;
        staged
            .persist_noclobber(path)
            .map_err(|error| error.error)
            .context("Saving the folded CPU profile")?;
        Ok(self.summary)
    }
}

fn fold(input: &str, pid: impl Into<Option<NonZeroU32>>, process: &str) -> Result<Folded> {
    let pid = pid.into();
    let mut result = Folded::default();
    let process = frame_label(process);
    // perf script emits leaf through caller. Fold samples in caller-to-leaf order.
    let mut stack: Option<Vec<String>> = None;
    for line in input.lines() {
        let text = line.trim();
        if let Some(inline_frame) = sample_header(text, pid) {
            finish_sample(&mut result, &process, stack.take())?;
            let mut frames = Vec::new();
            if !inline_frame.is_empty() {
                push_frame(&mut result, &mut frames, inline_frame)?;
            }
            stack = Some(frames);
        } else if text.is_empty() {
            finish_sample(&mut result, &process, stack.take())?;
        } else {
            let frames = stack
                .as_mut()
                .context("Unexpected perf script sample header")?;
            push_frame(&mut result, frames, text)?;
        }
    }
    finish_sample(&mut result, &process, stack)?;
    ensure!(result.summary.samples > 0, "perf recorded no CPU samples");
    result.summary.distinct_stacks = result.stacks.len();
    Ok(result)
}

fn sample_header(line: &str, pid: Option<NonZeroU32>) -> Option<&str> {
    let (prefix, event) = line.split_once(": ")?;
    let mut parts = prefix.split_ascii_whitespace();
    let reported_pid: NonZeroU32 = parts.next()?.parse().ok()?;
    if pid.is_some_and(|pid| reported_pid != pid)
        || !parts.next()?.contains('.')
        || parts.next().is_some()
    {
        return None;
    }
    let event = event.trim_start();
    event
        .strip_prefix("cpu-clock:u:")
        .or_else(|| event.strip_prefix("cpu-clock:"))
        .map(str::trim)
}

fn push_frame(result: &mut Folded, stack: &mut Vec<String>, frame: &str) -> Result<()> {
    let (address, details) = frame
        .split_once(char::is_whitespace)
        .context("Missing perf script frame address or symbol")?;
    ensure!(
        u64::from_str_radix(address.trim_start_matches("0x"), 16).is_ok(),
        "Invalid perf script frame address"
    );
    let (symbol, dso) = details
        .trim()
        .strip_suffix(')')
        .and_then(|details| details.rsplit_once(" ("))
        .context("Missing perf script frame binary")?;
    ensure!(
        stack.len() < 512,
        "CPU sample exceeds the stack depth limit"
    );
    let unknown = symbol.is_empty()
        || symbol == "[unknown]"
        || symbol
            .strip_prefix("0x")
            .is_some_and(|value| u64::from_str_radix(value, 16).is_ok());
    let label = if unknown {
        result.summary.unresolved_frames += 1;
        format!("[unknown 0x{} in {dso}]", address.trim_start_matches("0x"))
    } else {
        symbol.to_string()
    };
    stack.push(frame_label(&label));
    Ok(())
}

fn frame_label(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return "[unnamed]".into();
    }
    text.chars()
        .map(|character| {
            if character == ';' || character.is_control() {
                '_'
            } else {
                character
            }
        })
        .collect()
}

fn finish_sample(result: &mut Folded, process: &str, stack: Option<Vec<String>>) -> Result<()> {
    if let Some(stack) = stack {
        ensure!(!stack.is_empty(), "CPU sample has no stack frames");
        let mut name = process.to_string();
        for frame in stack.into_iter().rev() {
            name.push(';');
            name.push_str(&frame);
        }
        *result.stacks.entry(name).or_default() += 1;
        result.summary.samples += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_callers_before_leaves_and_counts_identical_stacks() {
        let input = concat!(
            "  42 158.570607: cpu-clock:u: \n",
            "\t    1a2b work(int) (/bin/test)\n",
            "\t    fe01 start (/bin/test)\n\n",
            "  42 158.580607: cpu-clock:u: \n",
            "\t    1a3f work(int) (/bin/test)\n",
            "\t    fe01 start (/bin/test)\n\n",
            "  42 158.590607: cpu-clock:u:     abcd [unknown] ([unknown])\n",
        );
        let result = fold(input, NonZeroU32::new(42).unwrap(), "test;app").unwrap();
        assert_eq!(result.stacks["test_app;start;work(int)"], 2);
        assert_eq!(result.stacks["test_app;[unknown 0xabcd in [unknown]]"], 1);
        assert_eq!(result.summary.samples, 3);
        assert_eq!(result.summary.distinct_stacks, 2);
        assert_eq!(result.summary.unresolved_frames, 1);
    }

    #[test]
    fn rejects_missing_frames_and_samples_for_another_process() {
        let pid = NonZeroU32::new(42).unwrap();
        assert!(fold("", pid, "test").is_err());
        assert!(fold("42 19.8: cpu-clock:u:\n", pid, "test").is_err());
        assert!(fold("43 19.8: cpu-clock:u: 123 abc (test)\n", pid, "test").is_err());
    }
}
