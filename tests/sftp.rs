//! Real SSH + SFTP packets, a temporary server filesystem, and the public CLI/pipe.
use russh::{Channel, ChannelId, server};
use russh_sftp::protocol::*;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use w11term::{
    control::{self, Pane, Registry, Request},
    options::{Authentication, Source, SshOptions},
    session::{Command, Session},
};

struct Peer {
    channels: HashMap<ChannelId, Channel<server::Msg>>,
    wire: Option<tokio::sync::mpsc::UnboundedSender<in_band_resize::Wire>>,
    forwarded: HashSet<ChannelId>,
    #[cfg(feature = "ui-smoke")]
    withheld_session_open: Option<server::ChannelOpenHandle>,
    root: PathBuf,
    opens: Arc<AtomicUsize>,
    posix_entries: text_save::PosixEntries,
    reject: bool,
}

const PERF_DATA_FIXTURE: &[u8] = b"PERFILE2\0\xff\r\nraw perf fixture\0";
const SYSCALL_FIXTURE: &[u8] =
    b"720 1700000000.000001 openat(AT_FDCWD, \"missing\", O_RDONLY) = -1 ENOENT (No such file or directory) <0.000002>\n";
// Classic little-endian pcap, one captured packet with a four-byte payload.
const PCAP_FIXTURE: &[u8] = b"\xd4\xc3\xb2\xa1\x02\0\x04\0\0\0\0\0\0\0\0\0\xc0\0\0\0\x01\0\0\0\
    \x01\0\0\0\0\0\0\0\x04\0\0\0\x04\0\0\0\xab\xcd\xef\x01";

fn packet_ring_segment(slot: u8) -> Vec<u8> {
    let mut bytes = PCAP_FIXTURE.to_vec();
    *bytes.last_mut().unwrap() = slot + 10;
    bytes
}

fn packet_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn packet_capture_ready(
    session: &mut server::Session,
    channel: ChannelId,
    reason: &str,
    bad_digest: bool,
    rolling: bool,
) -> anyhow::Result<()> {
    let line = if rolling {
        let segments = [(1, 1), (2, 0)].map(|(sequence, slot)| {
            let bytes = packet_ring_segment(slot);
            json!({"sequence": sequence, "bytes": bytes.len(), "sha256": packet_digest(&bytes)})
        });
        let manifest = json!({"packets": 3, "segments": segments});
        format!("w11term-packet-ring-v1\t{reason}\t{manifest}\n")
    } else {
        let sha256 = if bad_digest {
            "0".repeat(64)
        } else {
            packet_digest(PCAP_FIXTURE)
        };
        format!(
            "w11term-packet-ready-v1\t{reason}\t{}\t{sha256}\n",
            PCAP_FIXTURE.len()
        )
    };
    session.extended_data(
        channel,
        1,
        if rolling {
            b"3 packets captured\n3 packets received by filter\n0 packets dropped by kernel\n"
                .to_vec()
        } else {
            b"1 packet captured\n2 packets received by filter\n0 packets dropped by kernel\n"
                .to_vec()
        },
    )?;
    session.data(channel, line.into_bytes())?;
    Ok(())
}

fn hold_profile_for_stop(
    root: &Path,
    channel: ChannelId,
    raw: bool,
    response: &[u8],
) -> std::io::Result<bool> {
    if !root.join("profile-wait-for-stop").is_file() {
        return Ok(false);
    }
    let mut payload = Vec::new();
    if raw {
        payload.extend_from_slice(perf_data_header().as_bytes());
        payload.extend_from_slice(PERF_DATA_FIXTURE);
    }
    payload.extend_from_slice(response);
    fs::write(root.join(format!("profile-stop-{channel}")), payload)?;
    Ok(true)
}

fn perf_data_header() -> String {
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(PERF_DATA_FIXTURE)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(
        "w11term-perf-data-v1\t{}\t{digest}\n",
        PERF_DATA_FIXTURE.len()
    )
}

fn set_fixture_modified(path: &std::path::Path, seconds: u64) -> std::time::SystemTime {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES};

    let time = std::time::UNIX_EPOCH + Duration::from_secs(seconds);
    fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
    time
}

impl server::Handler for Peer {
    type Error = anyhow::Error;
    async fn auth_none(&mut self, _user: &str) -> anyhow::Result<server::Auth> {
        #[cfg(feature = "ui-smoke")]
        if self.root.join("authorized-key").is_file()
            || self.root.join("authorized-certificate").is_file()
        {
            return Ok(server::Auth::reject());
        }
        #[cfg(feature = "ui-smoke")]
        if self.root.join("authenticated-users").is_file() {
            writeln!(
                fs::OpenOptions::new()
                    .append(true)
                    .open(self.root.join("authenticated-users"))?,
                "{_user}"
            )?;
        }
        Ok(server::Auth::Accept)
    }
    #[cfg(feature = "ui-smoke")]
    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        key: &russh::keys::PublicKey,
    ) -> anyhow::Result<server::Auth> {
        let path = self.root.join("offered-key-trace");
        if path.is_file() {
            writeln!(
                fs::OpenOptions::new().append(true).open(path)?,
                "{}",
                key.fingerprint(russh::keys::HashAlg::Sha256)
            )?;
        }
        Ok(server::Auth::Accept)
    }
    #[cfg(feature = "ui-smoke")]
    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: &russh::keys::PublicKey,
    ) -> anyhow::Result<server::Auth> {
        let allowed = self.root.join("authorized-key");
        if !allowed.is_file() {
            return Ok(server::Auth::reject());
        }
        let expected = russh::keys::PublicKey::from_openssh(&fs::read_to_string(allowed)?)?;
        Ok(if key.key_data() == expected.key_data() {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    #[cfg(feature = "ui-smoke")]
    async fn auth_openssh_certificate(
        &mut self,
        _user: &str,
        certificate: &russh::keys::Certificate,
    ) -> anyhow::Result<server::Auth> {
        let path = self.root.join("authorized-certificate");
        if !path.is_file() {
            return Ok(server::Auth::reject());
        }
        let expected = russh::keys::load_openssh_certificate(path)?;
        Ok(if *certificate == expected {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> anyhow::Result<()> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        #[cfg(feature = "ui-smoke")]
        {
            let gate = self.root.join("hold-session-open");
            while gate.is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if self.root.join("withhold-session-open").is_file() {
                // Keep the SSH connection responsive while leaving the request unanswered.
                self.withheld_session_open = Some(reply);
                return Ok(());
            }
        }
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<server::Msg>,
        host: &str,
        port: u32,
        origin: &str,
        origin_port: u32,
        reply: server::ChannelOpenHandle,
        _: &mut server::Session,
    ) -> anyhow::Result<()> {
        let trace = self.root.join("forward-trace");
        if trace.is_file() {
            writeln!(
                fs::OpenOptions::new().append(true).open(trace)?,
                "{host}:{port} {origin}:{origin_port}"
            )?;
        }
        let allowed = fs::read_to_string(self.root.join("forward-port"))
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok());
        let Some(target) = allowed.filter(|p| {
            host == "127.0.0.1"
                && u32::from(*p) == port
                && !self.root.join("forward-refuse").is_file()
        }) else {
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        };
        let Ok(mut socket) =
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, target)).await
        else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        self.forwarded.insert(channel.id());
        reply.accept().await;
        tokio::spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut channel.into_stream(), &mut socket).await;
        });
        Ok(())
    }
    async fn pty_request(
        &mut self,
        c: ChannelId,
        _: &str,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        s: &mut server::Session,
    ) -> anyhow::Result<()> {
        #[cfg(feature = "ui-smoke")]
        if self.root.join("pty-trace").is_file() {
            writeln!(
                fs::OpenOptions::new()
                    .append(true)
                    .open(self.root.join("pty-trace"))?,
                "{c}"
            )?;
        }
        let grid_trace = self.root.join("pty-grid-trace");
        if cfg!(feature = "ui-smoke") && grid_trace.is_file() {
            writeln!(
                fs::OpenOptions::new().append(true).open(grid_trace)?,
                "{cols}x{rows}"
            )?;
        }
        s.channel_success(c)?;
        Ok(())
    }
    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        cols: u32,
        rows: u32,
        width: u32,
        height: u32,
        session: &mut server::Session,
    ) -> anyhow::Result<()> {
        if let Some(wire) = &self.wire {
            let _ = wire.send(in_band_resize::Wire::Window(
                channel,
                [cols, rows, width, height],
            ));
        }
        if cfg!(feature = "ui-smoke") && self.root.join("keyboard-input.bin").exists() {
            session.data(
                channel,
                format!("\r\nssh-grid-{cols}x{rows}\r\n").into_bytes(),
            )?;
        }
        Ok(())
    }
    async fn exec_request(
        &mut self,
        c: ChannelId,
        command: &[u8],
        s: &mut server::Session,
    ) -> anyhow::Result<()> {
        if let Some((_, args)) = std::str::from_utf8(command)
            .ok()
            .filter(|command| command.starts_with("/bin/bash -c '# w11term-packet-capture-v1"))
            .and_then(|command| command.rsplit_once("' w11term-packet-capture "))
        {
            s.channel_success(c)?;
            if args == "interfaces" {
                s.data(c, b"1.eth0 [Up, Running, Connected]\n2.any (Pseudo-device) [Up, Running]\n3.lo [Up, Running, Loopback]\n".as_slice())?;
                s.exit_status_request(c, 0)?;
                s.eof(c)?;
                s.close(c)?;
            } else if args.starts_with("inspect ") {
                s.data(
                    c,
                    b"w11term-packet-plan-v1\ttmpfs\t12:34\t/dev/shm\n".to_vec(),
                )?;
                s.exit_status_request(c, 0)?;
                s.eof(c)?;
                s.close(c)?;
            } else {
                let ring = args.rsplit_once(" w11term-ring-v1 ");
                let job = ring
                    .map_or(args, |(prefix, _)| prefix)
                    .rsplit_once(' ')
                    .unwrap()
                    .1
                    .trim_matches('\'');
                anyhow::ensure!(
                    job.len() == 32 && job.bytes().all(|byte| byte.is_ascii_hexdigit())
                );
                fs::write(self.root.join("pcap-started"), args)?;
                let remote = self
                    .root
                    .join("dev")
                    .join("shm")
                    .join(format!("w11term-pcap-{job}"));
                fs::create_dir_all(&remote)?;
                if ring.is_some() {
                    for slot in 0..2 {
                        fs::write(
                            remote.join(format!("segment-{slot}.pcap")),
                            packet_ring_segment(slot),
                        )?;
                    }
                } else {
                    fs::write(remote.join("capture.pcap"), PCAP_FIXTURE)?;
                }
                fs::write(
                    self.root.join(format!("pcap-ack-{c}")),
                    remote.to_str().unwrap(),
                )?;
                if !self.root.join("pcap-wait-for-stop").is_file() {
                    packet_capture_ready(
                        s,
                        c,
                        "packets",
                        self.root.join("pcap-bad-digest").is_file(),
                        ring.is_some(),
                    )?;
                }
            }
            return Ok(());
        }
        if let Some((_, args)) = std::str::from_utf8(command)
            .ok()
            .filter(|command| command.starts_with("/bin/bash -c '# w11term-syscall-capture-v1"))
            .and_then(|command| command.rsplit_once("' w11term-syscall-capture "))
        {
            s.channel_success(c)?;
            if args.starts_with("inspect ") {
                s.data(
                    c,
                    b"w11term-syscall-plan-v1\t/home/test\t/bin/sh\tfixture-identity\n".to_vec(),
                )?;
                s.exit_status_request(c, 0)?;
                s.eof(c)?;
                s.close(c)?;
            } else {
                let job = args
                    .split_ascii_whitespace()
                    .nth(10)
                    .unwrap()
                    .trim_matches('\'');
                anyhow::ensure!(
                    job.len() == 32 && job.bytes().all(|byte| byte.is_ascii_hexdigit())
                );
                let remote = self.root.join("tmp").join(format!("w11term-strace-{job}"));
                fs::create_dir_all(&remote)?;
                fs::write(remote.join("trace.strace"), SYSCALL_FIXTURE)?;
                fs::write(
                    self.root.join(format!("syscall-ack-{c}")),
                    remote.to_str().unwrap(),
                )?;
                fs::write(self.root.join("syscall-started"), args)?;
            }
            return Ok(());
        }
        if command.starts_with(b"/bin/sh -c '# w11term-linux-triage-v1")
            && let Ok(report) = fs::read(self.root.join("triage-response"))
        {
            fs::write(self.root.join("triage-command"), command)?;
            s.channel_success(c)?;
            if self.root.join("triage-wait-for-stop").is_file() {
                fs::write(self.root.join(format!("triage-stop-{c}")), report)?;
                s.data(
                    c,
                    concat!(
                        "w11term-triage-progress\tuptime\n",
                        "w11term-triage-progress\tdmesg\n",
                        "w11term-triage-progress\tvmstat\n",
                    )
                    .as_bytes()
                    .to_vec(),
                )?;
                return Ok(());
            }
            s.data(c, report)?;
            s.exit_status_request(c, 0)?;
            s.eof(c)?;
            s.close(c)?;
            return Ok(());
        }
        if let Ok(command) = std::str::from_utf8(command)
            && command.starts_with("/bin/sh -c '# w11term-cpu-profile-v1")
            && command.contains("w11term-cpu-launch-v1")
        {
            use base64::{Engine as _, engine::general_purpose::STANDARD};
            let directory = "/tmp/work space";
            let program = "/tmp/work space/cpu worker";
            let mut reply = format!(
                "w11term-cpu-launch-v1\t{}\t{}\n",
                STANDARD.encode(directory),
                STANDARD.encode(program),
            );
            let argv = r#"'one arg' 'it'\''s $literal' '' '--child'"#;
            let inspect = format!(" 2 73 fp 32768 4096 '{directory}' './cpu worker' '' '' {argv}");
            let launch = format!(
                " 2 73 fp 32768 4096 '{directory}' './cpu worker' '{directory}' '{program}' {argv}"
            );
            fs::write(self.root.join("profile-launch-wire"), command)?;
            s.channel_success(c)?;
            if command.ends_with(&launch) {
                fs::write(self.root.join("profile-launched"), [])?;
                let raw = command.contains(" 74 launch-raw ");
                let outcome = if self.root.join("profile-wait-for-stop").is_file() {
                    "stopped\n"
                } else {
                    "timeout\n"
                };
                reply.push_str(outcome);
                reply.push_str(concat!(
                    "42 20.0: cpu-clock:u: ab main (/tmp/work space/cpu worker)\n",
                    "43 20.1: cpu-clock:u: ac child (/tmp/work space/cpu worker)\n",
                    "\nw11term-cpu-profile-end\n",
                ));
                if hold_profile_for_stop(&self.root, c, raw, reply.as_bytes())? {
                    return Ok(());
                }
                if raw {
                    s.data(c, perf_data_header().into_bytes())?;
                    s.data(c, PERF_DATA_FIXTURE.to_vec())?;
                }
            } else if !command.ends_with(&inspect) {
                let tail = command.chars().rev().take(200).collect::<Vec<_>>();
                let tail: String = tail.into_iter().rev().collect();
                s.extended_data(
                    c,
                    1,
                    format!("Unexpected reviewed command suffix: {tail}").into_bytes(),
                )?;
                s.exit_status_request(c, 2)?;
                s.eof(c)?;
                s.close(c)?;
                return Ok(());
            }
            s.data(c, reply.into_bytes())?;
            s.exit_status_request(c, 0)?;
            s.eof(c)?;
            s.close(c)?;
            return Ok(());
        }
        if let Some((_, args)) = std::str::from_utf8(command)
            .ok()
            .filter(|command| command.starts_with("/bin/sh -c '# w11term-cpu-profile-v1"))
            .and_then(|command| command.rsplit_once("' "))
        {
            let args: Vec<_> = args.split_ascii_whitespace().collect();
            s.channel_success(c)?;
            match args.as_slice() {
                ["4", "inspect", "42", "0", "2", "99", "fp", "32768", "4096"] => {
                    let ticks = fs::read_to_string(self.root.join("profile-start-ticks"))
                        .ok()
                        .and_then(|value| value.parse::<u64>().ok())
                        .unwrap_or(800);
                    s.data(
                        c,
                        format!("w11term-cpu-profile-v1\t{ticks}\td29ya2Vy\n").into_bytes(),
                    )?;
                    s.exit_status_request(c, 0)?;
                }
                [
                    "14" | "74",
                    "capture" | "capture-raw",
                    "42",
                    "800",
                    "2",
                    "99",
                    "fp",
                    "32768",
                    "4096",
                ] => {
                    let response = fs::read(self.root.join("profile-response"))?;
                    fs::write(self.root.join("profile-captured"), [])?;
                    if hold_profile_for_stop(&self.root, c, args[1] == "capture-raw", &response)? {
                        return Ok(());
                    }
                    if args[1] == "capture-raw" {
                        s.data(c, perf_data_header().into_bytes())?;
                        for chunk in PERF_DATA_FIXTURE.chunks(7) {
                            s.data(c, chunk.to_vec())?;
                        }
                    }
                    s.data(c, response)?;
                    s.extended_data(c, 1, b"Processed 3 events and lost 1 chunks!\n".to_vec())?;
                    s.exit_status_request(c, 0)?;
                }
                _ => {
                    s.extended_data(c, 1, b"The target PID was reused.\n".to_vec())?;
                    s.exit_status_request(c, 2)?;
                }
            }
            s.eof(c)?;
            s.close(c)?;
            return Ok(());
        }
        if let Some(args) = std::str::from_utf8(command).ok().and_then(|command| {
            command
                .split_once("' w11term-tools-sha256 ")
                .map(|(_, args)| args)
        }) {
            let Some(report) = utilities::tool_digest_report(&self.root, args)? else {
                s.channel_failure(c)?;
                return Ok(());
            };
            s.channel_success(c)?;
            s.data(c, report.into_bytes())?;
            s.exit_status_request(c, 0)?;
            s.eof(c)?;
            s.close(c)?;
            return Ok(());
        }
        if command.starts_with(b"/bin/sh -c 'exec 9<")
            && self.root.join("utility-probe-response").is_file()
        {
            s.channel_success(c)?;
            if self.root.join("hold-utility-lock").is_file() {
                s.exit_status_request(c, 3)?;
                s.eof(c)?;
                s.close(c)?;
            } else {
                s.data(c, b"w11term-tools-locked-v1\n".as_slice())?;
            }
            return Ok(());
        }
        if command.starts_with(b"/bin/sh -c 'printf")
            && let Ok(report) = fs::read(self.root.join("utility-probe-response"))
        {
            fs::write(self.root.join("utility-probe-command"), command)?;
            s.channel_success(c)?;
            s.data(c, report)?;
            s.exit_status_request(c, 0)?;
            s.eof(c)?;
            s.close(c)?;
            return Ok(());
        }
        let trace = self.root.join("workspace-exec.bin");
        if !trace.exists() {
            s.channel_failure(c)?;
            return Ok(());
        }
        fs::OpenOptions::new()
            .append(true)
            .open(trace)?
            .write_all(command)?;
        let drop_once = self.root.join("drop-before-exec-reply");
        if drop_once.exists() {
            fs::remove_file(drop_once)?;
            s.disconnect(
                russh::Disconnect::ByApplication,
                "fixture lost link before exec reply",
                "en",
            )?;
            return Ok(());
        }
        s.channel_success(c)?;
        s.data(c, b"workspace-command-started\r\n".as_slice())?;
        Ok(())
    }
    async fn shell_request(&mut self, c: ChannelId, s: &mut server::Session) -> anyhow::Result<()> {
        #[cfg(feature = "ui-smoke")]
        {
            if self.root.join("withhold-shell-reply").is_file() {
                fs::write(self.root.join("shell-reply-withheld"), [])?;
                return Ok(());
            }
            let gate = self.root.join("hold-shell-request");
            while gate.is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if self.root.join("reject-shell-request").is_file() {
                s.channel_failure(c)?;
                return Ok(());
            }
            if self.root.join("drop-shell-before-ready").is_file() {
                s.disconnect(
                    russh::Disconnect::ByApplication,
                    "fixture drops login before the shell is ready",
                    "en",
                )?;
                return Ok(());
            }
        }
        s.channel_success(c)?;
        if let Some(wire) = &self.wire {
            let _ = wire.send(in_band_resize::Wire::Shell(c, s.handle()));
        }
        if let Ok(probe) = fs::read_to_string(self.root.join("dotfiles-shell-probe")) {
            let (shell, extension, startups): (_, _, &[_]) = if probe == "fish" {
                ("fish", "fish", &[".config/fish/config.fish"])
            } else {
                ("bash", "sh", &[".bashrc", ".profile"])
            };
            let managed = self.root.join(".config/w11term/dotfiles");
            let active =
                fs::read_to_string(managed.join(format!("{shell}-current"))).unwrap_or_default();
            let value = active.lines().next().unwrap_or("absent");
            let ready = if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                // Check the files at the instant of the SSH shell request.
                let bundle = managed.join(format!("{shell}-{value}"));
                bundle.join(format!("prompt.{extension}")).is_file()
                    && bundle.join(format!("environment.{extension}")).is_file()
                    && startups.iter().all(|path| self.root.join(path).is_file())
            } else {
                value == "disabled"
            };
            s.data(
                c,
                format!(
                    "dotfiles-at-shell:{}\r\n",
                    if ready { value } else { "absent" }
                )
                .into_bytes(),
            )?;
        }
        if let Ok(probe) = fs::read_to_string(self.root.join("utility-shell-probe")) {
            let store = self.root.join(".local/share/w11term/tools/portable");
            let selector = fs::read_to_string(store.join("current")).unwrap_or_default();
            let target = selector.lines().nth(1).unwrap_or_default();
            let id = target.rsplit('/').next().filter(|id| id.len() == 64);
            let id = id
                .filter(|id| {
                    store.join("versions").join(id).join("rg").is_file()
                        && self.root.join(".local/bin/rg").is_file()
                })
                .unwrap_or("absent");
            let startup = if probe == "fish" {
                ".config/fish/config.fish"
            } else {
                ".bashrc"
            };
            let path = fs::read_to_string(self.root.join(startup))
                .unwrap_or_default()
                .contains("w11term tool PATH");
            s.data(c, format!("tools-at-shell:{id}:{path}\r\n").into_bytes())?;
        }
        s.data(c, b"ready\r\n".as_slice())?;
        #[cfg(feature = "ui-smoke")]
        if self.root.join("copy-history.vt").is_file() {
            s.data(c, fs::read(self.root.join("copy-history.vt"))?)?;
        }
        #[cfg(feature = "ui-smoke")]
        if self.root.join("hold-shell-output").is_file() {
            let gate = self.root.join("hold-shell-output");
            let output = self.root.join("deferred-output.vt");
            // Reserve prewritten output for this shell. A fixture that needs
            // the measured PTY can instead write the data before releasing it.
            let prepared = match fs::read(&output) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            let peer = s.handle();
            tokio::spawn(async move {
                while gate.is_file() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                let bytes = match prepared {
                    Some(bytes) => bytes,
                    None => fs::read(output)
                        .expect("write deferred fixture output before releasing its gate"),
                };
                let _ = peer.data(c, bytes).await;
            });
        }
        #[cfg(feature = "ui-smoke")]
        {
            // Let a read-only, background pane lose its connection from the server side.
            let gate = self.root.join("keep-first-connection");
            if gate.is_file() {
                let peer = s.handle();
                tokio::spawn(async move {
                    while gate.is_file() {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    let _ = peer
                        .disconnect(
                            russh::Disconnect::ByApplication,
                            "fixture reconnect".into(),
                            "en".into(),
                        )
                        .await;
                });
            }
        }
        if self.root.join("shell directory").is_dir() {
            s.data(c, b"\x1b]133;A\x07$ \x1b]133;B\x07first\r\n\x1b]133;C\x07first result\r\n\x1b]133;D;0\x07".as_slice())?;
            s.data(
                c,
                b"\x1b]133;A\x07$ \x1b]133;B\x07status\r\n\x1b]133;C\x07".as_slice(),
            )?;
            s.data(
                c,
                b"\x1b[35mfixture status\x1b[0m\r\n\x1b]133;D;7\x07".as_slice(),
            )?;
            s.data(
                c,
                b"src/trace.rs:11:1\r\n\x1b]7;file://127.0.0.1/shell%20directory\x1b\\".as_slice(),
            )?;
        }
        Ok(())
    }
    async fn data(
        &mut self,
        c: ChannelId,
        data: &[u8],
        s: &mut server::Session,
    ) -> anyhow::Result<()> {
        if data == b"stop\n" {
            let profile = self.root.join(format!("profile-stop-{c}"));
            if let Ok(report) = fs::read(&profile) {
                fs::remove_file(profile)?;
                s.data(c, report)?;
                s.extended_data(c, 1, b"Processed 3 events and lost 1 chunks!\n".to_vec())?;
                s.exit_status_request(c, 0)?;
                s.eof(c)?;
                s.close(c)?;
                return Ok(());
            }
            let triage = self.root.join(format!("triage-stop-{c}"));
            if let Ok(report) = fs::read(&triage) {
                fs::remove_file(triage)?;
                s.data(c, report)?;
                s.exit_status_request(c, 0)?;
                s.eof(c)?;
                s.close(c)?;
                return Ok(());
            }
        }
        let capture_marker = self.root.join(format!("pcap-ack-{c}"));
        if data == b"stop\n"
            && self.root.join("pcap-wait-for-stop").is_file()
            && let Ok(remote) = fs::read_to_string(&capture_marker)
        {
            fs::write(self.root.join("pcap-stopped"), [])?;
            packet_capture_ready(
                s,
                c,
                "stopped",
                false,
                Path::new(&remote).join("segment-0.pcap").is_file(),
            )?;
            return Ok(());
        }
        if data == b"verified\n"
            && let Ok(remote) = fs::read_to_string(&capture_marker)
        {
            let remote = PathBuf::from(remote);
            if remote.join("segment-0.pcap").is_file() {
                fs::remove_file(remote.join("segment-0.pcap"))?;
                fs::remove_file(remote.join("segment-1.pcap"))?;
            } else {
                fs::remove_file(remote.join("capture.pcap"))?;
            }
            fs::remove_dir(remote)?;
            fs::remove_file(capture_marker)?;
            fs::write(self.root.join("pcap-verified"), [])?;
            s.data(c, b"w11term-packet-complete-v1\n".to_vec())?;
            s.exit_status_request(c, 0)?;
            s.eof(c)?;
            s.close(c)?;
            return Ok(());
        }
        let syscall_marker = self.root.join(format!("syscall-ack-{c}"));
        if let Ok(remote) = fs::read_to_string(&syscall_marker) {
            if data == b"stop\n" {
                use sha2::{Digest, Sha256};
                let hash: String = Sha256::digest(SYSCALL_FIXTURE)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                s.data(
                    c,
                    format!(
                        "w11term-syscall-ready-v1\tstopped\t-\t{}\t{hash}\n",
                        SYSCALL_FIXTURE.len()
                    )
                    .into_bytes(),
                )?;
            } else if data == b"verified\n" {
                let remote = PathBuf::from(remote);
                fs::remove_file(remote.join("trace.strace"))?;
                fs::remove_dir(remote)?;
                fs::remove_file(syscall_marker)?;
                fs::write(self.root.join("syscall-verified"), [])?;
                s.data(c, b"w11term-syscall-complete-v1\n".to_vec())?;
                s.exit_status_request(c, 0)?;
                s.eof(c)?;
                s.close(c)?;
            }
            return Ok(());
        }
        if self.channels.contains_key(&c) {
            if let Some(wire) = &self.wire {
                let _ = wire.send(in_band_resize::Wire::Input(c, data.to_vec()));
                return Ok(());
            }
            #[cfg(feature = "ui-smoke")]
            if self.root.join("split-inputs").is_dir() {
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.root.join("split-inputs").join(c.to_string()))?
                    .write_all(data)?;
                if self.root.join("notification-fixture").is_file() {
                    if let Some(outbound) = data.strip_prefix(b"notification-fixture:") {
                        s.data(c, outbound.to_vec())?;
                        return Ok(());
                    }
                    if data.starts_with(b"\x1b]99;") {
                        // Record real terminal replies without reflecting them back as requests.
                        return Ok(());
                    }
                }
                let report: Option<&[u8]> = match data {
                    b"report-foreign" => Some(b"\x1b]7;file://unrelated.invalid/srv/foreign\x1b\\\r\nforeign-reported\r\n"),
                    b"report-current" => Some(b"\x1b]7;file://127.0.0.1/srv/Pilot%27s%20Work\x1b\\\r\ncurrent-reported\r\n"),
                    b"report-current-again" => Some(b"\x1b]7;file://127.0.0.1/srv/Pilot%27s%20Work\x1b\\\r\ncurrent-confirmed\r\n"),
                    b"nested-host" => Some(b"\x1b]1337;RemoteHost=test@nested.invalid\x07nested-host-seen\r\n"),
                    b"nested-dir" => Some(b"\x1b]1337;CurrentDir=/tmp/nested-only\x07nested-dir-seen\r\n"),
                    b"other-login" => Some(b"\x1b]1337;RemoteHost=other@127.0.0.1\x07\x1b]1337;CurrentDir=/home/other\x07other-login-seen\r\n"),
                    b"login-host" => Some(b"\x1b]1337;RemoteHost=test@127.0.0.1\x1b\\login-host-seen\r\n"),
                    b"literal-dir" => Some("\x1b]1337;CurrentDir=/srv/Pilot's 100%2F;#雪\x1b\\literal-dir-seen\r\n".as_bytes()),
                    _ => None,
                };
                if let Some(report) = report {
                    s.data(c, report)?;
                    return Ok(());
                }
            }
            #[cfg(feature = "ui-smoke")]
            if self.root.join("keyboard-input.bin").exists() {
                fs::OpenOptions::new()
                    .append(true)
                    .open(self.root.join("keyboard-input.bin"))?
                    .write_all(data)?;
            }
            #[cfg(feature = "ui-smoke")]
            if data == b"\x04" && self.root.join("exit-on-eot").is_file() {
                s.exit_status_request(c, 23)?;
                s.eof(c)?;
                s.close(c)?;
                return Ok(());
            }
            #[cfg(feature = "ui-smoke")]
            if data == b"\x04" && self.root.join("disconnect-on-eot").is_file() {
                s.disconnect(
                    russh::Disconnect::ByApplication,
                    "test connection loss",
                    "en",
                )?;
                return Ok(());
            }
            s.data(c, data.to_vec())?;
        }
        Ok(())
    }
    async fn channel_eof(&mut self, c: ChannelId, s: &mut server::Session) -> anyhow::Result<()> {
        // A forwarded TCP client may still read a reply after shutting down its write half.
        if !self.forwarded.contains(&c) {
            s.close(c)?;
        }
        Ok(())
    }
    async fn channel_close(&mut self, c: ChannelId, _: &mut server::Session) -> anyhow::Result<()> {
        self.forwarded.remove(&c);
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        c: ChannelId,
        name: &str,
        s: &mut server::Session,
    ) -> anyhow::Result<()> {
        if self.reject || name != "sftp" {
            s.channel_failure(c)?;
            return Ok(());
        }
        s.channel_success(c)?;
        let channel = self.channels.remove(&c).unwrap();
        let peer = Files {
            root: self.root.clone(),
            ssh: s.handle(),
            handles: HashMap::new(),
            posix: None,
            posix_entries: self.posix_entries.clone(),
            next: 0,
        };
        tokio::spawn(async move {
            russh_sftp::server::run(channel.into_stream(), peer).await;
        });
        Ok(())
    }
}
enum Opened {
    File(fs::File),
    Dir(Vec<russh_sftp::protocol::File>),
}
struct Files {
    root: PathBuf,
    ssh: server::Handle,
    handles: HashMap<String, Opened>,
    posix: Option<text_save::PosixMetadata>,
    posix_entries: text_save::PosixEntries,
    next: u64,
}
fn status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: String::new(),
    }
}
fn io(e: std::io::Error) -> StatusCode {
    if e.kind() == std::io::ErrorKind::NotFound {
        StatusCode::NoSuchFile
    } else {
        StatusCode::Failure
    }
}
fn entry_attrs(path: &std::path::Path) -> Result<FileAttributes, StatusCode> {
    let metadata = fs::symlink_metadata(path).map_err(io)?;
    let mut attrs = FileAttributes::from(&metadata);
    // russh-sftp's Windows Metadata conversion treats non-directories as regular files.
    if metadata.file_type().is_symlink() {
        attrs.permissions =
            Some((attrs.permissions.unwrap_or_default() & !0o170000) | FileMode::LNK.bits());
    }
    Ok(attrs)
}
impl Files {
    fn trace_transfer(&self, request: impl std::fmt::Display) -> Result<(), StatusCode> {
        let path = self.root.join("transfer-trace");
        if path.is_file() {
            let mut log = fs::OpenOptions::new().append(true).open(path).map_err(io)?;
            writeln!(log, "{request}").map_err(io)?;
        }
        Ok(())
    }

    async fn hold_response(&self, marker: &str) {
        let gate = self.root.join(marker);
        while gate.is_file() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[cfg(feature = "ui-smoke")]
    fn trace_listing(&self, request: &str) -> Result<(), StatusCode> {
        let path = self.root.join("listing-trace");
        if path.is_file() {
            let mut trace = fs::OpenOptions::new().append(true).open(path).map_err(io)?;
            writeln!(trace, "{request}").map_err(io)?;
        }
        Ok(())
    }

    fn path(&self, path: &str) -> PathBuf {
        assert!(!path.contains("..") && !path.contains(':'));
        self.root.join(path.trim_start_matches('/'))
    }
    fn add(&mut self, id: u32, h: Opened) -> Handle {
        self.next += 1;
        let handle = self.next.to_string();
        self.handles.insert(handle.clone(), h);
        Handle { id, handle }
    }
    fn file(&mut self, h: &str) -> Result<&mut fs::File, StatusCode> {
        match self.handles.get_mut(h) {
            Some(Opened::File(f)) => Ok(f),
            _ => Err(StatusCode::Failure),
        }
    }

    async fn remove_entry(
        &self,
        id: u32,
        path: String,
        directory: bool,
    ) -> Result<Status, StatusCode> {
        if fs::read_to_string(self.root.join("refuse-delete-path"))
            .is_ok_and(|blocked| blocked == path)
        {
            return Err(StatusCode::PermissionDenied);
        }
        let trace = self.root.join("delete-trace.jsonl");
        if trace.is_file() {
            let mut log = fs::OpenOptions::new()
                .append(true)
                .open(trace)
                .map_err(io)?;
            let opcode = if directory { "RMDIR" } else { "REMOVE" };
            writeln!(log, "{}", json!([opcode, path])).map_err(io)?;
        }
        let path = self.path(&path);
        let kind = fs::symlink_metadata(&path).map_err(io)?.file_type();
        if directory {
            if !kind.is_dir() {
                return Err(StatusCode::Failure);
            }
            fs::remove_dir(path).map_err(io)?;
        } else if std::os::windows::fs::FileTypeExt::is_symlink_dir(&kind) {
            // Windows unlinks a directory junction using RemoveDirectory. Its
            // SFTP representation is a symlink and requires SSH_FXP_REMOVE.
            fs::remove_dir(path).map_err(io)?;
        } else {
            fs::remove_file(path).map_err(io)?;
        }
        self.hold_response("hold-delete-reply").await;
        if self.root.join("disconnect-after-delete").is_file() {
            let _ = self
                .ssh
                .disconnect(
                    russh::Disconnect::ByApplication,
                    "fixture lost delete acknowledgement".into(),
                    "en".into(),
                )
                .await;
            return Err(StatusCode::ConnectionLost);
        }
        Ok(status(id))
    }
}
impl russh_sftp::server::Handler for Files {
    type Error = StatusCode;
    async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, StatusCode> {
        let mut version = Version::new();
        if let Ok(value) = fs::read_to_string(self.root.join("posix-rename-version")) {
            version
                .extensions
                .insert("posix-rename@openssh.com".into(), value);
            self.posix = Some(text_save::PosixMetadata::new(self.posix_entries.clone()));
        }
        if self.root.join("small-reads").exists() {
            version
                .extensions
                .insert("limits@openssh.com".into(), "1".into());
        }
        if self.root.join("fixture-copy-data").is_file() {
            version.extensions.insert("copy-data".into(), "1".into());
        }
        if let Ok(request) = fs::read_to_string(self.root.join("fixture-permissions")) {
            self.posix
                .get_or_insert_with(|| text_save::PosixMetadata::new(self.posix_entries.clone()));
            if request == "lsetstat" {
                version
                    .extensions
                    .insert("lsetstat@openssh.com".into(), "1".into());
            }
        }
        if self.root.join("fixture-space.json").is_file() {
            version
                .extensions
                .insert("statvfs@openssh.com".into(), "2".into());
        }
        Ok(version)
    }
    async fn extended(
        &mut self,
        id: u32,
        request: String,
        data: Vec<u8>,
    ) -> Result<Packet, StatusCode> {
        if request == "posix-rename@openssh.com" && self.posix.is_some() {
            return self.posix_rename(id, data);
        }
        if request == "lsetstat@openssh.com" {
            let mut payload = data.into();
            let (path, attrs): (String, FileAttributes) =
                russh_sftp::de::from_bytes(&mut payload).map_err(|_| StatusCode::BadMessage)?;
            if !payload.is_empty() {
                return Err(StatusCode::BadMessage);
            }
            self.set_permissions("LSETSTAT", path, &attrs)?;
            return Ok(Packet::Status(status(id)));
        }
        if request == "statvfs@openssh.com" {
            return self.fixture_statvfs(id, data);
        }
        if request == "copy-data" && self.root.join("fixture-copy-data").is_file() {
            self.trace_transfer("COPY-DATA")?;
            if self.root.join("reject-copy-data").is_file() {
                return Err(StatusCode::OpUnsupported);
            }
            let mut payload = data.into();
            let (from, offset, length, to, target): (String, u64, u64, String, u64) =
                russh_sftp::de::from_bytes(&mut payload).map_err(|_| StatusCode::BadMessage)?;
            if !payload.is_empty() || from == to || length == 0 || length > 4 * 1024 * 1024 {
                return Err(StatusCode::BadMessage);
            }
            let mut bytes = Vec::new();
            let source = self.file(&from)?;
            source.seek(SeekFrom::Start(offset)).map_err(io)?;
            source.take(length).read_to_end(&mut bytes).map_err(io)?;
            if bytes.len() as u64 != length {
                return Err(StatusCode::Failure);
            }
            if self.root.join("short-copy-data").is_file() {
                bytes.pop();
            }
            let output = self.file(&to)?;
            output.seek(SeekFrom::Start(target)).map_err(io)?;
            output.write_all(&bytes).map_err(io)?;
            return Ok(Packet::Status(status(id)));
        }
        if request != "limits@openssh.com" || !self.root.join("small-reads").exists() {
            return Err(StatusCode::OpUnsupported);
        }
        let data = [65536u64, 512, 32768, 128]
            .into_iter()
            .flat_map(u64::to_be_bytes)
            .collect();
        Ok(Packet::ExtendedReply(ExtendedReply { id, data }))
    }
    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }
    async fn open(
        &mut self,
        id: u32,
        path: String,
        flags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        self.trace_transfer(format_args!("OPEN {path}"))?;
        if fs::read_to_string(self.root.join("race-upload-path"))
            .ok()
            .as_deref()
            == Some(&path)
        {
            fs::File::create_new(self.path(&path))
                .and_then(|mut f| f.write_all(b"racing entry"))
                .map_err(io)?;
        }
        let file = fs::OpenOptions::from(flags)
            .open(self.path(&path))
            .map_err(io)?;
        let handle = self.add(id, Opened::File(file));
        if let Some(posix) = &mut self.posix {
            posix.opened(&handle.handle, path, flags, attrs);
        }
        Ok(handle)
    }
    async fn close(&mut self, id: u32, h: String) -> Result<Status, StatusCode> {
        if self.root.join("trace-directory-close").exists()
            && matches!(self.handles.get(&h), Some(Opened::Dir(_)))
        {
            self.trace_listing("CLOSE DIR")?;
        }
        if matches!(self.handles.get(&h), Some(Opened::File(_))) {
            self.trace_transfer("CLOSE")?;
            self.hold_response("hold-transfer-close").await;
        }
        self.handles.remove(&h).ok_or(StatusCode::Failure)?;
        if let Some(posix) = &mut self.posix {
            posix.closed(&h);
        }
        Ok(status(id))
    }
    async fn read(
        &mut self,
        id: u32,
        h: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        self.trace_transfer(format_args!("READ {offset}"))?;
        if offset > 0 {
            self.hold_response("hold-transfer-read").await;
            if self.root.join("disconnect-tree-read").is_file() {
                let _ = self
                    .ssh
                    .disconnect(
                        russh::Disconnect::ByApplication,
                        "fixture closed SFTP".into(),
                        "en".into(),
                    )
                    .await;
                return Err(StatusCode::ConnectionLost);
            }
            if self.root.join("tree-early-eof").is_file() {
                return Err(StatusCode::Eof);
            }
        }
        if self.root.join("small-reads").exists() && len > 512 {
            return Err(StatusCode::Failure);
        }
        fs::write(self.root.join("read-entered"), []).map_err(io)?;
        while offset > 0 && self.root.join("hold-next-chunk").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Keep the SFTP channel busy long enough to verify independent terminal input.
        tokio::time::sleep(Duration::from_millis(20)).await;
        if self.root.join("pause-reads").exists() {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let oversized = self.root.join("oversized-reads").exists();
        let file = self.file(&h)?;
        file.seek(SeekFrom::Start(offset)).map_err(io)?;
        let mut data = vec![0; len as usize + usize::from(oversized)];
        let n = file.read(&mut data).map_err(io)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        data.truncate(n);
        Ok(Data { id, data })
    }
    async fn write(
        &mut self,
        id: u32,
        h: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusCode> {
        self.trace_transfer(format_args!("WRITE {offset}"))?;
        if offset > 0 {
            self.hold_response("hold-transfer-write").await;
        }
        if offset > 0 && self.root.join("disconnect-next-write").exists() {
            let _ = self
                .ssh
                .disconnect(
                    russh::Disconnect::ByApplication,
                    "test connection loss".into(),
                    "en".into(),
                )
                .await;
            return Err(StatusCode::ConnectionLost);
        }
        if offset > 0 && self.root.join("fail-next-write").exists() {
            return Err(StatusCode::Failure);
        }
        let f = self.file(&h)?;
        f.seek(SeekFrom::Start(offset)).map_err(io)?;
        f.write_all(&data).map_err(io)?;
        Ok(status(id))
    }
    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.trace_properties("STAT", &path)?;
        Ok(Attrs {
            id,
            attrs: (&fs::metadata(self.path(&path)).map_err(io)?).into(),
        })
    }
    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        self.trace_properties("LSTAT", &path)?;
        if self.root.join("deny-rename-check").exists() {
            return Err(StatusCode::PermissionDenied);
        }
        if self.root.join("disconnect-on-lstat").exists() {
            let _ = self
                .ssh
                .disconnect(
                    russh::Disconnect::ByApplication,
                    "fixture interrupted LSTAT".into(),
                    "en".into(),
                )
                .await;
            return Err(StatusCode::ConnectionLost);
        }
        let mut attrs = entry_attrs(&self.path(&path))?;
        if self.root.join("utility-probe-response").is_file()
            && self.utility_fixture_link(&path).is_some()
        {
            attrs.permissions = Some(0o120777);
        }
        if let Some(posix) = &self.posix {
            posix.stat(&path, &mut attrs);
        }
        if self.root.join("tree-unknown-size").is_file() {
            attrs.size = None;
        }
        if self.root.join("tree-unknown-mtime").is_file() {
            attrs.atime = None;
            attrs.mtime = None;
        }
        if path == "/tree/socket" {
            attrs.permissions = Some(0o140600);
        }
        if self.root.join("omit-lstat-permissions").is_file() {
            attrs.permissions = None;
        }
        if let Ok(name) = fs::read_to_string(self.root.join("properties-omit-attrs"))
            && path.rsplit('/').next() == Some(name.as_str())
        {
            attrs = FileAttributes::empty();
        }
        Ok(Attrs { id, attrs })
    }
    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        if self.root.join("utility-probe-response").is_file()
            && let Some(target) = self.utility_fixture_link(&path)
        {
            return Ok(Name {
                id,
                files: vec![russh_sftp::protocol::File::dummy(target)],
            });
        }
        self.properties_readlink(id, &path)
    }
    async fn symlink(
        &mut self,
        id: u32,
        target: String,
        link: String,
    ) -> Result<Status, StatusCode> {
        if !self.root.join("utility-probe-response").is_file() {
            return Err(StatusCode::OpUnsupported);
        }
        let path = self.path(&link);
        if self.root.join("race-tool-launcher").is_file() && link.ends_with("/bin/rg") {
            fs::File::create_new(&path)
                .and_then(|mut file| file.write_all(b"my racing command"))
                .map_err(io)?;
        }
        fs::File::create_new(path)
            .and_then(|mut file| writeln!(file, "fixture-tool-symlink\n{target}"))
            .map_err(io)?;
        Ok(status(id))
    }
    async fn fstat(&mut self, id: u32, h: String) -> Result<Attrs, StatusCode> {
        if self.root.join("no-fstat").exists() {
            return Err(StatusCode::OpUnsupported);
        }
        let mut attrs = FileAttributes::from(&self.file(&h)?.metadata().map_err(io)?);
        if let Some(posix) = &self.posix {
            posix.fstat(&h, &mut attrs)?;
        }
        if self.root.join("tree-unknown-size").is_file() {
            attrs.size = None;
            attrs.permissions = Some(0o600);
        }
        if self.root.join("fstat-nonregular").is_file() {
            attrs.permissions = Some(0o140600);
        }
        Ok(Attrs { id, attrs })
    }
    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        if self.root.join("utility-probe-response").is_file()
            && let Some(mode) = attrs.permissions
            && let Some(posix) = &mut self.posix
        {
            posix.set_permissions(path, mode);
            return Ok(status(id));
        }
        if attrs.permissions.is_some() && self.root.join("fixture-permissions").is_file() {
            self.set_permissions("SETSTAT", path, &attrs)?;
            return Ok(status(id));
        }
        self.trace_properties("SETSTAT", &path)?;
        if let Some(size) = attrs.size {
            fs::OpenOptions::new()
                .write(true)
                .open(self.path(&path))
                .map_err(io)?
                .set_len(size)
                .map_err(io)?;
        }
        Ok(status(id))
    }
    async fn fsetstat(
        &mut self,
        id: u32,
        h: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        self.trace_properties("FSETSTAT", &h)?;
        if let Some(posix) = &mut self.posix {
            if self.root.join("deny-editor-fsetstat").is_file() {
                return Err(StatusCode::PermissionDenied);
            }
            posix.fsetstat(&h, &attrs)?;
        }
        if let Some(size) = attrs.size {
            self.file(&h)?.set_len(size).map_err(io)?;
        }
        if let (Some(atime), Some(mtime)) = (attrs.atime, attrs.mtime) {
            if self.root.join("deny-upload-times").is_file() {
                return Err(StatusCode::PermissionDenied);
            }
            let times = fs::FileTimes::new()
                .set_accessed(std::time::UNIX_EPOCH + Duration::from_secs(u64::from(atime)))
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(u64::from(mtime)));
            self.file(&h)?.set_times(times).map_err(io)?;
        }
        Ok(status(id))
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        #[cfg(feature = "ui-smoke")]
        {
            self.trace_listing(&format!("OPENDIR {path}"))?;
            if fs::read_to_string(self.root.join("hold-opendir"))
                .ok()
                .as_deref()
                == Some(&path)
            {
                fs::write(self.root.join("opendir-paused"), &path).map_err(io)?;
                self.hold_response("hold-opendir").await;
            }
        }
        let omit_mode = fs::read_to_string(self.root.join("omit-listed-mode")).ok();
        let omit_size = fs::read_to_string(self.root.join("omit-listed-size")).ok();
        let omit_mtime = fs::read_to_string(self.root.join("omit-listed-mtime")).ok();
        if path == "/tree"
            && let Ok(names) = fs::read_to_string(self.root.join("tree-test-names"))
        {
            return Ok(self.add(
                id,
                Opened::Dir(
                    names
                        .lines()
                        .map(|s| russh_sftp::protocol::File::new(s, FileAttributes::empty()))
                        .collect(),
                ),
            ));
        }
        let entries = fs::read_dir(self.path(&path))
            .map_err(io)?
            .map(|e| {
                let e = e.map_err(io)?;
                let name = e.file_name();
                let name = name.to_string_lossy();
                let mut attrs = entry_attrs(&e.path())?;
                if Some(name.as_ref()) == omit_size.as_deref()
                    || self.root.join("tree-unknown-size").is_file()
                {
                    attrs.size = None;
                }
                if Some(name.as_ref()) == omit_mtime.as_deref()
                    || self.root.join("tree-unknown-mtime").is_file()
                {
                    attrs.atime = None;
                    attrs.mtime = None;
                }
                if Some(name.as_ref()) == omit_mode.as_deref() || name == "socket" {
                    attrs.permissions = None;
                }
                Ok(russh_sftp::protocol::File::new(name, attrs))
            })
            .collect::<Result<_, StatusCode>>()?;
        Ok(self.add(id, Opened::Dir(entries)))
    }
    async fn readdir(&mut self, id: u32, h: String) -> Result<Name, StatusCode> {
        if self.root.join("empty-tree-page").is_file() {
            return Ok(Name {
                id,
                files: Vec::new(),
            });
        }
        #[cfg(feature = "ui-smoke")]
        self.trace_listing("READDIR")?;
        #[cfg(feature = "ui-smoke")]
        if self.root.join("slow-search-page").is_file() {
            tokio::time::sleep(Duration::from_millis(35)).await;
        }
        match self.handles.get_mut(&h) {
            Some(Opened::Dir(entries)) if !entries.is_empty() => Ok(Name {
                id,
                files: entries.drain(..1).collect(),
            }),
            Some(Opened::Dir(_)) => Err(StatusCode::Eof),
            _ => Err(StatusCode::Failure),
        }
    }
    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _: FileAttributes,
    ) -> Result<Status, StatusCode> {
        #[cfg(feature = "ui-smoke")]
        if self.root.join("mkdir-trace").exists() {
            let mut trace = fs::OpenOptions::new()
                .append(true)
                .open(self.root.join("mkdir-trace"))
                .map_err(io)?;
            writeln!(trace, "{path}").map_err(io)?;
        }
        fs::create_dir(self.path(&path)).map_err(io)?;
        Ok(status(id))
    }
    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.remove_entry(id, path, true).await
    }
    async fn remove(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.remove_entry(id, path, false).await
    }
    async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, StatusCode> {
        let trace = self.root.join("rename-trace.jsonl");
        if trace.is_file() {
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(trace)
                .map_err(io)?;
            writeln!(file, "{}", json!([from, to])).map_err(io)?;
        }
        let destination = self.path(&to);
        if self.root.join("race-next-rename").exists() {
            fs::File::create_new(&destination)
                .map_err(io)?
                .write_all(b"racing entry")
                .map_err(io)?;
            fs::remove_file(self.root.join("race-next-rename")).map_err(io)?;
        }
        // MoveFileW enforces one no-replace operation, including for directories
        // and reparse points. std::fs::rename would replace a racing entry.
        use windows::{Win32::Storage::FileSystem::MoveFileW, core::HSTRING};
        unsafe {
            MoveFileW(
                &HSTRING::from(self.path(&from).as_os_str()),
                &HSTRING::from(destination.as_os_str()),
            )
        }
        .map_err(|_| StatusCode::Failure)?;
        if let Some(posix) = &mut self.posix {
            posix.renamed(&from, &to);
        }
        self.hold_response("hold-rename-reply").await;
        if self.root.join("disconnect-after-rename").exists() {
            let _ = self
                .ssh
                .disconnect(
                    russh::Disconnect::ByApplication,
                    "fixture lost RENAME reply".into(),
                    "en".into(),
                )
                .await;
            return Err(StatusCode::ConnectionLost);
        }
        Ok(status(id))
    }
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        if let Some((_, resolved)) = self.fixture_link(&path) {
            return Ok(Name {
                id,
                files: vec![russh_sftp::protocol::File::dummy(resolved)],
            });
        }
        if path == "/tree/data" && self.root.join("tree-redirect").is_file() {
            return Ok(Name {
                id,
                files: vec![russh_sftp::protocol::File::dummy("/elsewhere")],
            });
        }
        Ok(Name {
            id,
            files: vec![russh_sftp::protocol::File::dummy(if path == "." {
                "/".into()
            } else if path == "alias" && self.root.join("canonical-parent").is_dir() {
                "/canonical-parent".into()
            } else {
                path
            })],
        })
    }
}

type FixtureServer = (
    Source,
    tempfile::TempDir,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
);
async fn fixture_server(reject: bool) -> FixtureServer {
    fixture_connections(reject, 1).await
}
async fn fixture_connections(reject: bool, connections: usize) -> FixtureServer {
    fixture_counting_accepts(reject, connections, Arc::default()).await
}
async fn fixture_counting_accepts(
    reject: bool,
    connections: usize,
    accepted: Arc<AtomicUsize>,
) -> FixtureServer {
    fixture_with_wire(reject, connections, accepted, None).await
}

async fn fixture_with_wire(
    reject: bool,
    connections: usize,
    accepted: Arc<AtomicUsize>,
    wire: Option<tokio::sync::mpsc::UnboundedSender<in_band_resize::Wire>>,
) -> FixtureServer {
    use russh::keys::{Algorithm, HashAlg, PrivateKey};
    let root = tempfile::tempdir().unwrap();
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        ..Default::default()
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let opens = Arc::new(AtomicUsize::new(0));
    let root_path = root.path().to_owned();
    let opens_for_server = opens.clone();
    let server = tokio::spawn(async move {
        let mut peers = tokio::task::JoinSet::new();
        let posix_entries = text_save::PosixEntries::default();
        for _ in 0..connections {
            let (stream, _) = listener.accept().await.unwrap();
            accepted.fetch_add(1, Ordering::Relaxed);
            let peer = Peer {
                wire: wire.clone(),
                channels: HashMap::new(),
                forwarded: HashSet::new(),
                #[cfg(feature = "ui-smoke")]
                withheld_session_open: None,
                root: root_path.clone(),
                opens: opens_for_server.clone(),
                posix_entries: posix_entries.clone(),
                reject,
            };
            let config = config.clone();
            peers.spawn(async move {
                let _ = server::run_stream(config, stream, peer)
                    .await
                    .unwrap()
                    .await;
            });
        }
        while let Some(done) = peers.join_next().await {
            done.unwrap();
        }
    });
    let source = Source::Ssh(SshOptions {
        scrollback_lines: Default::default(),
        color_scheme: Default::default(),
        connect_timeout_seconds: None,
        keepalive: Default::default(),
        font_size: None,
        tab_color: Default::default(),
        terminal_type: Default::default(),
        host: "127.0.0.1".into(),
        user: "test".into(),
        port,
        authentication: Authentication::None,
        agent_pipe: None,
        host_key: Some(pin),
        host_key_alias: None,
        proxy_jump: Vec::new(),
        local_forwards: Vec::new(),
        durable: false,
        dotfiles: None,
        tool_bundle: None,
        group_prompt: None,
        player: Default::default(),
    });
    (source, root, opens, server)
}

async fn fixture(
    reject: bool,
) -> (
    Session,
    Source,
    tempfile::TempDir,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let (source, root, opens, server) = fixture_server(reject).await;
    let session = Session::start(source.clone());
    tokio::time::timeout(Duration::from_secs(5), async {
        while session.frames.load().connection_generation == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("SSH fixture did not connect");
    (session, source, root, opens, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ssh_keepalives_detect_a_silent_tcp_peer_at_the_profile_threshold() {
    use tokio::{
        net::{TcpListener, TcpStream},
        sync::watch,
    };
    use w11term::events::SshConnectionPhase;
    let (Source::Ssh(options), _root, _opens, server) = fixture_connections(false, 3).await else {
        unreachable!()
    };
    let (stall, gate) = watch::channel(false);
    let mut relays = tokio::task::JoinSet::new();
    let mut panes = Vec::new();
    for (interval, unanswered) in [(1, 1), (1, 4), (0, 1)] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let mut profile = options.clone();
        profile.port = listener.local_addr().unwrap().port();
        profile.keepalive.interval_seconds = interval;
        profile.keepalive.max_unanswered = unanswered.try_into().unwrap();
        let server_port = options.port;
        let mut gate = gate.clone();
        relays.spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut peer = TcpStream::connect(("127.0.0.1", server_port))
                .await
                .unwrap();
            tokio::select! {
                result = tokio::io::copy_bidirectional(&mut client, &mut peer) => {
                    panic!("fixture link closed before stalling: {result:?}");
                }
                result = gate.changed() => result.unwrap(),
            }
            // Keep both sockets open, including the listening socket, while no
            // more bytes flow. A clean close would exercise a different path.
            std::future::pending::<()>().await;
        });
        panes.push(Session::start(Source::Ssh(profile)));
    }
    let [fast, slow, disabled] = panes.as_slice() else {
        unreachable!()
    };
    let reconnecting = |pane: &Session| {
        matches!(
            pane.events.ssh_connection_phase(),
            Some(SshConnectionPhase::Reconnecting { .. })
        )
    };
    tokio::time::timeout(Duration::from_secs(8), async {
        while [fast, slow, disabled]
            .iter()
            .any(|pane| pane.ssh_connection_generation().is_none())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("all three SSH panes should authenticate");
    stall.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !reconnecting(fast) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("one unanswered keepalive should trigger a reconnect");
    assert!(
        slow.ssh_connection_generation().is_some(),
        "a larger limit should keep its original link longer"
    );
    tokio::time::timeout(Duration::from_secs(7), async {
        while !reconnecting(slow) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the larger unanswered limit should eventually trigger a reconnect");
    assert!(
        disabled.ssh_connection_generation().is_some(),
        "disabled keepalives should leave the silent connection open"
    );
    relays.abort_all();
    server.abort();
}

#[cfg(feature = "ui-smoke")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_window_expires_during_shell_setup_and_preserves_manual_restart() {
    use in_band_resize::Wire;
    use w11term::events::SshConnectionPhase;

    let (wire, mut messages) = tokio::sync::mpsc::unbounded_channel();
    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(mut options), root, _, server) =
        fixture_with_wire(false, 3, accepted.clone(), Some(wire)).await
    else {
        unreachable!()
    };
    options.player.reconnect_window_seconds = 3.try_into().unwrap();
    let pane = Session::start(Source::Ssh(options));
    let Wire::Shell(_, first) = tokio::time::timeout(Duration::from_secs(5), messages.recv())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("first connection did not reach the shell");
    };
    let first_generation = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(generation) = pane.ssh_connection_generation() {
                break generation;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let gate = root.path().join("withhold-shell-reply");
    fs::write(&gate, []).unwrap();
    first
        .disconnect(
            russh::Disconnect::ByApplication,
            "retry fixture disconnect".into(),
            "en".into(),
        )
        .await
        .unwrap();

    let restart = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(token) = pane.events.restart_token() {
                break token;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the configured retry deadline should expire during remote shell setup");
    assert!(root.path().join("shell-reply-withheld").is_file());
    assert_eq!(accepted.load(Ordering::Relaxed), 2);
    assert!(matches!(
        pane.events.ssh_connection_phase(),
        Some(SshConnectionPhase::Failed(reason)) if reason.contains("after 3 seconds")
    ));
    let retry = pane
        .events
        .since(0)
        .events
        .into_iter()
        .find(|event| event.kind == "reconnecting")
        .unwrap();
    assert_eq!(retry.data["delay_seconds"], 1);
    assert_eq!(retry.data["retry_remaining_seconds"], 3);
    assert_eq!(retry.data["will_retry"], true);

    fs::remove_file(gate).unwrap();
    pane.restart_exited(restart).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while pane
            .ssh_connection_generation()
            .is_none_or(|generation| generation <= first_generation)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("manual restart should have a fresh retry window");
    assert_eq!(accepted.load(Ordering::Relaxed), 3);
    server.abort();
}

#[tokio::test]
async fn reconnect_window_can_expire_before_the_next_login() {
    reconnect_window_with_closing_peer(false).await;
}

#[tokio::test]
async fn retry_now_keeps_the_original_reconnect_window() {
    reconnect_window_with_closing_peer(true).await;
}

async fn reconnect_window_with_closing_peer(retry_early: bool) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    let server = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::Relaxed);
            drop(socket);
        }
    });
    let options = serde_json::from_value(json!({
        "host": "127.0.0.1",
        "user": "fixture",
        "port": port,
        "auth": "none",
        "player": {"reconnect_window_seconds": 1},
    }))
    .unwrap();
    let pane = Session::start(Source::Ssh(options));
    if retry_early {
        let token = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(token) = pane.events.pending_ssh_retry() {
                    break token;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the remaining retry window should allow a manual attempt");
        assert!(pane.events.retry_ssh_now(token));
    }
    tokio::time::timeout(Duration::from_secs(6), async {
        while pane.events.restart_token().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let journal = pane.events.since(0);
    let mut waits = journal
        .events
        .iter()
        .filter(|event| event.kind == "reconnecting");
    let wait = waits.next().unwrap();
    assert_eq!(wait.data["delay_seconds"], 1);
    assert_eq!(wait.data["retry_remaining_seconds"], 1);
    assert_eq!(wait.data["will_retry"], false);
    if retry_early {
        let second = waits.next().unwrap();
        assert_eq!(second.data["attempt"], 2);
        assert_eq!(second.data["retry_remaining_seconds"], 1);
        assert_eq!(second.data["will_retry"], false);
        assert!(
            journal
                .events
                .iter()
                .any(|event| { event.kind == "ssh-retry-wait" && event.data["requested"] == true })
        );
    }
    assert!(waits.next().is_none());
    assert!(pane.events.pending_ssh_retry().is_none());
    assert_eq!(
        accepted.load(Ordering::Relaxed),
        1 + usize::from(retry_early)
    );
    server.abort();
}

fn directory_junction(root: &std::path::Path, name: &str, target: &str) {
    // A directory junction is a link on NTFS and needs no symbolic-link privilege.
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/j", name, target])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::symlink_metadata(root.join(name))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

fn mutation_trace(root: &std::path::Path, log: &str) -> Vec<[String; 2]> {
    fs::read_to_string(root.join(log))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn guarded_job(
    pane: &Session,
    generation: Option<u64>,
    operation: w11term::sftp::Operation,
) -> Result<Value, String> {
    let (reply, response) = tokio::sync::oneshot::channel();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    pane.input
        .send(Command::Sftp(w11term::sftp::Job {
            expected_generation: generation,
            operation,
            deadline,
            reply,
            progress: None,
            cancel: None,
        }))
        .unwrap();
    tokio::time::timeout_at(deadline + Duration::from_secs(1), response)
        .await
        .unwrap()
        .unwrap()
}

#[path = "sftp/cancellation.rs"]
mod cancellation;
#[path = "sftp/checksum.rs"]
mod checksum;
#[path = "sftp/dotfiles.rs"]
mod dotfiles;
#[path = "sftp/download_folder.rs"]
mod download_folder;
#[path = "sftp/filesystem_space.rs"]
mod filesystem_space;
#[path = "sftp/find.rs"]
mod find;
#[path = "sftp/folder_contents.rs"]
mod folder_contents;
#[path = "sftp/in_band_resize.rs"]
mod in_band_resize;
#[cfg(feature = "ui-smoke")]
#[path = "sftp/keypad.rs"]
mod keypad;
#[path = "sftp/kitty_text_width.rs"]
mod kitty_text_width;
#[path = "sftp/local_forward.rs"]
mod local_forward;
#[cfg(feature = "ui-smoke")]
#[path = "sftp/native_gui.rs"]
mod native_gui;
#[path = "sftp/properties.rs"]
mod properties;
#[path = "sftp/remote_copy.rs"]
mod remote_copy;
#[path = "sftp/remove.rs"]
mod remove;
#[path = "sftp/rename.rs"]
mod rename;
#[cfg(feature = "ui-smoke")]
#[path = "sftp/tab_windows.rs"]
mod tab_windows;
#[path = "sftp/text_preview.rs"]
mod text_preview;
#[path = "sftp/text_save.rs"]
mod text_save;
#[path = "sftp/upload_folder.rs"]
mod upload_folder;
#[path = "sftp/utilities.rs"]
mod utilities;
#[path = "sftp/xterm_other_keys.rs"]
mod xterm_other_keys;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_creation_checks_the_ssh_generation_before_opening_sftp() {
    let (pane, _, remote, opens, server) = fixture(false).await;
    let generation = pane.frames.load().connection_generation;
    let baseline = opens.load(Ordering::Relaxed);
    for expected in [generation + 1, generation] {
        let (reply, result) = tokio::sync::oneshot::channel();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        pane.input
            .send(Command::Sftp(w11term::sftp::Job {
                operation: w11term::sftp::Operation::Mkdir {
                    path: "/approved".into(),
                    attrs: Default::default(),
                },
                expected_generation: Some(expected),
                deadline,
                reply,
                progress: None,
                cancel: None,
            }))
            .unwrap();
        let result = tokio::time::timeout_at(deadline, result)
            .await
            .unwrap()
            .unwrap();
        if expected != generation {
            assert!(result.unwrap_err().contains("operation was not started"));
            assert!(!remote.path().join("approved").exists());
            assert_eq!(opens.load(Ordering::Relaxed), baseline);
        } else {
            result.unwrap();
            assert!(remote.path().join("approved").is_dir());
            assert_eq!(opens.load(Ordering::Relaxed), baseline + 1);
        }
    }
    drop(pane);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_workspace_exec_reply_does_not_dispatch_command_again() {
    let (Source::Ssh(mut ssh), remote, opens, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    ssh.player.reconnect = true;
    let trace = remote.path().join("workspace-exec.bin");
    fs::write(&trace, []).unwrap();
    fs::write(remote.path().join("drop-before-exec-reply"), []).unwrap();
    let command = "make deploy";
    let startup = w11term::workspace::SshStartup::new(&ssh, String::new(), command.into()).unwrap();
    let session = Session::start_workspace_ssh(ssh, Default::default(), false, startup);
    let exit = registry(&session)
        .execute(
            Request::WaitPaneExit {
                pane: 1,
                after_exit_token: 0,
                timeout_seconds: 10,
            },
            Arc::new(|_, _, _| unreachable!()),
        )
        .await
        .unwrap();
    assert_eq!(exit["pane"], 1);
    assert_eq!(exit["exit_token"], 1);
    assert_eq!(exit["restart_token"], Value::Null);
    assert!(exit["ssh_connection"].is_object(), "{exit}");
    let frame = session.frames.load();
    assert!(
        frame.status.contains("Workspace command interrupted"),
        "exit wait returned before the final screen: {:?}",
        frame.status
    );
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    assert_eq!(fs::read(trace).unwrap(), command.as_bytes());
    drop(session);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directory_only_workspace_reconnects_to_an_interactive_shell() {
    use w11term::folder_grants::FolderScope;
    use w11term::sftp::{Job, Operation};
    let (Source::Ssh(mut ssh), remote, opens, server) = fixture_connections(false, 2).await else {
        unreachable!()
    };
    ssh.player.reconnect = true;
    let trace = remote.path().join("workspace-exec.bin");
    fs::write(&trace, []).unwrap();
    fs::write(remote.path().join("drop-before-exec-reply"), []).unwrap();
    let startup = w11term::workspace::SshStartup::new(&ssh, "/work".into(), String::new()).unwrap();
    let local = tempfile::tempdir().unwrap();
    let catalog = local.path().join("profiles.json");
    fs::write(
        &catalog,
        serde_json::to_vec(&json!({"profiles":{"guest":ssh}})).unwrap(),
    )
    .unwrap();
    let expected_exec = startup.exec_request();
    let session = Session::start_workspace_ssh(ssh, Default::default(), false, startup);
    tokio::time::timeout(Duration::from_secs(10), async {
        while session.frames.load().connection_generation != 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("directory-only shell should reconnect after the first exec loses its reply");
    assert_eq!(opens.load(Ordering::Relaxed), 2);
    assert_eq!(fs::read_to_string(trace).unwrap(), expected_exec.repeat(2));
    // The first connection failed before publishing a usable shell. Folder
    // actions and SFTP must still identify the same second SSH generation.
    let shares = session.profile_folders(FolderScope::new(
        catalog,
        "guest".into(),
        local.path().into(),
    ));
    let authority = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(auth) = shares.authority() {
                break auth;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(authority.connection_generation(), 2);
    let mount = remote.path().join("mounted");
    fs::create_dir(&mount).unwrap();
    fs::write(mount.join("from-windows"), []).unwrap();
    for (expected_generation, path, error) in [
        (Some(1), "/mounted", Some("SSH connection changed")),
        (None, "/mounted", Some("Missing SSH connection")),
        (Some(2), ".", Some("different path")),
        (Some(2), "/mounted", None),
    ] {
        let (reply, response) = tokio::sync::oneshot::channel();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        session
            .input
            .send(Command::Sftp(Job {
                expected_generation,
                operation: Operation::ListMountedFolder { path: path.into() },
                deadline,
                reply,
                progress: None,
                cancel: None,
            }))
            .unwrap();
        let result = tokio::time::timeout_at(deadline, response)
            .await
            .unwrap()
            .unwrap();
        if let Some(message) = error {
            assert!(result.unwrap_err().contains(message));
        } else {
            assert_eq!(result.unwrap()["entries"][0]["filename"], "from-windows");
        }
    }
    drop(shares);
    drop(session);
    server.abort();
}

async fn request(op: Value) -> control::Reply {
    let request: Request =
        serde_json::from_value(json!({"verb":"sftp","pane":1,"timeout_seconds":10,"operation":op}))
            .unwrap();
    control::call(Some(std::process::id()), &request)
        .await
        .unwrap()
}
async fn ok(op: Value) -> Value {
    let r = request(op).await;
    assert!(r.ok, "{r:?}");
    r.result.unwrap()["sftp"].clone()
}
fn registry(session: &Session) -> Registry {
    let r = Registry::default();
    r.replace([Pane {
        id: 1,
        tab_id: 4,
        tab_title: None,
        pane_name: None,
        label: "test".into(),
        active: false,
        visible: false,
        read_only: false,
        capture: None,
        input: session.input.clone(),
    }]);
    r
}

const TRIAGE_RESPONSE: &str = concat!(
    "w11term-linux-triage-v1\t1000\t2000\t124\n",
    "uptime\t1000\t1010\t0\tdXB0aW1lCg==\t\n",
    "dmesg\t1000\t1010\t1\t\tT3BlcmF0aW9uIG5vdCBwZXJtaXR0ZWQ=\n",
    "vmstat\t1000\t2000\t0\tciBiIHNpIHNvIHVzIHN5IGlkIHdhIHN0CjIzIDEgMCAwIDk1IDQgMSAwIDAKMyAwIDAgOCA0MCAxMiA0NiAxIDEK\t\n",
    "mpstat\t1000\t1010\t127\t\t\n",
    "pidstat\t1000\t2000\t0\t\t\n",
    "iostat\t1000\t2000\t0\t\t\n",
    "free\t1000\t1010\t0\t\t\n",
    "sar-dev\t1000\t2000\t0\t\t\n",
    "sar-tcp\t1000\t-\t-\t\t\n",
    "top\t1000\t1010\t0\t\t\n",
    "end\n"
);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn packet_capture_verifies_review_and_sftp_bytes_before_acknowledging_cleanup() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let registry = registry(&pane);
    let callback: control::CapturePng = Arc::new(|_, _, _| unreachable!("no screenshot"));
    let options = control::PacketCaptureOptions {
        interface: "lo".into(),
        filter: "udp and port 4096".into(),
        snap_length: 192,
        seconds: 5,
        max_mib: 1,
        packet_limit: Some(1),
        rotate_files: None,
        scratch_directory: "/dev/shm".into(),
        require_tmpfs: true,
    };
    let preview = registry
        .execute(
            Request::PreviewPacketCapture {
                pane: 1,
                options: options.clone(),
            },
            callback.clone(),
        )
        .await
        .unwrap();
    let plan = &preview["plan"];
    assert_eq!(plan["scratch_filesystem"], "tmpfs");
    assert_eq!(
        plan["tcpdump_arguments"]
            .as_array()
            .unwrap()
            .last()
            .unwrap(),
        "udp and port 4096"
    );
    let generation = plan["connection_generation"]
        .as_u64()
        .unwrap()
        .try_into()
        .unwrap();
    let request = |name: &str, options, job_id, report: Option<&str>| Request::CapturePackets {
        pane: 1,
        options,
        connection_generation: generation,
        review_id: plan["review_id"].as_str().unwrap().into(),
        job_id,
        output_file: local.path().join(name),
        report_file: report.map(|name| local.path().join(name)),
    };
    let scratch = remote.path().join("dev/shm");
    let mut changed = options.clone();
    changed.seconds += 1;
    let error = registry
        .execute(request("stale.pcap", changed, None, None), callback.clone())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("Review again"));
    assert!(!scratch.exists());

    let existing = local.path().join("existing.json");
    fs::write(&existing, b"previous report").unwrap();
    for (destination, expected) in [
        ("existing.json", "destination already exists"),
        ("blocked.pcap", "separate capture and report files"),
    ] {
        let error = registry
            .execute(
                request("blocked.pcap", options.clone(), None, Some(destination)),
                callback.clone(),
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected));
        assert!(!local.path().join("blocked.pcap").exists());
        assert!(!scratch.exists());
    }
    assert_eq!(fs::read(existing).unwrap(), b"previous report");

    fs::write(remote.path().join("pcap-bad-digest"), []).unwrap();
    let error = registry
        .execute(
            request("corrupt.pcap", options.clone(), None, Some("corrupt.json")),
            callback.clone(),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("SHA-256 digest"));
    assert!(!remote.path().join("pcap-verified").exists());
    assert!(!local.path().join("corrupt.pcap").exists());
    assert!(!local.path().join("corrupt.json").exists());
    fs::remove_file(remote.path().join("pcap-bad-digest")).unwrap();

    let result = registry
        .execute(
            request(
                "reviewed capture.pcap",
                options.clone(),
                None,
                Some("capture report.json"),
            ),
            callback.clone(),
        )
        .await
        .unwrap();
    let capture = &result["capture"];
    assert_eq!(capture["bytes"], PCAP_FIXTURE.len());
    assert_eq!(capture["packets"], 1);
    assert_eq!(capture["received_by_filter"], 2);
    assert_eq!(capture["dropped_by_kernel"], 0);
    assert_eq!(capture["stop_reason"], "packet-limit");
    let report_path = local.path().join("capture report.json");
    assert_eq!(result["report_path"], json!(report_path));
    let report: Value = serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["file_name"], "reviewed capture.pcap");
    for field in [
        "bytes",
        "sha256",
        "packets",
        "stop_reason",
        "dropped_by_kernel",
        "received_by_filter",
    ] {
        assert_eq!(report[field], capture[field], "{field}");
    }
    for field in [
        "host",
        "user",
        "port",
        "host_key_fingerprint",
        "options",
        "scratch_directory",
        "scratch_filesystem",
        "effective_packet_limit",
        "tcpdump_arguments",
    ] {
        assert_eq!(report["reviewed"][field], plan[field], "{field}");
    }
    assert!(report.get("path").is_none());
    assert!(report["reviewed"].get("review_id").is_none());
    assert!(remote.path().join("pcap-verified").exists());
    assert_eq!(
        fs::read(local.path().join("reviewed capture.pcap")).unwrap(),
        PCAP_FIXTURE
    );
    let job = "0123456789abcdef0123456789abcdef";
    let started = remote.path().join("pcap-started");
    fs::remove_file(&started).unwrap();
    fs::remove_file(remote.path().join("pcap-verified")).unwrap();
    fs::write(remote.path().join("pcap-wait-for-stop"), []).unwrap();
    let stop = |job_id: &str| Request::StopPacketCapture {
        pane: 1,
        connection_generation: generation,
        job_id: job_id.into(),
    };
    let capture = registry.execute(
        request(
            "stopped.pcap",
            options,
            Some(job.into()),
            Some("raced report.json"),
        ),
        callback.clone(),
    );
    let requested = async {
        while !started.is_file() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        fs::write(
            local.path().join("raced report.json"),
            b"created during capture",
        )
        .unwrap();
        let error = registry
            .execute(stop("00000000000000000000000000000000"), callback.clone())
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("No matching running packet capture")
        );
        assert!(!remote.path().join("pcap-stopped").exists());
        registry.execute(stop(job), callback.clone()).await.unwrap()
    };
    let (capture, requested) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(capture, requested)
    })
    .await
    .unwrap();
    assert_eq!(requested["stop_requested"], true);
    let stopped = capture.unwrap();
    assert_eq!(stopped["job_id"], job);
    assert_eq!(stopped["capture"]["stop_reason"], "stopped");
    assert_eq!(stopped["capture"]["packets"], 1);
    assert!(stopped.get("report_path").is_none());
    assert!(
        stopped["report_error"]
            .as_str()
            .unwrap()
            .contains("Saving new packet capture report")
    );
    assert_eq!(
        fs::read(local.path().join("raced report.json")).unwrap(),
        b"created during capture"
    );
    assert_eq!(
        fs::read(local.path().join("stopped.pcap")).unwrap(),
        PCAP_FIXTURE
    );
    assert!(remote.path().join("pcap-verified").exists());
    assert!(registry.execute(stop(job), callback.clone()).await.is_err());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn syscall_capture_requires_review_and_stops_only_the_matching_job() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let registry = registry(&pane);
    let callback: control::CapturePng = Arc::new(|_, _, _| unreachable!("no screenshot"));
    let options = control::SyscallOptions {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), "printf '%s' '$literal'".into()],
        directory: "/home/test".into(),
        seconds: 30,
        max_mib: 1,
        string_size: 128,
        filter: control::SyscallFilter::Files,
    };
    let settings = local.path().join("syscall settings.json");
    fs::write(&settings, options.settings_json().unwrap()).unwrap();
    let options = control::SyscallOptions::load_settings(&settings).unwrap();
    let preview = registry
        .execute(
            Request::PreviewSyscallCapture {
                pane: 1,
                options: options.clone(),
            },
            callback.clone(),
        )
        .await
        .unwrap();
    let plan = &preview["plan"];
    assert_eq!(
        plan["strace_arguments"].as_array().unwrap().last().unwrap(),
        "printf '%s' '$literal'"
    );
    let generation = plan["connection_generation"]
        .as_u64()
        .unwrap()
        .try_into()
        .unwrap();
    let job = "1234567890abcdef1234567890abcdef";
    let output_file = local.path().join("raw.strace");
    let report_file = local.path().join("syscall report.json");
    let request = |options, report: &Path| Request::CaptureSyscalls {
        pane: 1,
        options,
        connection_generation: generation,
        review_id: plan["review_id"].as_str().unwrap().into(),
        job_id: job.into(),
        output_file: output_file.clone(),
        report_file: Some(report.to_owned()),
    };
    let error = registry
        .execute(request(options.clone(), &output_file), callback.clone())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("Choose separate capture and report files"));
    fs::write(&report_file, b"previous report").unwrap();
    let error = registry
        .execute(request(options.clone(), &report_file), callback.clone())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("already exists"));
    assert_eq!(fs::read(&report_file).unwrap(), b"previous report");
    fs::remove_file(&report_file).unwrap();
    let mut edited = options.clone();
    edited.arguments.push("changed".into());
    fs::write(&settings, edited.settings_json().unwrap()).unwrap();
    let changed = control::SyscallOptions::load_settings(&settings).unwrap();
    let error = registry
        .execute(request(changed, &report_file), callback.clone())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("Review again"));
    let started = remote.path().join("syscall-started");
    assert!(!started.exists());
    let stop = |job_id: &str| Request::StopSyscallCapture {
        pane: 1,
        connection_generation: generation,
        job_id: job_id.into(),
    };
    let capture = registry.execute(request(options, &report_file), callback.clone());
    let stop = async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let error = registry
            .execute(stop("00000000000000000000000000000000"), callback.clone())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("No matching running syscall capture"));
        registry.execute(stop(job), callback.clone()).await.unwrap()
    };
    let (capture, stopped) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(capture, stop)
    })
    .await
    .unwrap();
    assert_eq!(stopped["stop_requested"], true);
    let capture = capture.unwrap();
    assert_eq!(capture["capture"]["stop_reason"], "stopped");
    assert_eq!(fs::read(&output_file).unwrap(), SYSCALL_FIXTURE);
    assert_eq!(capture["report_path"], json!(report_file));
    assert!(capture.get("report_error").is_none());
    let report = w11term::ssh::verify_syscall_capture_report(&output_file, &report_file).unwrap();
    assert_eq!(report.bytes, SYSCALL_FIXTURE.len() as u64);
    assert_eq!(
        report.sha256,
        capture["capture"]["sha256"].as_str().unwrap()
    );
    assert_eq!(report.stop_reason, "stopped");
    assert_eq!(report.file_name, "raw.strace");
    assert_eq!(report.command_exit_status, None);
    assert_eq!(report.reviewed.program, "/bin/sh");
    assert_eq!(report.reviewed.directory, "/home/test");
    assert_eq!(
        serde_json::to_value(&report.reviewed.strace_arguments).unwrap(),
        plan["strace_arguments"]
    );
    assert_eq!(
        serde_json::to_value(&report.reviewed.options).unwrap(),
        plan["options"]
    );
    let saved: Value = serde_json::from_slice(&fs::read(report_file).unwrap()).unwrap();
    for field in ["host", "user", "port", "host_key_fingerprint"] {
        assert_eq!(saved["reviewed"][field], plan[field], "{field}");
    }
    for field in ["review_id", "connection_generation", "identity", "job_id"] {
        assert!(saved["reviewed"].get(field).is_none(), "{saved}");
    }
    assert!(remote.path().join("syscall-verified").exists());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cpu_profile_raw_export_keeps_valid_recording_when_folding_fails() {
    let (pane, _, remote, opens, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let profile = remote.path().join("profile-response");
    let capture: control::CapturePng = Arc::new(|_, _, _| unreachable!("no screen capture"));
    let registry = registry(&pane);
    let options = control::CpuProfileOptions {
        target_pid: 42.try_into().unwrap(),
        seconds: 2,
        frequency: 99,
        call_graph: control::CpuCallGraph::Fp,
    };
    let preview = registry
        .execute(
            Request::PreviewCpuProfile { pane: 1, options },
            capture.clone(),
        )
        .await
        .unwrap();
    let plan = &preview["plan"];
    let request = |name, perf_data_file, report_file| Request::CaptureCpuProfile {
        pane: 1,
        options,
        connection_generation: plan["connection_generation"]
            .as_u64()
            .unwrap()
            .try_into()
            .unwrap(),
        process_start_ticks: plan["process_start_ticks"].as_u64().unwrap(),
        output_file: local.path().join(name),
        perf_data_file: Some(perf_data_file),
        report_file,
    };
    let before = opens.load(Ordering::SeqCst);
    let same = registry
        .execute(
            request("run.folded", local.path().join("RUN.FOLDED"), None),
            capture.clone(),
        )
        .await
        .unwrap_err();
    assert!(format!("{same:#}").contains("different destinations"));
    assert_eq!(opens.load(Ordering::SeqCst), before);
    let same_report = registry
        .execute(
            request(
                "run.folded",
                local.path().join("run.perf.data"),
                Some(local.path().join("RUN.PERF.DATA")),
            ),
            capture.clone(),
        )
        .await
        .unwrap_err();
    assert!(format!("{same_report:#}").contains("different destinations"));
    assert_eq!(opens.load(Ordering::SeqCst), before);

    let preserved = local.path().join("preserved.perf.data");
    fs::write(
        &profile,
        "w11term-cpu-profile-v1\t800\td29ya2Vy\n\nw11term-cpu-profile-end\n",
    )
    .unwrap();
    let error = registry
        .execute(
            request("empty.folded", preserved.clone(), None),
            capture.clone(),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("Raw perf recording saved"));
    assert!(!local.path().join("empty.folded").exists());
    assert_eq!(fs::read(preserved).unwrap(), PERF_DATA_FIXTURE);

    let raw = local.path().join("run.perf.data");
    let report = local.path().join("run report.json");
    let folded = local.path().join("run.folded");
    fs::write(&profile, "w11term-cpu-profile-v1\t800\td29ya2Vy\n 42 22.01: cpu-clock:u: ab hot_loop (worker)\n\nw11term-cpu-profile-end\n").unwrap();
    let captured = registry
        .execute(
            request("run.folded", raw.clone(), Some(report.clone())),
            capture.clone(),
        )
        .await
        .unwrap();
    assert_eq!(captured["summary"]["perf_data"]["path"], json!(raw));
    assert_eq!(
        captured["summary"]["perf_data"]["bytes"],
        PERF_DATA_FIXTURE.len()
    );
    assert_eq!(fs::read(&raw).unwrap(), PERF_DATA_FIXTURE);
    assert_eq!(fs::read_to_string(&folded).unwrap(), "worker;hot_loop 1\n");
    assert_eq!(captured["report_path"], json!(report));
    let provenance = w11term::ssh::verify_cpu_profile_report(&folded, &report, Some(&raw)).unwrap();
    let provenance = serde_json::to_value(provenance).unwrap();
    assert_eq!(provenance["reviewed"]["host"], plan["host"]);
    assert_eq!(
        provenance["reviewed"]["host_key_fingerprint"],
        plan["host_key_fingerprint"]
    );
    assert_eq!(
        provenance["reviewed"]["target"]["process_name"],
        plan["process_name"]
    );
    assert_eq!(provenance["reviewed"]["raw_perf_data_requested"], true);
    let before = opens.load(Ordering::SeqCst);
    let saved_report = fs::read(&report).unwrap();
    assert!(
        registry
            .execute(
                request(
                    "new.folded",
                    local.path().join("new.perf.data"),
                    Some(report.clone())
                ),
                capture.clone(),
            )
            .await
            .is_err()
    );
    assert_eq!(fs::read(report).unwrap(), saved_report);
    assert!(
        registry
            .execute(request("new.folded", raw.clone(), None), capture)
            .await
            .is_err()
    );
    assert_eq!(opens.load(Ordering::SeqCst), before);
    assert_eq!(fs::read(raw).unwrap(), PERF_DATA_FIXTURE);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cpu_profile_reuses_the_verified_ssh_transport_and_saves_only_complete_stacks() {
    let (pane, _, remote, opens, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let destination = local.path().join("profile.folded");
    let profile_response = remote.path().join("profile-response");
    let capture: control::CapturePng = Arc::new(|_, _, _| unreachable!("no screen capture"));
    let registry = registry(&pane);
    let options = control::CpuProfileOptions {
        target_pid: 42.try_into().unwrap(),
        seconds: 2,
        frequency: 99,
        call_graph: control::CpuCallGraph::Fp,
    };
    let preview = registry
        .execute(
            Request::PreviewCpuProfile { pane: 1, options },
            capture.clone(),
        )
        .await
        .unwrap();
    let plan = &preview["plan"];
    assert_eq!(plan["host"], "127.0.0.1");
    assert_eq!(plan["process_name"], "worker");
    assert_eq!(plan["event"], "cpu-clock:u");
    let generation = plan["connection_generation"].as_u64().unwrap();
    let ticks = plan["process_start_ticks"].as_u64().unwrap();
    let request = |generation: u64, process_start_ticks| Request::CaptureCpuProfile {
        pane: 1,
        options,
        connection_generation: generation.try_into().unwrap(),
        process_start_ticks,
        output_file: destination.clone(),
        perf_data_file: None,
        report_file: None,
    };
    let prior = opens.load(Ordering::SeqCst);
    assert!(
        registry
            .execute(request(generation + 1, ticks), capture.clone())
            .await
            .is_err()
    );
    assert_eq!(opens.load(Ordering::SeqCst), prior);
    let error = registry
        .execute(request(generation, ticks + 1), capture.clone())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("target PID was reused"));
    let partial = concat!(
        "w11term-cpu-profile-v1\t800\td29ya2Vy\n",
        " 42 22.01: cpu-clock:u:\n",
        "\t   deadbeef hot_loop (/tmp/worker)\n",
        "\t   12ab34cd main (/tmp/worker)\n\n",
    );
    fs::write(&profile_response, partial).unwrap();
    assert!(
        registry
            .execute(request(generation, ticks), capture.clone())
            .await
            .is_err()
    );
    assert!(!destination.exists());
    fs::write(
        &profile_response,
        format!("{partial}\nw11term-cpu-profile-end\n"),
    )
    .unwrap();
    let result = registry
        .execute(request(generation, ticks), capture.clone())
        .await
        .unwrap();
    assert_eq!(result["profile_path"], json!(destination));
    assert_eq!(result["summary"]["samples"], 1);
    assert_eq!(
        result["summary"]["diagnostics"][0],
        "Processed 3 events and lost 1 chunks!"
    );
    let saved = fs::read_to_string(&destination).unwrap();
    assert_eq!(saved, "worker;main;hot_loop 1\n");
    let prior = opens.load(Ordering::SeqCst);
    assert!(
        registry
            .execute(request(generation, ticks), capture)
            .await
            .is_err()
    );
    assert_eq!(opens.load(Ordering::SeqCst), prior);
    assert_eq!(fs::read_to_string(&destination).unwrap(), saved);
    drop(pane);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linux_triage_saves_a_generation_bound_report_without_touching_the_shell() {
    let (pane, _, remote, opens, server) = fixture(false).await;
    let output = tempfile::tempdir().unwrap();
    let destination = output.path().join("triage.json");
    let capture: control::CapturePng = Arc::new(|_, _, _| unreachable!("triage uses no capture"));
    let registry = registry(&pane);
    let preview = registry
        .execute(
            Request::LinuxTriage {
                pane: 1,
                connection_generation: None,
                output_file: None,
            },
            capture.clone(),
        )
        .await
        .unwrap();
    assert_eq!(preview["plan"]["probes"].as_array().unwrap().len(), 10);
    assert_eq!(preview["plan"]["host"], "127.0.0.1");
    let generation = preview["plan"]["connection_generation"].as_u64().unwrap();
    assert!(!remote.path().join("triage-command").exists());
    let request = |generation| Request::LinuxTriage {
        pane: 1,
        connection_generation: std::num::NonZeroU64::new(generation),
        output_file: Some(destination.clone()),
    };
    assert!(
        registry
            .execute(request(generation + 1), capture.clone())
            .await
            .is_err()
    );
    assert!(!destination.exists());
    fs::write(remote.path().join("triage-response"), TRIAGE_RESPONSE).unwrap();
    let result = registry
        .execute(request(generation), capture.clone())
        .await
        .unwrap();
    assert_eq!(result["report_path"], json!(destination));
    let bytes = fs::read(&destination).unwrap();
    let reopened = w11term::ssh::TriageReport::load(&destination).unwrap();
    let cpu = reopened
        .findings
        .iter()
        .find(|finding| finding.label == "Peak CPU busy")
        .unwrap();
    assert_eq!(cpu.value, "52% user + system");
    assert_eq!(cpu.stdout_line, Some(3));
    assert_eq!(
        reopened.collection.samples[1].stderr.text,
        "Operation not permitted"
    );
    let bad = output.path().join("incomplete.json");
    let mut incomplete: Value = serde_json::from_slice(&bytes).unwrap();
    incomplete["collection"]["samples"]
        .as_array_mut()
        .unwrap()
        .pop();
    fs::write(&bad, serde_json::to_vec(&incomplete).unwrap()).unwrap();
    assert!(w11term::ssh::TriageReport::load(&bad).is_err());
    let report: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| { finding["label"] == "Peak CPU busy" && finding["stdout_line"] == 3 })
    );
    let samples = &report["collection"]["samples"];
    assert_eq!(report["target"], preview["plan"]);
    assert_eq!(samples[0]["stdout"]["text"], "uptime\n");
    assert_eq!(samples[1]["status"], "permission-denied");
    assert_eq!(samples[3]["status"], "missing");
    assert_eq!(samples[8]["status"], "timed-out");
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one shell and one exec channel"
    );
    assert!(
        registry
            .execute(request(generation), capture)
            .await
            .is_err()
    );
    assert_eq!(fs::read(&destination).unwrap(), bytes);
    assert_eq!(opens.load(Ordering::Relaxed), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_panes_have_independent_sftp_handles_on_one_tcp_connection() {
    use w11term::sftp::{Job, Operation};

    async fn request(pane: &Session, operation: Operation) -> Result<Value, String> {
        let (reply, response) = tokio::sync::oneshot::channel();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        pane.input
            .send(Command::Sftp(Job {
                expected_generation: None,
                operation,
                deadline,
                reply,
                progress: None,
                cancel: None,
            }))
            .unwrap();
        tokio::time::timeout_at(deadline, response)
            .await
            .unwrap()
            .unwrap()
    }

    // The fixture accepts exactly one TCP connection.
    let (original, source, root, opens, server) = fixture(false).await;
    let Source::Ssh(mut incompatible) = source.clone() else {
        unreachable!()
    };
    {
        let link = original.ssh_connection().unwrap();
        assert!(link.matches(&incompatible, None));
        incompatible.keepalive.interval_seconds += 1;
        assert!(!link.matches(&incompatible, None));
    }
    fs::write(root.path().join("from-second-pane"), b"").unwrap();
    let child = Session::start_configured_with_connection(
        source,
        Default::default(),
        false,
        original.ssh_connection(),
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while child.ssh_connection().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("second pane must use the fixture's single connection");
    let remote = request(&child, Operation::Opendir { path: "/".into() })
        .await
        .unwrap();
    let handle = remote["handle"].as_u64().unwrap();
    assert_eq!(
        request(&original, Operation::Info).await.unwrap()["open_handles"],
        0
    );
    assert!(
        request(&original, Operation::Readdir { handle })
            .await
            .is_err()
    );
    assert_eq!(
        opens.load(Ordering::Relaxed),
        4,
        "two shell and two SFTP channels"
    );
    drop(original);
    let result = request(&child, Operation::Readdir { handle })
        .await
        .unwrap();
    assert_eq!(result["entries"][0]["filename"], "from-second-pane");
    request(&child, Operation::Close { handle }).await.unwrap();
    drop(child);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnecting_one_shell_keeps_its_shared_ssh_transport_available() {
    async fn disconnect(pane: &Session, connection: u64) -> Result<(), String> {
        let (reply, response) = tokio::sync::oneshot::channel();
        pane.input
            .send(Command::DisconnectSshShell { connection, reply })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .unwrap()
            .unwrap()
    }

    let (original, source, _root, opens, server) = fixture(false).await;
    let generation = original.frames.load().connection_generation;
    let child = Session::start_configured_with_connection(
        source,
        Default::default(),
        false,
        original.ssh_connection(),
    );
    let child_generation = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(generation) = child.events.shell_generation() {
                break generation;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("second shell must use the fixture's single TCP connection");
    assert!(disconnect(&original, generation + 1).await.is_err());
    assert_eq!(original.ssh_connection_generation(), Some(generation));
    disconnect(&original, generation).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while original.events.restart_token().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a deliberate disconnect must make the same pane restartable");
    assert!(original.ssh_connection().is_none());
    assert_eq!(child.ssh_connection_generation(), Some(child_generation));
    let (reply, response) = tokio::sync::oneshot::channel();
    child
        .input
        .send(Command::Raw(b"shared-shell-still-live".to_vec(), reply))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), response)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !child
            .frames
            .load()
            .rows
            .iter()
            .any(|line| line.as_str().contains("shared-shell-still-live"))
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the sibling shell must still receive its echo");
    assert_eq!(opens.load(Ordering::Relaxed), 2, "one channel per shell");
    drop(original);
    drop(child);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_group_connection_can_be_stopped_before_using_a_launch_slot() {
    use w11term::events::SshConnectionPhase;

    let accepted = Arc::new(AtomicUsize::new(0));
    let (Source::Ssh(ssh), _root, opens, server) =
        fixture_counting_accepts(false, 1, accepted.clone()).await
    else {
        unreachable!()
    };
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let slot = gate.clone().try_acquire_owned().unwrap();
    let start = || {
        Session::start_group_ssh(
            ssh.clone(),
            wezterm_term::TerminalSize::default(),
            false,
            gate.clone(),
        )
    };
    let stopped = start();
    let waiting = start();
    let attempt = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(attempt) = stopped.events.pending_ssh_attempt()
                && waiting.events.pending_ssh_attempt().is_some()
            {
                break attempt;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        stopped.events.ssh_connection_phase(),
        Some(SshConnectionPhase::Queued)
    );
    assert_eq!(
        waiting.events.ssh_connection_phase(),
        Some(SshConnectionPhase::Queued)
    );
    assert_eq!(accepted.load(Ordering::Relaxed), 0);
    assert!(stopped.events.cancel_ssh_attempt(attempt));
    tokio::time::timeout(Duration::from_secs(5), async {
        while stopped.events.restart_token().is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        stopped.events.ssh_connection_phase(),
        Some(SshConnectionPhase::Stopped)
    );
    drop(slot);
    tokio::time::timeout(Duration::from_secs(5), async {
        while waiting.events.ssh_connection_phase() != Some(SshConnectionPhase::Connected) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    assert_eq!(accepted.load(Ordering::Relaxed), 1);
    assert_eq!(gate.available_permits(), 1);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[cfg(feature = "ui-smoke")]
async fn ssh_shell_setup_times_out_and_releases_the_group_login_slot() {
    use w11term::{events::SshConnectionPhase, ssh::ConnectionStep};

    async fn failure(pane: &Session) -> Arc<str> {
        loop {
            if let Some(SshConnectionPhase::Failed(reason)) = pane.events.ssh_connection_phase() {
                return reason;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    let (Source::Ssh(mut stalled_open), open_root, open_requests, open_server) =
        fixture_server(false).await
    else {
        unreachable!()
    };
    let (Source::Ssh(mut stalled_setup), setup_root, _, setup_server) = fixture_server(false).await
    else {
        unreachable!()
    };
    let (Source::Ssh(healthy), _healthy_root, healthy_opens, healthy_server) =
        fixture_server(false).await
    else {
        unreachable!()
    };
    stalled_open.player.reconnect = false;
    stalled_setup.player.reconnect = false;
    fs::write(open_root.path().join("withhold-session-open"), []).unwrap();
    fs::write(setup_root.path().join("withhold-shell-reply"), []).unwrap();

    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    let start = |options| {
        Session::start_group_ssh(
            options,
            wezterm_term::TerminalSize::default(),
            false,
            gate.clone(),
        )
    };
    let opening = start(stalled_open);
    let setting_up = Session::start(Source::Ssh(stalled_setup));
    tokio::time::timeout(Duration::from_secs(5), async {
        while open_requests.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the first host did not request a session channel");
    let waiting = start(healthy);
    assert_eq!(
        waiting.events.ssh_connection_phase(),
        Some(SshConnectionPhase::Queued)
    );
    assert_eq!(healthy_opens.load(Ordering::Relaxed), 0);

    let (open_error, setup_error, ()) = tokio::time::timeout(Duration::from_secs(25), async {
        tokio::join!(failure(&opening), failure(&setting_up), async {
            while waiting.events.ssh_connection_phase() != Some(SshConnectionPhase::Connected) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    })
    .await
    .expect("the stalled hosts did not time out or the queued host did not connect");
    assert!(
        open_error.contains("SSH session channel open timed out"),
        "{open_error}"
    );
    assert!(
        setup_error.contains("SSH PTY or shell setup timed out"),
        "{setup_error}"
    );
    for (pane, step) in [
        (&opening, ConnectionStep::OpeningSessionChannel),
        (&setting_up, ConnectionStep::RequestingTerminal),
    ] {
        let (reply, inspection) = tokio::sync::oneshot::channel();
        pane.input.send(Command::ReadModes(reply)).unwrap();
        let inspection = inspection.await.unwrap();
        assert!(matches!(
            inspection.ssh_connection,
            Some(SshConnectionPhase::Failed(_))
        ));
        assert!(inspection.ssh.is_none());
        assert_eq!(inspection.ssh_progress.unwrap().step, step);
    }
    assert_eq!(healthy_opens.load(Ordering::Relaxed), 1);
    assert_eq!(gate.available_permits(), 1);
    open_server.abort();
    setup_server.abort();
    healthy_server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_an_ssh_retry_keeps_the_pane_available_for_manual_reconnect() {
    use in_band_resize::Wire;
    use w11term::events::SshConnectionPhase;

    async fn wait_for<T>(description: &str, mut poll: impl FnMut() -> Option<T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(result) = poll() {
                    return result;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect(description)
    }

    let accepted = Arc::new(AtomicUsize::new(0));
    let (wire, mut messages) = tokio::sync::mpsc::unbounded_channel();
    let (Source::Ssh(mut ssh), _root, _, server) =
        fixture_with_wire(false, 2, accepted.clone(), Some(wire)).await
    else {
        unreachable!()
    };
    ssh.player.reconnect = true;
    let pane = Session::start(Source::Ssh(ssh));
    let Wire::Shell(_, server_handle) =
        tokio::time::timeout(Duration::from_secs(5), messages.recv())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("waiting for the first shell");
    };
    let generation = wait_for("first established shell", || {
        pane.ssh_connection_generation()
    })
    .await;
    assert_eq!(pane.events.pending_ssh_attempt(), None);
    server_handle
        .disconnect(
            russh::Disconnect::ByApplication,
            "fixture drops the SSH session".into(),
            "en".into(),
        )
        .await
        .unwrap();

    let attempt = wait_for("the automatic retry to be scheduled", || {
        if matches!(
            pane.events.ssh_connection_phase(),
            Some(SshConnectionPhase::Reconnecting { attempt: Some(_) })
        ) {
            pane.events.pending_ssh_attempt()
        } else {
            None
        }
    })
    .await;
    assert!(pane.events.cancel_ssh_attempt(attempt));
    let restart = wait_for("the stopped pane to be restartable", || {
        pane.events.restart_token()
    })
    .await;
    assert_eq!(
        pane.events.ssh_connection_phase(),
        Some(SshConnectionPhase::Stopped)
    );
    assert_eq!(pane.events.pending_ssh_attempt(), None);
    assert_eq!(pane.ssh_connection_generation(), None);

    // The first automatic retry had a one-second delay.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(accepted.load(Ordering::Relaxed), 1);
    pane.restart_exited(restart).unwrap();
    assert!(!pane.events.cancel_ssh_attempt(attempt));
    wait_for(
        "the manual reconnect to open a new shell in the same pane",
        || {
            pane.ssh_connection_generation()
                .filter(|next| *next > generation)
        },
    )
    .await;
    assert_eq!(accepted.load(Ordering::Relaxed), 2);
    drop(pane);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verified_download_checks_every_chunk_before_replacing_the_destination() {
    use sha2::{Digest, Sha256};
    use w11term::sftp::{CHUNK, Job, Operation};

    let (session, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let destination = local.path().join("result.bin");
    let data: Vec<_> = (0..CHUNK * 2 + 71).map(|i| (i % 251) as u8).collect();
    fs::write(remote.path().join("source.bin"), &data).unwrap();
    fs::write(&destination, b"previous version").unwrap();
    let run = |expected_sha256: String| {
        let (reply, response) = tokio::sync::oneshot::channel();
        session
            .input
            .send(Command::Sftp(Job {
                operation: Operation::Get {
                    remote: "/source.bin".into(),
                    local: destination.clone(),
                    overwrite: true,
                    max_bytes: Some(data.len() as u64),
                    expected_sha256: Some(expected_sha256),
                    resume: false,
                    preserve_mtime: false,
                },
                expected_generation: None,
                deadline: tokio::time::Instant::now() + Duration::from_secs(10),
                reply,
                progress: None,
                cancel: None,
            }))
            .unwrap();
        response
    };
    let hex_digest = |data: &[u8]| {
        Sha256::digest(data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let digest = hex_digest(&data);
    let mut wrong = data.clone();
    *wrong.last_mut().unwrap() ^= 1;
    let bad_digest = hex_digest(&wrong);
    assert!(
        run(bad_digest)
            .await
            .unwrap()
            .unwrap_err()
            .contains("SHA-256 mismatch")
    );
    assert_eq!(fs::read(&destination).unwrap(), b"previous version");
    assert_eq!(fs::read_dir(local.path()).unwrap().count(), 1);
    let valid = run(digest.to_ascii_uppercase()).await.unwrap().unwrap();
    assert_eq!(valid["sha256"], digest);
    assert_eq!(fs::read(&destination).unwrap(), data);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drawer_reports_confirmed_transfer_bytes_and_keeps_remote_errors() {
    use tokio::sync::{oneshot, watch};
    use w11term::sftp::{CHUNK, Job, Operation, TransferProgress};

    fn start(
        session: &Session,
        operation: Operation,
    ) -> (
        watch::Receiver<TransferProgress>,
        oneshot::Receiver<Result<Value, String>>,
    ) {
        let (sender, progress) = watch::channel(TransferProgress::default());
        let (reply, response) = oneshot::channel();
        session
            .input
            .send(Command::Sftp(Job {
                expected_generation: None,
                operation,
                deadline: tokio::time::Instant::now() + Duration::from_secs(5),
                reply,
                progress: Some(w11term::sftp::JobProgress::Transfer(sender)),
                cancel: None,
            }))
            .unwrap();
        (progress, response)
    }

    let (session, _, remote, _, server) = fixture(false).await;
    let local = tempfile::tempdir().unwrap();
    let size = CHUNK * 2 + 123;
    let bytes: Vec<_> = (0..size).map(|i| i as u8).collect();
    fs::write(remote.path().join("source"), &bytes).unwrap();
    let destination = local.path().join("download");
    let download = || Operation::Get {
        remote: "/source".into(),
        local: destination.clone(),
        overwrite: true,
        max_bytes: None,
        expected_sha256: None,
        resume: false,
        preserve_mtime: false,
    };
    let hold = remote.path().join("hold-next-chunk");
    fs::write(&hold, []).unwrap();
    fs::write(&destination, b"previous copy").unwrap();
    let (mut progress, response) = start(&session, download());
    let expected_total = Some(size as u64);
    let partial = tokio::time::timeout(Duration::from_secs(5), progress.wait_for(|p| p.bytes > 0))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        *partial,
        TransferProgress {
            bytes: CHUNK as u64,
            total: expected_total,
            files: None,
        }
    );
    drop(partial);
    assert_eq!(fs::read(&destination).unwrap(), b"previous copy");
    fs::remove_file(hold).unwrap();
    assert_eq!(response.await.unwrap().unwrap()["bytes_transferred"], size);
    assert_eq!(
        *progress.borrow(),
        TransferProgress {
            bytes: size as u64,
            total: expected_total,
            files: None,
        }
    );
    assert_eq!(fs::read(&destination).unwrap(), bytes);

    fs::write(remote.path().join("no-fstat"), []).unwrap();
    let (progress, response) = start(&session, download());
    assert_eq!(response.await.unwrap().unwrap()["bytes_transferred"], size);
    assert_eq!(
        *progress.borrow(),
        TransferProgress {
            bytes: size as u64,
            total: None,
            files: None,
        }
    );

    let upload = || Operation::Put {
        local: destination.clone(),
        remote: "/upload".into(),
        overwrite: false,
        atomic: false,
        preserve_times: false,
    };
    let fail = remote.path().join("fail-next-write");
    fs::write(&fail, []).unwrap();
    let (progress, response) = start(&session, upload());
    assert!(
        response
            .await
            .unwrap()
            .unwrap_err()
            .contains("partial file")
    );
    assert_eq!(
        *progress.borrow(),
        TransferProgress {
            bytes: CHUNK as u64,
            total: expected_total,
            files: None,
        }
    );
    let uploaded = remote.path().join("upload");
    assert_eq!(fs::read(&uploaded).unwrap(), &bytes[..CHUNK]);
    fs::remove_file(fail).unwrap();
    // A retry preserves a partial remote destination until overwrite is requested.
    let (_, response) = start(&session, upload());
    assert!(response.await.unwrap().is_err());
    assert_eq!(fs::read(&uploaded).unwrap(), &bytes[..CHUNK]);
    let (progress, response) = start(
        &session,
        Operation::Put {
            local: destination.clone(),
            remote: "/upload".into(),
            overwrite: true,
            atomic: false,
            preserve_times: false,
        },
    );
    assert_eq!(response.await.unwrap().unwrap()["bytes_transferred"], size);
    assert_eq!(
        *progress.borrow(),
        TransferProgress {
            bytes: size as u64,
            total: expected_total,
            files: None,
        }
    );
    assert_eq!(fs::read(uploaded).unwrap(), bytes);
    fs::write(remote.path().join("disconnect-next-write"), []).unwrap();
    let (progress, response) = start(
        &session,
        Operation::Put {
            local: destination,
            remote: "/dropped".into(),
            overwrite: false,
            atomic: false,
            preserve_times: false,
        },
    );
    let error = response
        .await
        .expect("admitted upload lost its reply")
        .unwrap_err();
    assert!(error.contains("partial"), "{error}");
    assert_eq!(progress.borrow().bytes, CHUNK as u64);
    assert_eq!(
        fs::read(remote.path().join("dropped")).unwrap(),
        &bytes[..CHUNK]
    );
    drop(session);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn asset_pack_sync_verifies_hashes_reuses_cache_and_preserves_old_pack_on_failure() {
    use sha2::{Digest, Sha256};
    let (session, _, root, opens, server) = fixture(false).await;
    let cache = tempfile::tempdir().unwrap();
    fs::write(root.path().join("small-reads"), []).unwrap();
    fs::create_dir_all(root.path().join("assets/sounds")).unwrap();
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&836u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&800u32.to_le_bytes());
    wav.extend_from_slice(&[128; 800]);
    let hash: String = Sha256::digest(&wav)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    fs::write(root.path().join("assets/sounds/shot.wav"), &wav).unwrap();
    let manifest = |version: &str, hash: &str| json!({"version":1,"pack_version":version,"files":[{"name":"shot","path":"sounds/shot.wav","kind":"sound","sha256":hash,"size":wav.len()}]});
    fs::write(
        root.path().join("assets/manifest.json"),
        serde_json::to_vec(&manifest("pack1", &hash)).unwrap(),
    )
    .unwrap();
    let (input, mut jobs) = tokio::sync::mpsc::unbounded_channel();
    let commands = session.input.clone();
    let bridge = tokio::spawn(async move {
        while let Some(w11term::terminal::TransportInput::Sftp(job)) = jobs.recv().await {
            commands.send(Command::Sftp(job)).unwrap();
        }
    });
    let help = b"pilot help";
    let help_hash: String = Sha256::digest(help)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    fs::write(root.path().join("assets/help.txt"), help).unwrap();
    let mut initial = manifest("pack1", &hash);
    initial["files"].as_array_mut().unwrap().push(json!({"name":"help","path":"docs/help.txt","kind":"help","sha256":help_hash,"size":help.len()}));
    fs::create_dir_all(root.path().join("assets/docs")).unwrap();
    fs::write(root.path().join("assets/docs/help.txt"), help).unwrap();
    fs::write(
        root.path().join("assets/manifest.json"),
        serde_json::to_vec(&initial).unwrap(),
    )
    .unwrap();
    let store = w11term::assets::Store::default();
    let sync = |version: &str| {
        w11term::assets::sync_into(
            input.clone(),
            "test@loopback:22".into(),
            version.into(),
            store.clone(),
            cache.path().into(),
        )
    };
    assert_eq!(sync("pack1").await.unwrap(), 2);
    let path = store.sound("shot").unwrap();
    assert_eq!(fs::read(&path).unwrap(), wav);
    fs::remove_file(root.path().join("assets/sounds/shot.wav")).unwrap();
    assert_eq!(
        sync("pack1").await.unwrap(),
        0,
        "unchanged content must not be fetched again"
    );
    fs::write(root.path().join("assets/sounds/shot.wav"), &wav).unwrap();
    fs::write(
        root.path().join("assets/manifest.json"),
        serde_json::to_vec(&manifest("pack2", &"0".repeat(64))).unwrap(),
    )
    .unwrap();
    assert!(
        sync("pack2")
            .await
            .unwrap_err()
            .to_string()
            .contains("mismatch")
    );
    let help_path = path.parent().unwrap().join(&help_hash);
    assert_eq!(
        fs::read(&help_path).unwrap(),
        help,
        "failed replacement must retain non-sound files"
    );
    fs::write(
        root.path().join("assets/manifest.json"),
        serde_json::to_vec(&manifest("pack3", &hash)).unwrap(),
    )
    .unwrap();
    let other = w11term::assets::Store::default();
    w11term::assets::sync_into(
        input.clone(),
        "test@loopback:22".into(),
        "pack3".into(),
        other,
        cache.path().into(),
    )
    .await
    .unwrap();
    assert!(
        help_path.exists(),
        "a second pane must not prune the first pane's generation"
    );
    assert_eq!(store.version(), "pack1");
    assert_eq!(store.sound("shot"), Some(path));
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "asset traffic must reuse the established SSH and SFTP channels"
    );
    bridge.abort();
    drop(session);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorder_events_and_wait_for_text_share_the_terminal_output() {
    let (session, _, _root, _opens, server) = fixture(false).await;
    session.player.write().unwrap().mute = true;
    session.player.write().unwrap().remote_sounds = Some(true);
    let registry = registry(&session);
    let capture: w11term::control::CapturePng = Arc::new(|_, _, _| anyhow::bail!("unused"));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bug.vt");
    registry
        .execute(
            Request::Record {
                pane: 1,
                path: Some(path.clone()),
                timed: false,
            },
            capture.clone(),
        )
        .await
        .unwrap();
    let bytes=b"recorded fps=144\x07\x1b]0;Event title\x1b\\\x1b]9;Test notification\x1b\\\x1b]440;1;play;shot\x1b\\";
    let (reply, response) = tokio::sync::oneshot::channel();
    session
        .input
        .send(Command::Raw(bytes.to_vec(), reply))
        .unwrap();
    response.await.unwrap().unwrap();
    registry
        .execute(
            Request::WaitForText {
                pane: 1,
                pattern: "fps=144".into(),
                regex: false,
                connection_generation: None,
                after_frame: None,
                screenshot_path: None,
                timeout_seconds: 3,
            },
            capture.clone(),
        )
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut cursor = 0;
    let mut events = Vec::new();
    loop {
        let batch = session.events.wait(cursor, Duration::from_secs(1)).await;
        cursor = batch.next;
        events.extend(batch.events);
        if events.iter().any(|e| e.kind == "sound") {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
    }
    assert!(events.iter().any(|e| e.kind == "bell"));
    assert!(
        events
            .iter()
            .any(|e| e.kind == "title" && e.data["title"] == "Event title")
    );
    assert!(
        events
            .iter()
            .any(|e| e.kind == "notification" && e.data["body"] == "Test notification")
    );
    assert!(
        events
            .iter()
            .filter(|e| e.kind == "sound")
            .all(|e| e.data["outcome"] == "muted")
    );
    let result = registry
        .execute(
            Request::Record {
                pane: 1,
                path: None,
                timed: false,
            },
            capture.clone(),
        )
        .await
        .unwrap();
    assert_eq!(result["bytes"], bytes.len());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(path.with_extension("vt.json").is_file());
    assert!(
        registry
            .execute(
                Request::Record {
                    pane: 1,
                    path: Some(path),
                    timed: false,
                },
                capture
            )
            .await
            .is_err(),
        "recordings must not overwrite an existing bug capture"
    );
    drop(session);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transfers_handles_and_rejected_subsystem_without_reconnect() {
    tokio::time::timeout(Duration::from_secs(30),async {
        let (session,_,root,opens,server)=fixture(false).await;
        let panes=registry(&session);
        let discovery=tempfile::tempdir().unwrap();
        let control=control::Server::start_in_directory(panes,|_,_,_|anyhow::bail!("unused"),discovery.path().into()).unwrap();
        let local=tempfile::tempdir().unwrap();let source=local.path().join("binary source.bin");let dest=local.path().join("download.bin");
        let bytes:Vec<u8>=(0..200_003).map(|i|(i%256) as u8).collect();fs::write(&source,&bytes).unwrap();
        assert_eq!(ok(json!({"op":"info"})).await["version"],3);
        ok(json!({"op":"mkdir","path":"/folder"})).await;
        ok(json!({"op":"put","local":source,"remote":"/folder/日本 file.bin"})).await;
        assert!(!request(json!({"op":"put","local":source,"remote":"/folder/日本 file.bin"})).await.ok);
        assert_eq!(ok(json!({"op":"stat","path":"/folder/日本 file.bin"})).await["attrs"]["size"],bytes.len());
        let download=tokio::spawn({let dest=dest.clone();async move{ok(json!({"op":"get","remote":"/folder/日本 file.bin","local":dest})).await}});
        tokio::time::sleep(Duration::from_millis(40)).await;
        let (tx,rx)=tokio::sync::oneshot::channel();session.input.send(Command::Raw(b"interactive".to_vec(),tx)).unwrap();
        tokio::time::timeout(Duration::from_millis(100),rx).await.expect("SFTP blocked terminal input").unwrap().unwrap();
        assert!(!download.is_finished(),"test transfer finished before input assertion");download.await.unwrap();assert_eq!(fs::read(&dest).unwrap(),bytes);
        assert!(session.frames.load().rows.iter().any(|l|l.as_str().contains("interactive")),"shell echo was not processed during SFTP");
        let limited=local.path().join("limited.bin");
        let reply=request(json!({"op":"get","remote":"/folder/日本 file.bin","local":limited,"max_bytes":100})).await;
        assert!(!reply.ok && reply.error.unwrap().contains("max-bytes")); assert!(!limited.exists());
        fs::write(root.path().join("oversized-reads"),[]).unwrap();
        let reply=request(json!({"op":"get","remote":"/folder/日本 file.bin","local":limited})).await;
        assert!(!reply.ok && reply.error.unwrap().contains("more SFTP data")); assert!(!limited.exists());
        fs::remove_file(root.path().join("oversized-reads")).unwrap();
        fs::write(&dest,b"preserved").unwrap();assert!(!request(json!({"op":"get","remote":"/folder/日本 file.bin","local":dest})).await.ok);assert_eq!(fs::read(&dest).unwrap(),b"preserved");
        ok(json!({"op":"get","remote":"/folder/日本 file.bin","local":dest,"overwrite":true})).await;assert_eq!(fs::read(&dest).unwrap(),bytes);
        let h=ok(json!({"op":"opendir","path":"/folder"})).await["handle"].as_u64().unwrap();
        assert_eq!(ok(json!({"op":"readdir","handle":h})).await["entries"][0]["filename"],"日本 file.bin");
        assert_eq!(ok(json!({"op":"readdir","handle":h})).await["eof"],true);ok(json!({"op":"close","handle":h})).await;
        assert!(!request(json!({"op":"readdir","handle":h})).await.ok);
        let h=ok(json!({"op":"open","path":"/folder/日本 file.bin","flags":["read","write"]})).await["handle"].as_u64().unwrap();
        fs::write(root.path().join("oversized-reads"),[]).unwrap();
        assert!(!request(json!({"op":"read","handle":h,"offset":0,"len":3})).await.ok);
        fs::remove_file(root.path().join("oversized-reads")).unwrap();
        // Abandon an admitted read without resetting the shared channel or its handles.
        let _=fs::remove_file(root.path().join("read-entered"));
        fs::write(root.path().join("pause-reads"),[]).unwrap();
        let (reply,response)=tokio::sync::oneshot::channel();
        session.input.send(Command::Sftp(w11term::sftp::Job{expected_generation:None,operation:w11term::sftp::Operation::Read{handle:h,offset:0,len:3},deadline:tokio::time::Instant::now()+Duration::from_secs(5),reply,progress:None,cancel:None})).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !root.path().join("read-entered").exists() { tokio::task::yield_now().await; }
        }).await.unwrap();
        drop(response);
        assert_eq!(ok(json!({"op":"info"})).await["open_handles"],1);
        fs::remove_file(root.path().join("pause-reads")).unwrap();
        ok(json!({"op":"write","handle":h,"offset":1,"data_base64":"AAf/"})).await;
        assert_eq!(ok(json!({"op":"read","handle":h,"offset":1,"len":3})).await["data_base64"],"AAf/");
        ok(json!({"op":"fsetstat","handle":h,"attrs":{"size":4}})).await;
        assert_eq!(ok(json!({"op":"fstat","handle":h})).await["attrs"]["size"],4);ok(json!({"op":"close","handle":h})).await;
        assert_eq!(ok(json!({"op":"ls","path":"/folder"})).await["entries"].as_array().unwrap().len(),1);
        ok(json!({"op":"rename","from":"/folder/日本 file.bin","to":"/folder/renamed"})).await;
        assert!(!request(json!({"op":"remove-dir","path":"/folder"})).await.ok);
        ok(json!({"op":"remove-file","path":"/folder/renamed"})).await;ok(json!({"op":"remove-dir","path":"/folder"})).await;
        assert!(!request(json!({"op":"stat","path":"/missing"})).await.ok);
        assert!(!request(json!({"op":"limits"})).await.ok);
        // Exercise the actual console parser/process through the same running pipe.
        let pid=std::process::id().to_string();
        let output=tokio::task::spawn_blocking(move||std::process::Command::new(env!("CARGO_BIN_EXE_w11term-cli")).args(["--pid",&pid,"sftp","--pane","1","info"]).output().unwrap()).await.unwrap();
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stdout));
        fs::remove_file(root.path().join("read-entered")).unwrap();
        assert_eq!(fs::read_dir(root.path()).unwrap().count(),0);
        let remote_tree = root.path().join("cli-bundle");
        fs::create_dir_all(remote_tree.join("nested/empty")).unwrap();
        fs::write(remote_tree.join("nested/日本.bin"), [0, 255, 19]).unwrap();
        fs::write(remote_tree.join(".marker"), b"complete").unwrap();
        let destination = local.path().join("local bundle");
        let arg = destination.clone();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
                .args(["--pid", &std::process::id().to_string(), "sftp", "--pane", "1", "get-dir", "/cli-bundle"])
                .arg(arg)
                .output()
                .unwrap()
        }).await.unwrap();
        assert!(output.status.success(), "get-dir failed: {}", String::from_utf8_lossy(&output.stdout));
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        let transfer = &response["result"]["sftp"];
        assert_eq!(transfer["files"], 2);
        assert_eq!(transfer["directories"], 3);
        assert_eq!(transfer["skipped"], 0);
        assert_eq!(transfer["bytes_transferred"], 11);
        assert_eq!(fs::read(destination.join("nested/日本.bin")).unwrap(), [0, 255, 19]);
        assert_eq!(fs::read(destination.join(".marker")).unwrap(), b"complete");
        assert!(destination.join("nested/empty").is_dir());
        let caller_dir = local.path().to_owned();
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_w11term-cli"))
                .current_dir(caller_dir)
                .args(["--pid", &std::process::id().to_string(), "sftp", "--pane", "1",
                    "put-dir", "local bundle", "pushed 日本"])
                .output().unwrap()
        }).await.unwrap();
        assert!(output.status.success(), "put-dir failed: {} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let uploaded: Value = serde_json::from_slice(&output.stdout).unwrap();
        let upload = &uploaded["result"]["sftp"];
        assert_eq!(upload["remote"], "pushed 日本");
        assert_eq!(upload["resolved_remote"], "/pushed 日本");
        for field in ["files", "directories", "skipped", "bytes_transferred"] {
            assert_eq!(upload[field], transfer[field], "{field}");
        }
        let pushed = root.path().join("pushed 日本");
        assert_eq!(fs::read(pushed.join("nested/日本.bin")).unwrap(), [0, 255, 19]);
        assert_eq!(fs::read(pushed.join(".marker")).unwrap(), b"complete");
        assert!(pushed.join("nested/empty").is_dir());
        assert_eq!(opens.load(Ordering::Relaxed),2,"SFTP must reuse one sibling channel on the existing SSH connection");
        fs::write(root.path().join("slow"),b"slow").unwrap();
        let handle=ok(json!({"op":"open","path":"/slow","flags":["read"]})).await["handle"].as_u64().unwrap();
        fs::write(root.path().join("pause-reads"),b"").unwrap();
        let timeout_request:Request=serde_json::from_value(json!({"verb":"sftp","pane":1,"timeout_seconds":1,"operation":{"op":"get","remote":"/slow","local":local.path().join("cancelled")}})).unwrap();
        let reply=control::call(Some(std::process::id()),&timeout_request).await.unwrap();
        assert!(!reply.ok);assert!(reply.error.unwrap().contains("timed out"));
        assert!(!local.path().join("cancelled").exists(),"failed download published a partial destination");
        fs::remove_file(root.path().join("pause-reads")).unwrap();
        assert!(!request(json!({"op":"fstat","handle":handle})).await.ok,"timeout must invalidate prior handles");
        ok(json!({"op":"info"})).await;
        assert_eq!(opens.load(Ordering::Relaxed),3,"recovery opens only a new SFTP channel, not a new SSH transport");
        drop(control);drop(session);server.await.unwrap();
        let (session,_,_root,_opens,server)=fixture(true).await;
        let control=control::Server::start_in_directory(registry(&session),|_,_,_|anyhow::bail!("unused"),discovery.path().into()).unwrap();
        let r=request(json!({"op":"info"})).await;assert!(!r.ok);assert!(r.error.unwrap().contains("rejected"));
        let (tx,rx)=tokio::sync::oneshot::channel();session.input.send(Command::Raw(b"still here".to_vec(),tx)).unwrap();rx.await.unwrap().unwrap();
        drop(control);drop(session);server.await.unwrap();
    }).await.expect("SFTP integration timed out");
}
