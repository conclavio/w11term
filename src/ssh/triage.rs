use super::{ConnectionShare, ShareSlot};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use capture_job::CaptureStop;
use russh::{Channel, ChannelMsg, client};
use serde::{Deserialize, Serialize};
use std::{io::Read, path::Path, time::Duration};
use tokio::sync::watch;

pub(super) mod capture_job;
mod capture_settings;
pub(crate) mod cpu_profile;
mod findings;
pub(crate) mod packet_capture;
pub(crate) mod syscall_capture;
pub use findings::{Delta, Finding, Resource};

pub(crate) const DEADLINE: Duration = Duration::from_secs(60);
const STREAM_BYTES: usize = 16 * 1024;
const WIRE_BYTES: usize = PROBES.len() * 2 * (STREAM_BYTES.div_ceil(3) * 4) + 4096;
const PROGRESS_PREFIX: &[u8] = b"w11term-triage-progress\t";
const REPORT_BYTES: u64 = 3 * 1024 * 1024;
const SUPERVISOR: &str = include_str!("triage-supervisor.sh");
const WORKER: &str = include_str!("triage-worker.sh");
const PROBES: [(&str, &str, &str); 10] = [
    ("uptime", "uptime", "Load and uptime snapshot"),
    ("dmesg", "dmesg -T | tail -n 50", "Recent kernel messages"),
    (
        "vmstat",
        "vmstat 1 5",
        "First row is since boot, subsequent rows are interval samples",
    ),
    ("mpstat", "mpstat -P ALL 1 5", "Per-CPU interval samples"),
    ("pidstat", "pidstat 1 5", "Per-process interval samples"),
    (
        "iostat",
        "iostat -xz -y 1 5",
        "Extended disk interval samples",
    ),
    ("free", "free -m", "Memory snapshot"),
    ("sar-dev", "sar -n DEV 1 5", "Interface interval samples"),
    ("sar-tcp", "sar -n TCP,ETCP 1 5", "TCP interval samples"),
    ("top", "top -b -n 1 -w 120", "Batch process snapshot"),
];

pub struct Target {
    link: ConnectionShare,
    lease: watch::Receiver<()>,
    generation: u64,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TriageTarget")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Target {
    pub fn connection_generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn new(slot: &ShareSlot, generation: u64) -> Result<Self> {
        let (link, lease) = slot
            .forward_lease(generation)
            .context("The SSH pane has no matching authenticated connection")?;
        Ok(Self {
            link,
            lease,
            generation,
        })
    }

    pub fn preview(&self) -> Plan {
        let (host, user, port, fingerprint) = self.link.verified_endpoint();
        Plan {
            connection_generation: self.generation,
            host: host.into(),
            user: user.into(),
            port,
            host_key_fingerprint: fingerprint.into(),
            deadline_seconds: DEADLINE.as_secs(),
            max_bytes_per_stream: STREAM_BYTES,
            probes: PROBES.map(|(id, command, sampling)| Probe {
                id: id.into(),
                command: command.into(),
                sampling: sampling.into(),
            }),
        }
    }

    pub(crate) async fn collect(self) -> Result<Report> {
        self.collect_report(None, None).await
    }

    async fn collect_report(
        mut self,
        stop: Option<&CaptureStop>,
        progress: Option<watch::Sender<usize>>,
    ) -> Result<Report> {
        let plan = self.preview();
        let mut stdout = ReportStream {
            progress,
            ..ReportStream::default()
        };
        self.run_diagnostic_to(
            "Linux triage",
            command(),
            DEADLINE,
            WIRE_BYTES,
            &mut stdout,
            stop,
        )
        .await?;
        let collection = parse(&stdout.report)?;
        Ok(Report {
            version: 1,
            target: plan,
            findings: findings::summarize(&collection),
            collection,
        })
    }

    pub fn collect_blocking(self) -> Result<Report> {
        super::NETWORK.block_on(self.collect())
    }

    pub fn collect_blocking_with_stop(self, stop: &CaptureStop) -> Result<Report> {
        super::NETWORK.block_on(self.collect_report(Some(stop), None))
    }

    pub fn collect_blocking_with_progress(
        self,
        stop: &CaptureStop,
        progress: watch::Sender<usize>,
    ) -> Result<Report> {
        super::NETWORK.block_on(self.collect_report(Some(stop), Some(progress)))
    }

    async fn run_diagnostic(
        &mut self,
        operation: &str,
        command: String,
        deadline: Duration,
        stdout_limit: usize,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut stdout = Vec::new();
        let stderr = self
            .run_diagnostic_to(
                operation,
                command,
                deadline,
                stdout_limit,
                &mut stdout,
                None,
            )
            .await?;
        Ok((stdout, stderr))
    }

    async fn run_diagnostic_to(
        &mut self,
        operation: &str,
        command: String,
        deadline: Duration,
        stdout_limit: usize,
        stdout: &mut impl DiagnosticStdout,
        stop: Option<&CaptureStop>,
    ) -> Result<Vec<u8>> {
        let _busy = self.link.try_lock_diagnostic()?;
        let request = tokio::time::timeout(
            deadline,
            read_diagnostic(&self.link, operation, command, stdout_limit, stdout, stop),
        );
        tokio::select! {
            biased;
            _ = self.lease.changed() => bail!("The selected SSH connection closed or changed during {operation}"),
            result = request => result.with_context(|| {
                format!("{operation} exceeded its {}-second deadline", deadline.as_secs())
            })?,
        }
    }
}

trait DiagnosticStdout: Send {
    fn write(&mut self, bytes: &[u8]) -> impl std::future::Future<Output = Result<()>> + Send;
}

impl DiagnosticStdout for Vec<u8> {
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

#[derive(Default)]
struct ReportStream {
    report: Vec<u8>,
    receiving_report: bool,
    finished: [bool; PROBES.len()],
    progress: Option<watch::Sender<usize>>,
}

impl DiagnosticStdout for ReportStream {
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.report.extend_from_slice(bytes);
        while !self.receiving_report {
            let Some(end) = self.report.iter().position(|&byte| byte == b'\n') else {
                ensure!(
                    self.report.len() <= 128,
                    "Invalid Linux triage progress or header"
                );
                break;
            };
            let Some(id) = self.report[..end].strip_prefix(PROGRESS_PREFIX) else {
                self.receiving_report = true;
                break;
            };
            let index = PROBES
                .iter()
                .position(|(name, _, _)| name.as_bytes() == id)
                .context("Unknown Linux triage progress probe")?;
            ensure!(
                !self.finished[index],
                "Duplicate Linux triage progress probe"
            );
            self.finished[index] = true;
            self.report.drain(..=end);
            if let Some(progress) = &self.progress {
                let count = self.finished.iter().filter(|&&done| done).count();
                progress.send_replace(count);
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
pub struct Plan {
    pub connection_generation: u64,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub host_key_fingerprint: String,
    pub deadline_seconds: u64,
    pub max_bytes_per_stream: usize,
    pub probes: [Probe; PROBES.len()],
}

#[derive(Serialize, Deserialize)]
pub struct Probe {
    pub id: String,
    pub command: String,
    pub sampling: String,
}

#[derive(Serialize, Deserialize)]
pub struct Report {
    version: u8,
    pub target: Plan,
    pub collection: Collection,
    #[serde(skip_deserializing)]
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn load(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        ensure!(
            file.metadata()?.is_file(),
            "Choose a regular triage report file"
        );
        let mut bytes = Vec::new();
        file.take(REPORT_BYTES + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= REPORT_BYTES,
            "The triage report exceeds 3 MiB"
        );
        let mut report: Self =
            serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
                .context("Reading the Linux triage report")?;
        ensure!(
            report.version == 1,
            "Unsupported Linux triage report version"
        );
        ensure!(
            report.collection.samples.len() == PROBES.len()
                && report
                    .collection
                    .samples
                    .iter()
                    .zip(&report.target.probes)
                    .zip(PROBES)
                    .all(|((sample, probe), (id, _, _))| sample.id == id
                        && probe.id == id
                        && probe.command.len() <= 256
                        && probe.sampling.len() <= 512
                        && sample.stdout.text.len() <= STREAM_BYTES * 3
                        && sample.stderr.text.len() <= STREAM_BYTES * 3)
                && [
                    &report.target.host,
                    &report.target.user,
                    &report.target.host_key_fingerprint
                ]
                .iter()
                .all(|text| text.len() <= 4096),
            "The Linux triage report has invalid sample metadata or output"
        );
        report.findings = findings::summarize(&report.collection);
        Ok(report)
    }

    pub(crate) fn check_destination(path: &Path) -> Result<&Path> {
        diagnostic_destination(path, "triage report")
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let directory = Self::check_destination(path)?;
        let mut staged = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer_pretty(staged.as_file_mut(), self)?;
        staged.as_file().sync_all()?;
        staged
            .persist_noclobber(path)
            .map_err(|error| error.error)
            .context("Saving the Linux triage report")?;
        Ok(())
    }
}

pub(crate) fn diagnostic_destination<'a>(path: &'a Path, artifact: &str) -> Result<&'a Path> {
    ensure!(
        path.is_absolute() && path.file_name().is_some(),
        "Choose an absolute path for a new {artifact}"
    );
    let parent = path
        .parent()
        .with_context(|| format!("The {artifact} has no parent directory"))?;
    ensure!(parent.is_dir(), "The {artifact} directory does not exist");
    ensure!(
        !path.try_exists()?,
        "The {artifact} destination already exists"
    );
    Ok(parent)
}

fn command() -> String {
    format!(
        "/bin/sh -c {} w11term-triage {}",
        quote(SUPERVISOR),
        quote(WORKER)
    )
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

async fn read_diagnostic(
    link: &ConnectionShare,
    operation: &str,
    command: String,
    stdout_limit: usize,
    stdout: &mut impl DiagnosticStdout,
    stop: Option<&CaptureStop>,
) -> Result<Vec<u8>> {
    let mut channel = PendingChannel(Some(
        link.handle()
            .channel_open_session()
            .await
            .with_context(|| format!("Opening SSH channel for {operation}"))?,
    ));
    let channel = channel.0.as_mut().expect("opened channel");
    channel.exec(true, command).await?;
    let mut received = 0;
    let mut stderr = Vec::new();
    let mut result = None;
    let mut eof = false;
    let mut stop = stop.map(CaptureStop::subscribe);
    loop {
        let message = tokio::select! {
            biased;
            message = channel.wait() => message,
            _ = async {
                if let Some(stop) = &mut stop {
                    let _ = stop.wait_for(|requested| *requested).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                stop = None;
                // A completed report can still arrive if the request races channel closure.
                let _ = channel.data(b"stop\n".as_slice()).await;
                continue;
            }
        };
        let Some(message) = message else { break };
        match message {
            ChannelMsg::Data { data } => {
                received += data.len();
                ensure!(
                    received <= stdout_limit,
                    "Remote {operation} output exceeds its byte limit"
                );
                stdout.write(&data).await?;
            }
            ChannelMsg::ExtendedData { data, .. } => {
                ensure!(
                    stderr.len() + data.len() <= STREAM_BYTES,
                    "Remote {operation} diagnostic messages exceed their byte limit"
                );
                stderr.extend_from_slice(&data);
            }
            ChannelMsg::Failure => {
                bail!("The server rejected the SSH exec request for {operation}")
            }
            ChannelMsg::Eof => eof = true,
            ChannelMsg::ExitStatus { exit_status } => result = Some(exit_status),
            ChannelMsg::Close => break,
            _ => (),
        }
        if eof && result.is_some() {
            break;
        }
    }
    ensure!(
        result == Some(0),
        "Remote {operation} failed ({result:?}): {}",
        crate::events::display_field(&String::from_utf8_lossy(&stderr))
    );
    Ok(stderr)
}

struct ArtifactMessages {
    operation: &'static str,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: Option<u32>,
}

impl ArtifactMessages {
    fn new(operation: &'static str) -> Self {
        Self {
            operation,
            stdout: Vec::new(),
            stderr: Vec::new(),
            status: None,
        }
    }

    async fn receive(&mut self, channel: &mut Channel<client::Msg>) -> Result<bool> {
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => {
                ensure!(
                    self.stdout.len() + data.len() <= 4096,
                    "{} metadata exceeds its limit",
                    self.operation
                );
                self.stdout.extend_from_slice(&data);
            }
            Some(ChannelMsg::ExtendedData { data, .. }) => {
                ensure!(
                    self.stderr.len() + data.len() <= 12 * 1024,
                    "{} diagnostics exceed their limit",
                    self.operation
                );
                self.stderr.extend_from_slice(&data);
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => self.status = Some(exit_status),
            Some(ChannelMsg::Failure) => {
                bail!("The server rejected the {} command", self.operation)
            }
            Some(ChannelMsg::Close) | None => return Ok(false),
            _ => (),
        }
        Ok(true)
    }

    async fn line(&mut self, channel: &mut Channel<client::Msg>) -> Result<String> {
        loop {
            if let Some(i) = self.stdout.iter().position(|&byte| byte == b'\n') {
                let line = self.stdout.drain(..=i).collect::<Vec<_>>();
                return Ok(std::str::from_utf8(&line[..i])?.to_owned());
            }
            if !self.receive(channel).await? {
                bail!(
                    "{} ended early ({:?}). {}",
                    self.operation,
                    self.status,
                    crate::events::display_field(&String::from_utf8_lossy(&self.stderr))
                );
            }
        }
    }

    async fn finish(&mut self, channel: &mut Channel<client::Msg>) -> Result<()> {
        while self.status.is_none() && self.receive(channel).await? {}
        ensure!(
            self.status == Some(0) && self.stdout.is_empty(),
            "{} cleanup failed",
            self.operation
        );
        Ok(())
    }
}

struct PendingChannel(Option<Channel<client::Msg>>);

impl Drop for PendingChannel {
    fn drop(&mut self) {
        if let Some(channel) = self.0.take() {
            drop(channel.into_stream());
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Collection {
    pub started_unix_ms: u64,
    pub finished_unix_ms: u64,
    pub supervisor_status: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopped_by_user: bool,
    pub samples: Vec<Sample>,
}

#[derive(Serialize, Deserialize)]
pub struct Sample {
    pub id: String,
    pub started_unix_ms: Option<u64>,
    pub finished_unix_ms: Option<u64>,
    pub exit_status: Option<u32>,
    pub status: SampleStatus,
    pub stdout: Output,
    pub stderr: Output,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SampleStatus {
    Ok,
    Missing,
    TimedOut,
    Incomplete,
    OutputLimit,
    PermissionDenied,
    Failed,
}

#[derive(Serialize, Deserialize)]
pub struct Output {
    pub text: String,
    pub at_byte_limit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    base64: Option<String>,
}

impl Output {
    fn decode(encoded: &str) -> Result<Self> {
        ensure!(
            encoded.len() <= STREAM_BYTES.div_ceil(3) * 4,
            "Triage sample exceeds its byte limit"
        );
        let bytes = STANDARD
            .decode(encoded)
            .context("Invalid base64 in remote triage sample")?;
        ensure!(
            bytes.len() <= STREAM_BYTES,
            "Triage sample exceeds its byte limit"
        );
        let at_byte_limit = bytes.len() == STREAM_BYTES;
        let (text, base64) = match String::from_utf8(bytes) {
            Ok(text) => (text, None),
            Err(error) => (
                String::from_utf8_lossy(error.as_bytes()).into_owned(),
                Some(encoded.into()),
            ),
        };
        Ok(Self {
            text,
            at_byte_limit,
            base64,
        })
    }
}

fn number<T: std::str::FromStr>(field: &str) -> Result<Option<T>> {
    if field == "-" {
        return Ok(None);
    }
    field
        .parse()
        .map(Some)
        .map_err(|_| anyhow::anyhow!("Invalid timestamp or status in remote triage sample"))
}

fn parse(bytes: &[u8]) -> Result<Collection> {
    let mut lines = std::str::from_utf8(bytes)
        .context("Invalid remote triage framing")?
        .lines();
    let header = lines
        .next()
        .context("Remote triage did not return a report")?;
    let (version, start, end, status, stopped_by_user) =
        match header.split('\t').collect::<Vec<_>>()[..] {
            [version, start, end, status] => (version, start, end, status, false),
            [version, start, end, status, "stopped"] => (version, start, end, status, true),
            _ => bail!("Invalid remote triage report header"),
        };
    ensure!(
        version == "w11term-linux-triage-v1",
        "Unrecognized remote triage protocol"
    );
    let supervisor_status = status.parse()?;
    ensure!(
        !stopped_by_user || supervisor_status != 0,
        "Invalid stopped triage status"
    );
    let mut samples = Vec::with_capacity(PROBES.len());
    for (id, _, _) in PROBES {
        let line = lines.next().context("Remote triage report is incomplete")?;
        let [seen, start, end, code, out, err] = line.split('\t').collect::<Vec<_>>()[..] else {
            bail!("Invalid remote triage sample");
        };
        ensure!(seen == id, "Unexpected remote triage sample {seen}");
        let stdout = Output::decode(out)?;
        let stderr = Output::decode(err)?;
        let exit_status = number(code)?;
        let status = match exit_status {
            Some(0) => SampleStatus::Ok,
            Some(127) => SampleStatus::Missing,
            None if !stopped_by_user && matches!(supervisor_status, 124 | 137) => {
                SampleStatus::TimedOut
            }
            None => SampleStatus::Incomplete,
            Some(_) if stdout.at_byte_limit || stderr.at_byte_limit => SampleStatus::OutputLimit,
            Some(_) => {
                let message = stderr.text.to_ascii_lowercase();
                if message.contains("operation not permitted")
                    || message.contains("permission denied")
                {
                    SampleStatus::PermissionDenied
                } else {
                    SampleStatus::Failed
                }
            }
        };
        samples.push(Sample {
            id: id.into(),
            started_unix_ms: number(start)?,
            finished_unix_ms: number(end)?,
            exit_status,
            status,
            stdout,
            stderr,
        });
    }
    ensure!(
        lines.next() == Some("end") && lines.next().is_none(),
        "Remote triage report is incomplete or has trailing data"
    );
    Ok(Collection {
        started_unix_ms: start.parse()?,
        finished_unix_ms: end.parse()?,
        supervisor_status,
        stopped_by_user,
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn triage_progress_frames_are_bounded_and_separate_from_the_report() {
        let (progress, observed) = watch::channel(0);
        let mut stream = ReportStream {
            progress: Some(progress),
            ..ReportStream::default()
        };
        stream.write(b"w11term-triage-progress\tup").await.unwrap();
        assert_eq!(*observed.borrow(), 0);
        stream
            .write(b"time\nw11term-triage-progress\tdmesg\nw11term-linux-triage-v1\t1000")
            .await
            .unwrap();
        stream.write(b"\t2000\t0\n").await.unwrap();
        assert_eq!(*observed.borrow(), 2);
        assert_eq!(stream.report, b"w11term-linux-triage-v1\t1000\t2000\t0\n");

        let mut invalid = ReportStream::default();
        assert!(
            invalid
                .write(b"w11term-triage-progress\tunknown\n")
                .await
                .is_err()
        );
        let mut repeated = ReportStream::default();
        let complete = b"w11term-triage-progress\tuptime\n";
        repeated.write(complete).await.unwrap();
        assert!(repeated.write(complete).await.is_err());
        assert!(ReportStream::default().write(&[b'x'; 129]).await.is_err());
    }

    #[test]
    fn triage_preserves_binary_evidence_and_rejects_oversized_streams() {
        let encoded = STANDARD.encode([0xff, b'\n']);
        let output = Output::decode(&encoded).unwrap();
        assert_eq!(output.base64.as_deref(), Some(encoded.as_str()));
        assert_eq!(output.text, "\u{fffd}\n");
        assert!(
            Output::decode(&STANDARD.encode(vec![0; STREAM_BYTES]))
                .unwrap()
                .at_byte_limit
        );
        assert!(Output::decode(&STANDARD.encode(vec![0; STREAM_BYTES + 1])).is_err());
        assert!(parse(b"w11term-linux-triage-v1\t1000\t2000\t0\nend\n").is_err());
    }
}
