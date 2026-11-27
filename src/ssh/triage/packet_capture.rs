use super::{
    ArtifactMessages, ConnectionShare, PendingChannel, Target,
    capture_job::{CaptureStop, Kind, validate_job},
    diagnostic_destination, quote,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{BufReader, Read},
    path::{Path, PathBuf},
    time::Duration,
};

const SCRIPT: &str = include_str!("packet-capture.sh");
const RING_WORKER: &str = include_str!("packet-ring.py");
const CAPTURE_FILE: &str = "capture.pcap";
const METADATA_BYTES: usize = 4096;
const CAPTURE_OVERHEAD_SECONDS: u64 = 72;
pub(crate) const CONTROL_SECONDS: u64 = 155;
mod ring;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PacketCaptureOptions {
    pub interface: String,
    pub filter: String,
    pub snap_length: u32,
    pub seconds: u8,
    pub max_mib: u8,
    pub packet_limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate_files: Option<u8>,
    pub scratch_directory: String,
    pub require_tmpfs: bool,
}

impl PacketCaptureOptions {
    pub const DEFAULT_SECONDS: u8 = 15;
    pub const DEFAULT_SNAP_LENGTH: u32 = 192;
    pub const DEFAULT_MAX_MIB: u8 = 16;

    pub fn default_scratch_directory(tmpfs: bool) -> &'static str {
        if tmpfs { "/dev/shm" } else { "/tmp" }
    }

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
        if let Some(files) = self.rotate_files {
            ensure!(
                (2..=16).contains(&files),
                "Rolling packet capture requires 2 to 16 files"
            );
            ensure!(
                self.budget_bytes() / u64::from(files) >= 24 + 16 + u64::from(self.snap_length),
                "The byte budget and file count leave no room for one maximum-size packet per file"
            );
        }
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

    fn effective_packets(&self) -> Option<u32> {
        if self.rotate_files.is_some() {
            return self.packet_limit;
        }
        let ceiling = self.budget_packets();
        Some(
            self.packet_limit
                .map_or(ceiling, |requested| requested.min(ceiling)),
        )
    }

    fn rotation(&self) -> Option<Rotation> {
        self.rotate_files.map(|files| Rotation {
            files,
            bytes_per_file: self.budget_bytes() / u64::from(files),
        })
    }

    fn tcpdump_arguments(&self) -> Vec<String> {
        let mut args = vec![
            "-i".into(),
            self.interface.clone(),
            "-nn".into(),
            "-p".into(),
            "-s".into(),
            self.snap_length.to_string(),
        ];
        if let Some(count) = self.effective_packets() {
            args.extend(["-c".into(), count.to_string()]);
        }
        args.extend(["-w".into(), "-".into(), "--".into()]);
        if !self.filter.is_empty() {
            args.push(self.filter.clone());
        }
        args
    }
}

#[derive(Clone, Copy, Serialize)]
pub struct Rotation {
    pub files: u8,
    pub bytes_per_file: u64,
}

#[derive(Clone, Serialize)]
pub struct Plan {
    pub connection_generation: u64,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub host_key_fingerprint: String,
    pub options: PacketCaptureOptions,
    pub scratch_directory: String,
    pub scratch_filesystem: String,
    #[serde(skip)]
    scratch_id: String,
    pub effective_packet_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<Rotation>,
    pub tcpdump_arguments: Vec<String>,
    pub review_id: String,
}

#[derive(Serialize)]
pub struct Capture {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub packets: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<RotationResult>,
    pub received_by_filter: Option<u64>,
    pub dropped_by_kernel: Option<u64>,
    pub stop_reason: &'static str,
    pub diagnostics: Vec<String>,
}

#[derive(Serialize)]
pub struct RotationResult {
    pub retained_files: usize,
    pub overwritten_files: u64,
    pub captured_packets: u64,
}

struct CaptureFile {
    file: tempfile::NamedTempFile,
    bytes: u64,
    sha256: String,
    packets: u32,
}
impl Target {
    pub fn preview_packet_capture_blocking(
        mut self,
        options: PacketCaptureOptions,
    ) -> Result<Plan> {
        crate::ssh::NETWORK.block_on(self.preview_packet_capture(options))
    }

    pub fn save_packet_capture_blocking(self, plan: Plan, path: PathBuf) -> Result<Capture> {
        self.save_packet_capture_with_stop_blocking(0, plan, path, CaptureStop::default())
    }

    pub fn save_packet_capture_with_stop_blocking(
        self,
        pane: u64,
        plan: Plan,
        path: PathBuf,
        stop: CaptureStop,
    ) -> Result<Capture> {
        ensure!(
            self.generation == plan.connection_generation,
            "The reviewed SSH connection changed. Review again."
        );
        let job = format!("{:032x}", rand::random::<u128>());
        crate::ssh::NETWORK.block_on(self.capture_packets(
            pane,
            plan.options,
            &plan.review_id,
            &job,
            path,
            stop,
        ))
    }

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
            rotation: options.rotation(),
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
        pane: u64,
        options: PacketCaptureOptions,
        review_id: &str,
        job: &str,
        path: PathBuf,
        stop: CaptureStop,
    ) -> Result<Capture> {
        check_before_launch(&stop)?;
        let job = validate_job(job)?;
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
        let capture = tokio::time::timeout(
            timeout,
            capture_and_download(&self.link, &plan, pane, job, &parent, stop),
        );
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

    pub(crate) fn stop_packet_capture(&self, pane: u64, job: &str) -> Result<()> {
        self.link
            .capture_jobs()
            .stop(Kind::Packets, self.generation, pane, validate_job(job)?)
    }
}

fn check_before_launch(stop: &CaptureStop) -> Result<()> {
    ensure!(
        !stop.is_requested(),
        "Packet capture stopped before tcpdump started"
    );
    Ok(())
}

pub fn open_capture_in_wireshark(path: &Path, executable: &Path) -> Result<u32> {
    ensure!(
        executable.is_absolute()
            && executable
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")),
        "Choose a Windows Wireshark executable"
    );
    ensure!(
        executable.is_file(),
        "Wireshark executable was not found at {}",
        executable.display()
    );
    ensure!(
        path.is_absolute() && path.is_file(),
        "The saved capture file is unavailable"
    );
    Ok(std::process::Command::new(executable)
        .arg("-r")
        .arg(path)
        .spawn()?
        .id())
}

fn command(
    options: &PacketCaptureOptions,
    mode: &str,
    scratch: &str,
    id: &str,
    job: &str,
) -> String {
    let mut command = format!(
        "/bin/bash -c {} w11term-packet-capture {mode} {} {} {} {} {} {} {} {} {} {}",
        quote(SCRIPT),
        quote(&options.interface),
        options.snap_length,
        options.effective_packets().unwrap_or(0),
        options.seconds,
        options.budget_bytes() / 1024,
        if options.require_tmpfs { "yes" } else { "no" },
        quote(scratch),
        quote(&options.filter),
        quote(id),
        quote(job),
    );
    if let Some(files) = options.rotate_files {
        use std::fmt::Write;
        write!(command, " w11term-ring-v1 {files} {}", quote(RING_WORKER)).unwrap();
    }
    command
}

async fn capture_and_download(
    link: &ConnectionShare,
    plan: &Plan,
    pane: u64,
    job: &str,
    parent: &Path,
    stop: CaptureStop,
) -> Result<(tempfile::NamedTempFile, Capture)> {
    check_before_launch(&stop)?;
    let registration = link.capture_jobs().register(
        Kind::Packets,
        plan.connection_generation,
        pane,
        job,
        &stop,
    )?;
    let handle = link.handle();
    let mut guard = PendingChannel(Some(
        handle
            .channel_open_session()
            .await
            .context("Opening packet capture channel")?,
    ));
    let channel = guard.0.as_mut().unwrap();
    check_before_launch(&stop)?;
    channel
        .exec(
            true,
            command(
                &plan.options,
                "capture",
                &plan.scratch_directory,
                &plan.scratch_id,
                job,
            ),
        )
        .await?;
    let mut messages = ArtifactMessages::new("Packet capture");
    let line = stop.ready(&mut messages, channel).await?;
    drop(registration);
    let (kind, reason, data) = parse_ready(&line)?;
    ensure!(
        matches!(reason, "time" | "packets" | "stopped"),
        "Unknown packet capture stop reason"
    );
    let remote = format!(
        "{}/w11term-pcap-{job}",
        plan.scratch_directory.trim_end_matches('/')
    );
    let (capture, rotation) = match (kind, plan.rotation) {
        ("w11term-packet-ready-v1", None) => {
            let file =
                download_capture(&handle, &remote, parent, plan.options.budget_bytes(), data)
                    .await?;
            (file, None)
        }
        ("w11term-packet-ring-v1", Some(limits)) => {
            let (file, summary) = ring::download(&handle, &remote, parent, limits, data).await?;
            (file, Some(summary))
        }
        _ => bail!("Packet capture format differs from the reviewed settings"),
    };
    let captured_packets = rotation
        .as_ref()
        .map_or(u64::from(capture.packets), |ring| ring.captured_packets);
    ensure!(
        plan.effective_packet_limit
            .is_none_or(|limit| captured_packets <= u64::from(limit)),
        "Capture exceeded the reviewed packet limit"
    );
    channel.data(b"verified\n".as_slice()).await?;
    channel.eof().await?;
    ensure!(
        messages.line(channel).await? == "w11term-packet-complete-v1",
        "The packet capture cleanup was not acknowledged"
    );
    messages.finish(channel).await?;
    let reached_ceiling = plan
        .effective_packet_limit
        .is_some_and(|limit| captured_packets == u64::from(limit));
    let stop_reason = match reason {
        "time" => "time-limit",
        "stopped" => "stopped",
        _ if !reached_ceiling => "capture-ended",
        _ if plan.options.packet_limit == plan.effective_packet_limit => "packet-limit",
        _ => "byte-budget",
    };
    let stderr = String::from_utf8_lossy(&messages.stderr);
    Ok((
        capture.file,
        Capture {
            path: PathBuf::new(),
            bytes: capture.bytes,
            sha256: capture.sha256,
            packets: capture.packets,
            rotation,
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

fn parse_ready(line: &str) -> Result<(&str, &str, &str)> {
    let mut fields = line.splitn(3, '\t');
    match (fields.next(), fields.next(), fields.next()) {
        (Some(kind), Some(reason), Some(data)) => Ok((kind, reason, data)),
        _ => bail!("Invalid packet capture completion record"),
    }
}

async fn download_capture(
    handle: &russh::client::Handle<crate::ssh::Handler>,
    remote: &str,
    parent: &Path,
    budget: u64,
    data: &str,
) -> Result<CaptureFile> {
    let (size, hash) = data
        .split_once('\t')
        .context("Incomplete packet capture record")?;
    let bytes: u64 = size.trim().parse().context("Reading packet capture size")?;
    ensure!(
        (24..=budget).contains(&bytes),
        "Remote packet capture exceeded its reviewed byte budget"
    );
    let sha256 = crate::sftp::normalize_sha256(hash)?;
    let path = format!("{remote}/{CAPTURE_FILE}");
    let file =
        crate::sftp::stage_diagnostic_download(handle, &path, parent, bytes, &sha256).await?;
    tokio::task::spawn_blocking(move || {
        let packets = count_packets(file.reopen()?, bytes)?;
        Ok(CaptureFile {
            file,
            bytes,
            sha256,
            packets,
        })
    })
    .await?
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

fn count_packets(file: std::fs::File, size: u64) -> Result<u32> {
    let mut file = BufReader::new(file);
    let mut header = [0; 24];
    file.read_exact(&mut header)?;
    count_packet_records(&mut file, &header, size)
}

fn count_packet_records(
    file: &mut BufReader<std::fs::File>,
    header: &[u8; 24],
    size: u64,
) -> Result<u32> {
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
            rotate_files: None,
            scratch_directory: "/tmp".into(),
            require_tmpfs: false,
        };
        let packet_bytes = u64::from(options.snap_length) + 16;
        let packet_limit = u64::from(options.effective_packets().unwrap());
        assert!(24 + packet_bytes * packet_limit <= options.budget_bytes());
        assert!(24 + packet_bytes * (packet_limit + 1) > options.budget_bytes());
        options.packet_limit = Some(2);
        assert_eq!(options.effective_packets(), Some(2));
        options.rotate_files = Some(4);
        assert!(options.clone().validate().is_err());
        options.snap_length = 192;
        options.packet_limit = None;
        let options = options.validate().unwrap();
        assert_eq!(options.effective_packets(), None);
        assert_eq!(options.rotation().unwrap().bytes_per_file, 256 * 1024);
        assert!(!options.tcpdump_arguments().contains(&"-c".into()));
    }
}
