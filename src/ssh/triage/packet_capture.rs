use super::{ConnectionShare, PendingChannel, Target, diagnostic_destination, quote};
use anyhow::{Context, Result, bail, ensure};
use russh::{Channel, ChannelMsg, client};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::Duration,
};

const SCRIPT: &str = include_str!("packet-capture.sh");
const CAPTURE_FILE: &str = "capture.pcap";
const METADATA_BYTES: usize = 4096;
const DIAGNOSTIC_BYTES: usize = 12 * 1024;
const CAPTURE_OVERHEAD_SECONDS: u64 = 72;
pub(crate) const CONTROL_SECONDS: u64 = 155;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PacketCaptureOptions {
    pub interface: String,
    pub filter: String,
    pub snap_length: u32,
    pub seconds: u8,
    pub max_mib: u8,
    pub packet_limit: Option<u32>,
    pub scratch_directory: String,
    pub require_tmpfs: bool,
}

impl PacketCaptureOptions {
    pub fn validate(self) -> Result<Self> {
        ensure!(
            self.interface.len() <= 63
                && self
                    .interface
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && self
                    .interface
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-:@".contains(&b)),
            "Select a Linux interface such as eth0, lo, or any"
        );
        ensure!(
            self.filter.len() <= 1024 && !self.filter.chars().any(char::is_control),
            "Provide a tcpdump filter of up to 1024 bytes without control characters"
        );
        ensure!(
            (64..=262_144).contains(&self.snap_length),
            "Capture snap length must be 64..=262144 bytes"
        );
        ensure!(
            (1..=60).contains(&self.seconds),
            "Packet capture duration must be 1..=60 seconds"
        );
        ensure!(
            (1..=64).contains(&self.max_mib),
            "Packet capture budget must be 1..=64 MiB"
        );
        ensure!(
            self.packet_limit != Some(0),
            "The packet limit must be positive"
        );
        ensure!(
            self.scratch_directory.starts_with('/')
                && self.scratch_directory.len() <= 2048
                && !self.scratch_directory.chars().any(char::is_control),
            "Choose an absolute Linux scratch directory"
        );
        Ok(self)
    }

    fn budget_bytes(&self) -> u64 {
        u64::from(self.max_mib) * 1024 * 1024
    }

    fn budget_packets(&self) -> u32 {
        // libpcap's classic file header is 24 bytes; each packet uses 16
        // more bytes than its captured payload.
        ((self.budget_bytes() - 24) / (16 + u64::from(self.snap_length))) as u32
    }

    fn effective_packets(&self) -> u32 {
        let ceiling = self.budget_packets();
        self.packet_limit
            .map_or(ceiling, |requested| requested.min(ceiling))
    }

    fn tcpdump_arguments(&self) -> Vec<String> {
        let mut args = vec![
            "-i".into(),
            self.interface.clone(),
            "-nn".into(),
            "-p".into(),
            "-s".into(),
            self.snap_length.to_string(),
            "-c".into(),
            self.effective_packets().to_string(),
            "-w".into(),
            "-".into(),
            "--".into(),
        ];
        if !self.filter.is_empty() {
            args.push(self.filter.clone());
        }
        args
    }
}

#[derive(Serialize)]
pub(crate) struct Plan {
    pub connection_generation: u64,
    host: String,
    user: String,
    port: u16,
    host_key_fingerprint: String,
    options: PacketCaptureOptions,
    scratch_directory: String,
    scratch_filesystem: String,
    #[serde(skip)]
    scratch_id: String,
    effective_packet_limit: u32,
    tcpdump_arguments: Vec<String>,
    review_id: String,
}

#[derive(Serialize)]
pub(crate) struct Capture {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub packets: u32,
    pub received_by_filter: Option<u64>,
    pub dropped_by_kernel: Option<u64>,
    pub stop_reason: &'static str,
    pub diagnostics: Vec<String>,
}

impl Target {
    pub(crate) async fn preview_packet_capture(
        &mut self,
        options: PacketCaptureOptions,
    ) -> Result<Plan> {
        let options = options.validate()?;
        let (wire, _) = self
            .run_diagnostic(
                "packet capture review",
                command(&options, "inspect", &options.scratch_directory, "", ""),
                Duration::from_secs(8),
                METADATA_BYTES,
            )
            .await?;
        let line = std::str::from_utf8(&wire)?
            .strip_suffix('\n')
            .context("Incomplete packet capture review")?;
        let fields: Vec<_> = line.split('\t').collect();
        let ["w11term-packet-plan-v1", filesystem, id, directory] = fields.as_slice() else {
            bail!("Invalid packet capture review");
        };
        ensure!(
            directory.starts_with('/')
                && directory.len() <= 2048
                && !directory.chars().any(char::is_control)
                && !filesystem.is_empty()
                && filesystem
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_/.".contains(&b))
                && id
                    .split_once(':')
                    .is_some_and(|(a, b)| a.parse::<u64>().is_ok() && b.parse::<u64>().is_ok()),
            "Invalid scratch directory metadata"
        );
        ensure!(
            !options.require_tmpfs || *filesystem == "tmpfs",
            "The reviewed scratch directory is not tmpfs"
        );
        let (host, user, port, fingerprint) = self.link.verified_endpoint();
        let mut plan = Plan {
            connection_generation: self.generation,
            host: host.into(),
            user: user.into(),
            port,
            host_key_fingerprint: fingerprint.into(),
            scratch_directory: (*directory).into(),
            scratch_filesystem: (*filesystem).into(),
            scratch_id: (*id).into(),
            effective_packet_limit: options.effective_packets(),
            tcpdump_arguments: options.tcpdump_arguments(),
            options,
            review_id: String::new(),
        };
        let mut hash = Sha256::new();
        hash.update(serde_json::to_vec(&plan)?);
        hash.update(plan.scratch_id.as_bytes());
        plan.review_id = crate::sftp::digest_hex(hash);
        Ok(plan)
    }

    pub(crate) async fn capture_packets(
        mut self,
        options: PacketCaptureOptions,
        review_id: &str,
        path: PathBuf,
    ) -> Result<Capture> {
        let parent = diagnostic_destination(&path, "packet capture")?.to_owned();
        let review_id = crate::sftp::normalize_sha256(review_id)?;
        let plan = self.preview_packet_capture(options).await?;
        ensure!(
            plan.review_id == review_id,
            "The packet capture settings or scratch directory changed. Review again."
        );
        let _busy = self.link.try_lock_diagnostic()?;
        let timeout =
            Duration::from_secs(u64::from(plan.options.seconds) + CAPTURE_OVERHEAD_SECONDS);
        let capture =
            tokio::time::timeout(timeout, capture_and_download(&self.link, &plan, &parent));
        let (file, mut capture) = tokio::select! {
            biased;
            _ = self.lease.changed() => bail!("The reviewed SSH connection closed during packet capture"),
            result = capture => result.context("Packet capture or SFTP transfer exceeded its deadline")??,
        };
        capture.path = path.clone();
        tokio::task::spawn_blocking(move || -> Result<_> {
            file.persist_noclobber(path)
                .map_err(|error| error.error)
                .context("Saving the packet capture")?;
            Ok(capture)
        })
        .await?
    }
}

fn command(
    options: &PacketCaptureOptions,
    mode: &str,
    scratch: &str,
    id: &str,
    job: &str,
) -> String {
    format!(
        "/bin/bash -c {} w11term-packet-capture {mode} {} {} {} {} {} {} {} {} {} {}",
        quote(SCRIPT),
        quote(&options.interface),
        options.snap_length,
        options.effective_packets(),
        options.seconds,
        options.budget_bytes() / 1024,
        if options.require_tmpfs { "yes" } else { "no" },
        quote(scratch),
        quote(&options.filter),
        quote(id),
        quote(job),
    )
}

async fn capture_and_download(
    link: &ConnectionShare,
    plan: &Plan,
    parent: &Path,
) -> Result<(tempfile::NamedTempFile, Capture)> {
    let job = format!("{:032x}", rand::random::<u128>());
    let handle = link.handle();
    let mut guard = PendingChannel(Some(
        handle
            .channel_open_session()
            .await
            .context("Opening packet capture channel")?,
    ));
    let channel = guard.0.as_mut().unwrap();
    channel
        .exec(
            true,
            command(
                &plan.options,
                "capture",
                &plan.scratch_directory,
                &plan.scratch_id,
                &job,
            ),
        )
        .await?;
    let mut messages = Messages::default();
    let line = messages.line(channel).await?;
    let parts: Vec<_> = line.split('\t').collect();
    let ["w11term-packet-ready-v1", reason, size, hash] = parts.as_slice() else {
        bail!("Invalid packet capture completion record");
    };
    let size: u64 = size.trim().parse().context("Reading packet capture size")?;
    ensure!(
        (24..=plan.options.budget_bytes()).contains(&size),
        "Remote packet capture exceeded its reviewed byte budget"
    );
    ensure!(
        matches!(*reason, "time" | "packets"),
        "Unknown packet capture stop reason"
    );
    let sha256 = crate::sftp::normalize_sha256(hash)?;
    let remote = format!(
        "{}/w11term-pcap-{job}/{CAPTURE_FILE}",
        plan.scratch_directory.trim_end_matches('/')
    );
    let file =
        crate::sftp::stage_diagnostic_download(&handle, &remote, parent, size, &sha256).await?;
    let (file, packets) = tokio::task::spawn_blocking(move || -> Result<_> {
        let packets = count_packets(file.reopen()?, size)?;
        Ok((file, packets))
    })
    .await??;
    ensure!(
        packets <= plan.effective_packet_limit,
        "Capture exceeded the reviewed packet limit"
    );
    channel.data(b"verified\n".as_slice()).await?;
    channel.eof().await?;
    ensure!(
        messages.line(channel).await? == "w11term-packet-complete-v1",
        "The packet capture cleanup was not acknowledged"
    );
    messages.finish(channel).await?;
    let stop_reason = if *reason == "time" {
        "time-limit"
    } else if packets != plan.effective_packet_limit {
        "capture-ended"
    } else if plan.options.packet_limit == Some(plan.effective_packet_limit) {
        "packet-limit"
    } else {
        "byte-budget"
    };
    let stderr = String::from_utf8_lossy(&messages.stderr);
    Ok((
        file,
        Capture {
            path: PathBuf::new(),
            bytes: size,
            sha256,
            packets,
            stop_reason,
            received_by_filter: count_stat(&stderr, "received by filter"),
            dropped_by_kernel: count_stat(&stderr, "dropped by kernel"),
            diagnostics: stderr
                .lines()
                .map(crate::events::display_field)
                .filter(|line| !line.is_empty())
                .collect(),
        },
    ))
}

fn count_stat(stderr: &str, label: &str) -> Option<u64> {
    stderr.lines().find_map(|line| {
        let (count, text) = line.trim().split_once(' ')?;
        let suffix = text
            .strip_prefix("packets ")
            .or_else(|| text.strip_prefix("packet "))?;
        (suffix == label).then(|| count.parse().ok()).flatten()
    })
}

#[derive(Default)]
struct Messages {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: Option<u32>,
}

impl Messages {
    async fn receive(&mut self, channel: &mut Channel<client::Msg>) -> Result<bool> {
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => {
                ensure!(
                    self.stdout.len() + data.len() <= METADATA_BYTES,
                    "Packet capture metadata exceeds its limit"
                );
                self.stdout.extend_from_slice(&data);
            }
            Some(ChannelMsg::ExtendedData { data, .. }) => {
                ensure!(
                    self.stderr.len() + data.len() <= DIAGNOSTIC_BYTES,
                    "Packet capture diagnostics exceed their limit"
                );
                self.stderr.extend_from_slice(&data);
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => self.status = Some(exit_status),
            Some(ChannelMsg::Failure) => bail!("The server rejected the packet capture command"),
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
                    "Remote packet capture ended early ({:?}). {}",
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
            "Remote packet capture cleanup failed"
        );
        Ok(())
    }
}

fn count_packets(file: std::fs::File, size: u64) -> Result<u32> {
    let mut file = BufReader::new(file);
    let mut header = [0; 24];
    file.read_exact(&mut header)?;
    let little = match &header[..4] {
        [0xd4, 0xc3, 0xb2, 0xa1] | [0x4d, 0x3c, 0xb2, 0xa1] => true,
        [0xa1, 0xb2, 0xc3, 0xd4] | [0xa1, 0xb2, 0x3c, 0x4d] => false,
        _ => bail!("The transferred file is not a classic pcap capture"),
    };
    let number = if little {
        u32::from_le_bytes
    } else {
        u32::from_be_bytes
    };
    let version = if little { [2, 0, 4, 0] } else { [0, 2, 0, 4] };
    ensure!(header[4..8] == version, "Unsupported pcap file version");
    let snap = number(header[16..20].try_into().unwrap());
    ensure!(snap > 0, "Invalid pcap snapshot length");
    let mut remaining = size - 24;
    let mut packets = 0;
    while remaining > 0 {
        ensure!(remaining >= 16, "The pcap packet header is truncated");
        let mut header = [0; 16];
        file.read_exact(&mut header)?;
        let caplen = number(header[8..12].try_into().unwrap());
        let wirelen = number(header[12..16].try_into().unwrap());
        ensure!(
            caplen <= snap && caplen <= wirelen && u64::from(caplen) <= remaining - 16,
            "The pcap packet is incomplete or invalid"
        );
        file.seek_relative(i64::from(caplen))?;
        remaining -= 16 + u64::from(caplen);
        packets += 1;
    }
    Ok(packets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_reserves_room_for_each_complete_packet_record() {
        let mut options = PacketCaptureOptions {
            interface: "lo".into(),
            filter: String::new(),
            snap_length: 262_144,
            seconds: 5,
            max_mib: 1,
            packet_limit: None,
            scratch_directory: "/tmp".into(),
            require_tmpfs: false,
        };
        let packet_bytes = u64::from(options.snap_length) + 16;
        let packet_limit = u64::from(options.effective_packets());
        assert!(24 + packet_bytes * packet_limit <= options.budget_bytes());
        assert!(24 + packet_bytes * (packet_limit + 1) > options.budget_bytes());
        options.packet_limit = Some(2);
        assert_eq!(options.effective_packets(), 2);
    }
}
