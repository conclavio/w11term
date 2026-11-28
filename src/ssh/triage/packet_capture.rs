use super::{
    ArtifactMessages, ConnectionShare, PendingChannel, Target,
    capture_job::{CaptureStop, Kind, validate_job},
    diagnostic_destination, quote,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{BufReader, Read, Seek},
    net::{IpAddr, SocketAddr},
    ops::Range,
    path::{Path, PathBuf},
    time::Duration,
};

const SCRIPT: &str = include_str!("packet-capture.sh");
const RING_WORKER: &str = include_str!("packet-ring.py");
const CAPTURE_FILE: &str = "capture.pcap";
const METADATA_BYTES: usize = 4096;
const INTERFACE_BYTES: usize = 64 * 1024;
const MAX_INTERFACES: usize = 512;
const CAPTURE_OVERHEAD_SECONDS: u64 = 72;
pub(crate) const CONTROL_SECONDS: u64 = 155;
mod local;
mod ring;
mod settings;
pub use local::{
    CaptureComparison, CaptureExcerpt, CaptureExcerptPlan, ConversationChange,
    ConversationEndpoints, ConversationQuery, LocalCapture, ip_protocol_label,
};

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
            valid_interface(&self.interface),
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

fn valid_interface(name: &str) -> bool {
    name.len() <= 63
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:@".contains(&b))
}

#[derive(Serialize)]
pub struct CaptureInterface {
    pub name: String,
    pub detail: String,
}

#[derive(Serialize)]
pub struct CaptureInterfaces {
    pub interfaces: Vec<CaptureInterface>,
    pub truncated: bool,
}

fn parse_interfaces(bytes: &[u8]) -> CaptureInterfaces {
    let lines = String::from_utf8_lossy(bytes);
    let mut interfaces = Vec::new();
    let mut seen = HashSet::new();
    for line in lines.lines() {
        let Some((index, entry)) = line.split_once('.') else {
            continue;
        };
        if index.parse::<std::num::NonZeroU32>().is_err()
            || !index.bytes().all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let name = entry.split_whitespace().next().unwrap_or_default();
        if !valid_interface(name) || !entry.starts_with(name) {
            continue;
        }
        let detail = entry[name.len()..].trim();
        if !detail.is_empty() && !detail.starts_with(['(', '[']) {
            continue;
        }
        if !seen.insert(name) {
            continue;
        }
        if interfaces.len() == MAX_INTERFACES {
            return CaptureInterfaces {
                interfaces,
                truncated: true,
            };
        }
        interfaces.push(CaptureInterface {
            name: name.into(),
            detail: crate::events::display_field(detail),
        });
    }
    CaptureInterfaces {
        interfaces,
        truncated: false,
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
    pub fn list_packet_interfaces_blocking(mut self) -> Result<CaptureInterfaces> {
        crate::ssh::NETWORK.block_on(self.list_packet_interfaces())
    }

    pub(crate) async fn list_packet_interfaces(&mut self) -> Result<CaptureInterfaces> {
        let (stdout, _) = self
            .run_diagnostic(
                "packet capture interface listing",
                format!(
                    "/bin/bash -c {} w11term-packet-capture interfaces",
                    quote(SCRIPT)
                ),
                Duration::from_secs(8),
                INTERFACE_BYTES,
            )
            .await?;
        Ok(parse_interfaces(&stdout))
    }

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
    launch_wireshark(path, executable, None)
}

pub fn open_capture_interval_in_wireshark(
    path: &Path,
    executable: &Path,
    interval: Range<i128>,
) -> Result<u32> {
    let filter = wireshark_interval_filter(interval)?;
    launch_wireshark(path, executable, Some(&filter))
}

pub fn open_capture_conversation_in_wireshark(
    path: &Path,
    executable: &Path,
    a: IpAddr,
    b: IpAddr,
) -> Result<u32> {
    open_capture_pair_in_wireshark(path, executable, ConversationEndpoints::Ip { a, b }, None)
}

pub fn open_capture_conversation_interval_in_wireshark(
    path: &Path,
    executable: &Path,
    a: IpAddr,
    b: IpAddr,
    interval: Range<i128>,
) -> Result<u32> {
    open_capture_pair_in_wireshark(
        path,
        executable,
        ConversationEndpoints::Ip { a, b },
        Some(interval),
    )
}

pub fn open_capture_transport_in_wireshark(
    path: &Path,
    executable: &Path,
    protocol: u8,
    a: SocketAddr,
    b: SocketAddr,
) -> Result<u32> {
    open_capture_pair_in_wireshark(
        path,
        executable,
        ConversationEndpoints::Transport { protocol, a, b },
        None,
    )
}

pub fn open_capture_pair_in_wireshark(
    path: &Path,
    executable: &Path,
    endpoints: ConversationEndpoints,
    interval: Option<Range<i128>>,
) -> Result<u32> {
    let filter = wireshark_pair_filter(endpoints, interval)?;
    launch_wireshark(path, executable, Some(&filter))
}

fn wireshark_pair_filter(
    endpoints: ConversationEndpoints,
    interval: Option<Range<i128>>,
) -> Result<String> {
    let conversation = match endpoints {
        ConversationEndpoints::Ip { a, b } => wireshark_conversation_filter(a, b),
        ConversationEndpoints::Transport { protocol, a, b } => {
            wireshark_transport_filter(protocol, a, b)
        }
    }?;
    match interval {
        Some(interval) => Ok(format!(
            "({conversation}) && ({})",
            wireshark_interval_filter(interval)?
        )),
        None => Ok(conversation),
    }
}

// Wireshark numbers layers separately for IPv4 and IPv6. The prefix consumes
// the link-layer tokens without permitting any earlier IP header.
const OUTER_IP_PATTERN: &str = "^(?:(?!ip:|ipv6:)[^:]+:)*";

fn wireshark_ip_field(a: IpAddr, b: IpAddr) -> Result<&'static str> {
    Ok(match (a, b) {
        (IpAddr::V4(_), IpAddr::V4(_)) => "ip",
        (IpAddr::V6(_), IpAddr::V6(_)) => "ipv6",
        _ => bail!("Choose two addresses from the same IP version"),
    })
}

fn wireshark_conversation_filter(a: IpAddr, b: IpAddr) -> Result<String> {
    let protocol = wireshark_ip_field(a, b)?;
    let outer = format!(r#"frame.protocols matches "{OUTER_IP_PATTERN}{protocol}(?::|$)""#);
    let direction =
        |source, dest| format!("{protocol}.src#1 == {source} && {protocol}.dst#1 == {dest}");
    let forward = direction(a, b);
    if a == b {
        return Ok(format!("{outer} && {forward}"));
    }
    let reverse = direction(b, a);
    Ok(format!("{outer} && (({forward}) || ({reverse}))"))
}

fn wireshark_transport_filter(protocol: u8, a: SocketAddr, b: SocketAddr) -> Result<String> {
    let transport = match protocol {
        6 => "tcp",
        17 => "udp",
        _ => bail!("Choose a TCP or UDP conversation"),
    };
    let ip = wireshark_ip_field(a.ip(), b.ip())?;
    // Allow the IPv6 extensions supported by the summary between IP and
    // transport. A tunnel or an ICMP quote would introduce other tokens.
    let extensions = if ip == "ipv6" {
        "(?:ipv6[.](?:hopopts|routing|fraghdr|dstopts):|ah:)*"
    } else {
        ""
    };
    let outer = format!(
        r#"frame.protocols matches "{OUTER_IP_PATTERN}{ip}:{extensions}{transport}(?::|$)""#
    );
    let direction = |src: SocketAddr, dst: SocketAddr| {
        format!(
            "{ip}.src#1 == {} && {transport}.srcport#1 == {} && {ip}.dst#1 == {} && {transport}.dstport#1 == {}",
            src.ip(),
            src.port(),
            dst.ip(),
            dst.port()
        )
    };
    let forward = direction(a, b);
    if a == b {
        return Ok(format!("{outer} && {forward}"));
    }
    let reverse = direction(b, a);
    Ok(format!("{outer} && (({forward}) || ({reverse}))"))
}

fn wireshark_interval_filter(interval: Range<i128>) -> Result<String> {
    use chrono::{DateTime, Datelike, SecondsFormat};
    ensure!(
        interval.start < interval.end,
        "Choose a nonempty capture interval"
    );
    let utc = |timestamp: i128| -> Result<String> {
        let seconds = i64::try_from(timestamp.div_euclid(1_000_000_000)).ok();
        let nanos = timestamp.rem_euclid(1_000_000_000) as u32;
        let time = seconds
            .and_then(|seconds| DateTime::from_timestamp(seconds, nanos))
            .filter(|time| (1..=9999).contains(&time.year()))
            .context("The selected interval is outside Wireshark's calendar range")?;
        Ok(time.to_rfc3339_opts(SecondsFormat::Nanos, true))
    };
    Ok(format!(
        "frame.time >= \"{}\" && frame.time < \"{}\"",
        utc(interval.start)?,
        utc(interval.end)?,
    ))
}

fn launch_wireshark(path: &Path, executable: &Path, filter: Option<&str>) -> Result<u32> {
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
    let mut command = std::process::Command::new(executable);
    command.arg("-r").arg(path);
    if let Some(filter) = filter {
        command.arg("-Y").arg(filter);
    }
    Ok(command.spawn()?.id())
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

fn classic_little_endian(magic: [u8; 4]) -> Option<bool> {
    match magic {
        [0xd4, 0xc3, 0xb2, 0xa1] | [0x4d, 0x3c, 0xb2, 0xa1] => Some(true),
        [0xa1, 0xb2, 0xc3, 0xd4] | [0xa1, 0xb2, 0x3c, 0x4d] => Some(false),
        _ => None,
    }
}

fn count_packet_records(
    file: &mut BufReader<std::fs::File>,
    header: &[u8; 24],
    size: u64,
) -> Result<u32> {
    let format = ClassicPcap::parse(header)?;
    let mut remaining = size - 24;
    let mut packets = 0;
    while remaining > 0 {
        format.read_packet(file, &mut remaining, &mut [])?;
        packets += 1;
    }
    Ok(packets)
}

struct ClassicPcap {
    number: fn([u8; 4]) -> u32,
    snap: u32,
    nanoseconds: bool,
    link_type: u16,
}

impl ClassicPcap {
    fn parse(header: &[u8; 24]) -> Result<Self> {
        let little = classic_little_endian(header[..4].try_into().unwrap())
            .context("The transferred file is not a classic pcap capture")?;
        let number: fn([u8; 4]) -> u32 = if little {
            u32::from_le_bytes
        } else {
            u32::from_be_bytes
        };
        let version = if little { [2, 0, 4, 0] } else { [0, 2, 0, 4] };
        ensure!(header[4..8] == version, "Unsupported pcap file version");
        let snap = number(header[16..20].try_into().unwrap());
        ensure!(snap > 0, "Invalid pcap snapshot length");
        Ok(Self {
            number,
            snap,
            nanoseconds: matches!(
                &header[..4],
                [0x4d, 0x3c, 0xb2, 0xa1] | [0xa1, 0xb2, 0x3c, 0x4d]
            ),
            link_type: (number(header[20..24].try_into().unwrap()) & 0xffff) as u16,
        })
    }

    fn read_packet(
        &self,
        file: &mut (impl Read + Seek),
        remaining: &mut u64,
        prefix: &mut [u8],
    ) -> Result<([u32; 4], usize)> {
        ensure!(*remaining >= 16, "The pcap packet header is truncated");
        let mut header = [0; 16];
        file.read_exact(&mut header)?;
        let fields = std::array::from_fn(|index| {
            (self.number)(header[index * 4..index * 4 + 4].try_into().unwrap())
        });
        let [_, _, caplen, wirelen] = fields;
        ensure!(
            caplen <= self.snap && caplen <= wirelen && u64::from(caplen) <= *remaining - 16,
            "The pcap packet is incomplete or invalid"
        );
        let kept = prefix.len().min(caplen as usize);
        file.read_exact(&mut prefix[..kept])?;
        file.seek_relative(i64::from(caplen) - kept as i64)?;
        *remaining -= 16 + u64::from(caplen);
        Ok((fields, kept))
    }

    fn timestamp_ns(&self, seconds: u32, fraction: u32) -> Option<i128> {
        let limit = if self.nanoseconds {
            1_000_000_000
        } else {
            1_000_000
        };
        (fraction < limit).then(|| {
            i128::from(seconds) * 1_000_000_000
                + i128::from(fraction) * (1_000_000_000 / i128::from(limit))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wireshark_transport_filter_preserves_the_port_of_each_endpoint_and_outer_layer() {
        let a = "192.0.2.1:49152".parse().unwrap();
        let b = "192.0.2.2:443".parse().unwrap();
        assert_eq!(
            wireshark_transport_filter(6, a, b).unwrap(),
            concat!(
                r#"frame.protocols matches "^(?:(?!ip:|ipv6:)[^:]+:)*ip:tcp(?::|$)" && "#,
                "((ip.src#1 == 192.0.2.1 && tcp.srcport#1 == 49152 && ip.dst#1 == 192.0.2.2 && tcp.dstport#1 == 443) || ",
                "(ip.src#1 == 192.0.2.2 && tcp.srcport#1 == 443 && ip.dst#1 == 192.0.2.1 && tcp.dstport#1 == 49152))"
            )
        );
        let v6 = "[2001:db8::1]:5353".parse().unwrap();
        assert_eq!(
            wireshark_transport_filter(17, v6, v6).unwrap(),
            concat!(
                r#"frame.protocols matches "^(?:(?!ip:|ipv6:)[^:]+:)*ipv6:(?:ipv6[.](?:hopopts|routing|fraghdr|dstopts):|ah:)*udp(?::|$)" && "#,
                "ipv6.src#1 == 2001:db8::1 && udp.srcport#1 == 5353 && ipv6.dst#1 == 2001:db8::1 && udp.dstport#1 == 5353"
            )
        );
        assert!(wireshark_transport_filter(17, a, v6).is_err());
        assert!(wireshark_transport_filter(58, a, b).is_err());
    }

    #[test]
    fn wireshark_conversation_filter_matches_both_directions_at_the_outer_ip_layer() {
        let a = "192.0.2.1".parse().unwrap();
        let b = "192.0.2.2".parse().unwrap();
        let v6 = "2001:db8::1".parse().unwrap();
        let v6_peer = "2001:db8::2".parse().unwrap();
        assert_eq!(
            wireshark_conversation_filter(a, b).unwrap(),
            concat!(
                r#"frame.protocols matches "^(?:(?!ip:|ipv6:)[^:]+:)*ip(?::|$)" && "#,
                "((ip.src#1 == 192.0.2.1 && ip.dst#1 == 192.0.2.2) || ",
                "(ip.src#1 == 192.0.2.2 && ip.dst#1 == 192.0.2.1))"
            )
        );
        assert_eq!(
            wireshark_conversation_filter(v6, v6_peer).unwrap(),
            concat!(
                r#"frame.protocols matches "^(?:(?!ip:|ipv6:)[^:]+:)*ipv6(?::|$)" && "#,
                "((ipv6.src#1 == 2001:db8::1 && ipv6.dst#1 == 2001:db8::2) || ",
                "(ipv6.src#1 == 2001:db8::2 && ipv6.dst#1 == 2001:db8::1))"
            )
        );
        assert_eq!(
            wireshark_conversation_filter(v6, v6).unwrap(),
            concat!(
                r#"frame.protocols matches "^(?:(?!ip:|ipv6:)[^:]+:)*ipv6(?::|$)" && "#,
                "ipv6.src#1 == 2001:db8::1 && ipv6.dst#1 == 2001:db8::1"
            )
        );
        assert!(wireshark_conversation_filter(a, v6).is_err());
    }

    #[test]
    fn wireshark_interval_filters_keep_exact_utc_bounds_and_cross_the_epoch() {
        let filter = wireshark_interval_filter(-1..1_000_000_001).unwrap();
        assert_eq!(
            filter,
            "frame.time >= \"1969-12-31T23:59:59.999999999Z\" && frame.time < \"1970-01-01T00:00:01.000000001Z\""
        );
        assert!(wireshark_interval_filter(0..0).is_err());
        assert!(wireshark_interval_filter(0..i128::MAX).is_err());
    }

    #[test]
    fn wireshark_combines_the_entire_bidirectional_filter_with_exact_time_bounds() {
        let a = "192.0.2.1".parse().unwrap();
        let b = "192.0.2.2".parse().unwrap();
        assert_eq!(
            wireshark_pair_filter(
                ConversationEndpoints::Ip { a, b },
                Some(1_966_080_000..2_097_152_000)
            )
            .unwrap(),
            concat!(
                r#"(frame.protocols matches "^(?:(?!ip:|ipv6:)[^:]+:)*ip(?::|$)" && "#,
                "((ip.src#1 == 192.0.2.1 && ip.dst#1 == 192.0.2.2) || ",
                "(ip.src#1 == 192.0.2.2 && ip.dst#1 == 192.0.2.1))) && ",
                r#"(frame.time >= "1970-01-01T00:00:01.966080000Z" && frame.time < "1970-01-01T00:00:02.097152000Z")"#,
            )
        );
        assert!(wireshark_pair_filter(ConversationEndpoints::Ip { a, b }, Some(0..0)).is_err());
    }

    #[test]
    fn wireshark_limits_both_directions_of_an_exact_transport_pair_to_the_interval() {
        let endpoints = ConversationEndpoints::Transport {
            protocol: 17,
            a: "192.0.2.1:49152".parse().unwrap(),
            b: "192.0.2.2:443".parse().unwrap(),
        };
        assert_eq!(
            wireshark_pair_filter(endpoints, Some(-1..1)).unwrap(),
            concat!(
                r#"(frame.protocols matches "^(?:(?!ip:|ipv6:)[^:]+:)*ip:udp(?::|$)" && "#,
                "((ip.src#1 == 192.0.2.1 && udp.srcport#1 == 49152 && ip.dst#1 == 192.0.2.2 && udp.dstport#1 == 443) || ",
                "(ip.src#1 == 192.0.2.2 && udp.srcport#1 == 443 && ip.dst#1 == 192.0.2.1 && udp.dstport#1 == 49152))) && ",
                r#"(frame.time >= "1969-12-31T23:59:59.999999999Z" && frame.time < "1970-01-01T00:00:00.000000001Z")"#,
            ),
        );
        assert!(wireshark_pair_filter(endpoints, Some(1..1)).is_err());
    }

    #[test]
    fn interface_list_preserves_literal_names_and_skips_unselectable_entries() {
        let list = parse_interfaces(
            concat!(
                "1.enp0s3 [Up, Running]\n",
                "2.any (Pseudo-device) [Up, Running]\n",
                "3.lo [Up, Running, Loopback]\r\n",
                "4.veth.100@peer [Up]\n",
                "  dummy0 [NoCapture]\n",
                "5.-some-option [Up]\n",
                "6.enp0s3 (duplicate)\n",
                "7.bad trailing text\n",
                "0.zero\n",
                "8.can0 [Up\x1b[0m]\n",
            )
            .as_bytes(),
        );
        assert!(!list.truncated);
        assert_eq!(
            list.interfaces
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["enp0s3", "any", "lo", "veth.100@peer", "can0"]
        );
        assert_eq!(list.interfaces[1].detail, "(Pseudo-device) [Up, Running]");
        assert!(!list.interfaces[4].detail.contains('\x1b'));
    }

    #[test]
    fn interface_catalog_is_bounded_and_reports_truncation() {
        use std::fmt::Write;
        let mut wire = String::new();
        for number in 1..=MAX_INTERFACES + 1 {
            writeln!(wire, "{number}.eth{number}").unwrap();
        }
        let list = parse_interfaces(wire.as_bytes());
        assert_eq!(list.interfaces.len(), MAX_INTERFACES);
        assert!(list.truncated);
    }

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
