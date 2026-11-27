use super::{ConnectionShare, ShareSlot};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use russh::{Channel, ChannelMsg, client};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::watch;

pub(crate) const DEADLINE: Duration = Duration::from_secs(60);
const STREAM_BYTES: usize = 16 * 1024;
const WIRE_BYTES: usize = PROBES.len() * 2 * (STREAM_BYTES.div_ceil(3) * 4) + 4096;
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

    pub(crate) fn preview(&self) -> Value {
        let (host, user, port, fingerprint) = self.link.verified_endpoint();
        json!({
            "connection_generation": self.generation,
            "host": host, "user": user, "port": port, "host_key_fingerprint": fingerprint,
            "deadline_seconds": DEADLINE.as_secs(),
            "max_bytes_per_stream": STREAM_BYTES,
            "probes": PROBES.map(|(id, command, sampling)| json!({
                "id": id, "command": command, "sampling": sampling,
            })),
        })
    }

    pub(crate) async fn collect(mut self) -> Result<Value> {
        let plan = self.preview();
        let _lease = self.link.try_lock_triage()?;
        let capture = tokio::time::timeout(DEADLINE, collect(&self.link));
        let bytes = tokio::select! {
            biased;
            _ = self.lease.changed() => bail!("The selected SSH connection closed or changed during triage"),
            result = capture => result.context("Linux triage exceeded its 60-second deadline")??,
        };
        Ok(json!({ "version": 1, "target": plan, "collection": parse(&bytes)? }))
    }
}

fn command() -> String {
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    format!(
        "/bin/sh -c {} w11term-triage {}",
        quote(SUPERVISOR),
        quote(WORKER)
    )
}

async fn collect(link: &ConnectionShare) -> Result<Vec<u8>> {
    let mut channel = PendingChannel(Some(
        link.handle()
            .channel_open_session()
            .await
            .context("Opening SSH triage channel")?,
    ));
    let channel = channel.0.as_mut().expect("opened channel");
    channel.exec(true, command()).await?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut result = None;
    let mut eof = false;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => {
                ensure!(
                    stdout.len() + data.len() <= WIRE_BYTES,
                    "Remote triage report exceeds its byte limit"
                );
                stdout.extend_from_slice(&data);
            }
            ChannelMsg::ExtendedData { data, .. } => {
                ensure!(
                    stderr.len() + data.len() <= STREAM_BYTES,
                    "Remote triage error output exceeds its byte limit"
                );
                stderr.extend_from_slice(&data);
            }
            ChannelMsg::Failure => bail!("The server rejected the SSH triage command"),
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
        "Remote Linux triage failed ({result:?}): {}",
        crate::events::display_field(&String::from_utf8_lossy(&stderr))
    );
    Ok(stdout)
}

struct PendingChannel(Option<Channel<client::Msg>>);

impl Drop for PendingChannel {
    fn drop(&mut self) {
        if let Some(channel) = self.0.take() {
            drop(channel.into_stream());
        }
    }
}

#[derive(Serialize)]
struct Collection {
    started_unix_ms: u64,
    finished_unix_ms: u64,
    supervisor_status: u32,
    samples: Vec<Sample>,
}

#[derive(Serialize)]
struct Sample {
    id: &'static str,
    started_unix_ms: Option<u64>,
    finished_unix_ms: Option<u64>,
    exit_status: Option<u32>,
    status: &'static str,
    stdout: Output,
    stderr: Output,
}

#[derive(Serialize)]
struct Output {
    text: String,
    at_byte_limit: bool,
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
    let [version, start, end, status] = header.split('\t').collect::<Vec<_>>()[..] else {
        bail!("Invalid remote triage report header");
    };
    ensure!(
        version == "w11term-linux-triage-v1",
        "Unrecognized remote triage protocol"
    );
    let supervisor_status = status.parse()?;
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
            Some(0) => "ok",
            Some(127) => "missing",
            None if matches!(supervisor_status, 124 | 137) => "timed-out",
            None => "incomplete",
            Some(_) if stdout.at_byte_limit || stderr.at_byte_limit => "output-limit",
            Some(_) => {
                let message = stderr.text.to_ascii_lowercase();
                if message.contains("operation not permitted")
                    || message.contains("permission denied")
                {
                    "permission-denied"
                } else {
                    "failed"
                }
            }
        };
        samples.push(Sample {
            id,
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
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
