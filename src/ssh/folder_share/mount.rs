//! One passive SSHFS process and one read-only export per explicit grant.
use super::{
    filesystem::Root,
    protocol::{Export, valid_read_path_encoding},
};
use crate::ssh::{ConnectionShare, NETWORK, durability::bounded_command, sharing::ShellChannel};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::{Buf, Bytes, BytesMut};
use russh::{Channel, ChannelMsg, Sig, client};
use russh_sftp::protocol::{Packet, StatusCode, Version};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{oneshot, watch},
    time::{sleep, timeout},
};

const MAX_PACKET: usize = 64 * 1024;

pub fn valid_mount_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FolderMountStatus {
    Starting,
    Mounted,
    Unmounting,
    Stopped,
    Failed(String),
}

/// Owns a single export. Dropping the owner revokes new reads and requests
/// teardown even if the pane that opened it is already gone.
pub struct FolderMount {
    state: Arc<Mutex<FolderMountStatus>>,
    revoked: Arc<AtomicBool>,
    stop: watch::Sender<bool>,
}

impl FolderMount {
    pub fn start(connection: ConnectionShare, root: Root, name: &str) -> Result<Self> {
        ensure!(valid_mount_name(name), "Invalid Linux mount name");
        ensure!(!connection.handle().is_closed(), "SSH connection is closed");
        let state = Arc::new(Mutex::new(FolderMountStatus::Starting));
        let revoked = root.revoked.clone();
        let (stop, receiver) = watch::channel(false);
        let mount = Self {
            state: state.clone(),
            revoked: revoked.clone(),
            stop,
        };
        let remote = Remote::new(name);
        NETWORK.spawn(async move {
            let result = run(connection, remote, root, &state, &revoked, receiver).await;
            revoked.store(true, Ordering::Release);
            *state.lock().unwrap() = match result {
                Ok(()) => FolderMountStatus::Stopped,
                Err(e) => FolderMountStatus::Failed(diagnostic(&format!("{e:#}"))),
            };
        });
        Ok(mount)
    }
    pub fn status(&self) -> FolderMountStatus {
        self.state.lock().unwrap().clone()
    }
    pub fn unmount(&self) {
        let mut state = self.state.lock().unwrap();
        if matches!(
            *state,
            FolderMountStatus::Starting | FolderMountStatus::Mounted
        ) {
            self.revoked.store(true, Ordering::Release);
            *state = FolderMountStatus::Unmounting;
            self.stop.send_replace(true);
        }
    }
}
impl Drop for FolderMount {
    fn drop(&mut self) {
        self.unmount();
    }
}

fn diagnostic(message: &str) -> String {
    message
        .chars()
        .take(512)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

// No user-supplied text is shell source. A validated name travels as one base64
// argument with no shell metacharacters; $2 is a client-generated hex token.
struct Remote {
    name_arg: String,
    token: String,
}
impl Remote {
    fn new(name: &str) -> Self {
        Self {
            name_arg: STANDARD.encode(name),
            token: format!("w11term-{:032x}", rand::random::<u128>()),
        }
    }
    fn command(&self, script: &str) -> String {
        format!(
            "exec /bin/sh -c '{}' w11term {} {}",
            script.replace('\'', "'\\''"),
            self.name_arg,
            self.token
        )
    }
}

const START: &str = r#"set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin; LC_ALL=C; export PATH LC_ALL
fail() { printf '%s\n' "$*" >&2; exit 1; }
for tool in sshfs fusermount3 flock mountpoint stat find findmnt base64 id; do
    command -v "$tool" >/dev/null || fail "Folder sharing requires $tool on the SSH host"
done
[ -c /dev/fuse ] && [ -r /dev/fuse ] && [ -w /dev/fuse ] || fail "No access to /dev/fuse"
name=$(printf '%s' "$1" | base64 -d)
uid=$(id -u)
umask 077
case "$HOME" in /*) ;; *) fail 'An absolute home directory is required';; esac
[ ! -L "$HOME" ] && cd -P "$HOME" && [ "$PWD" = "$HOME" ] || fail 'A real home directory is required'
[ "$(stat -c %u .)" = "$uid" ] || fail 'The account must own its home directory'
mode=$(stat -c %a .)
[ "$((0$mode & 022))" = 0 ] || fail 'The home directory is writable by another user'
mount_dir() {
    [ ! -L "$1" ] || fail 'Folder sharing refuses symbolic links'
    if [ -d "$1" ]; then
        if mountpoint -q -- "$1"; then fail 'The destination is already mounted'
        else code=$?; [ "$code" = 32 ] || fail 'Cannot verify the mount directory'; fi
    fi
    if [ ! -d "$1" ]; then mkdir -m 700 -- "$1" 2>/dev/null || [ -d "$1" ] || fail 'Cannot create the mount directory'; fi
    [ ! -L "$1" ] && [ "$(stat -c '%u' -- "$1")" = "$uid" ] || fail 'The account must own its mount directories'
    mode=$(stat -c %a -- "$1")
    if [ "$2" = parent ]; then
        [ "$((0$mode & 022))" = 0 ] || fail 'The mount parent is writable by another user'
    else
        [ "$mode" = 700 ] || fail 'The mount and lock directories must have mode 0700'
    fi
}
mount_dir windows parent
cd -P windows
mount_dir .w11term-locks private
mount_dir ".w11term-locks/$name" private
exec 9<".w11term-locks/$name"
flock -n 9 || fail 'Another folder mount already holds this name'
mount_dir "$name" private
cd -P "$name"
[ "$PWD" = "$HOME/windows/$name" ] || fail 'The mount point changed'
contents=$(find . -mindepth 1 -maxdepth 1 -printf x -quit) || fail 'Cannot inspect the mount point'
[ -z "$contents" ] || fail 'The mount point is not empty'
exec sshfs -f -o "passive,ro,auto_unmount,subtype=sshfs.$2,fsname=$2" :/ .
"#;

const PROBE: &str = r#"set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin; LC_ALL=C; export PATH LC_ALL
name=$(printf '%s' "$1" | base64 -d)
printf '%s ' "$(id -u)"
findmnt --kernel --raw --noheadings --nocanonicalize --mountpoint "$HOME/windows/$name" --output FSTYPE,SOURCE,VFS-OPTIONS,FS-OPTIONS
"#;

// Search kernel mountinfo by the generation's subtype, even if a parent was
// renamed. Never send a path-based unmount or remove a user's directory.
const ABSENT: &str = r#"exec /usr/bin/awk -v type="fuse.sshfs.$2" '{for (i=7;i<=NF;i++) if ($i == "-" && $(i+1) == type) found=1} END {exit found ? 1 : 0}' /proc/self/mountinfo"#;

// libfuse 3.15 taught its setuid helper to retry the dead-filesystem check
// as the mount owner. Earlier helpers silently leave owner-only mounts
// behind after the SSHFS process exits.
const HELPER_VERSION: &str =
    "PATH=/usr/sbin:/usr/bin:/sbin:/bin; LC_ALL=C; export PATH LC_ALL; exec fusermount3 --version";

fn check_helper_version(stdout: &[u8]) -> Result<()> {
    let version = std::str::from_utf8(stdout)
        .ok()
        .and_then(|line| line.trim().strip_prefix("fusermount3 version: "))
        .and_then(|number| semver::Version::parse(number).ok())
        .context("Could not verify fusermount3. Install FUSE 3.15.0 or newer on the SSH host")?;
    ensure!(
        version >= semver::Version::new(3, 15, 0),
        "Found fusermount3 {version}. Upgrade the SSH host's fuse3 package to 3.15.0 or newer for reliable owner-only unmount"
    );
    Ok(())
}

async fn verify(link: &ConnectionShare, remote: &Remote, absent: bool) -> Result<()> {
    let command = remote.command(if absent { ABSENT } else { PROBE });
    loop {
        let answer = bounded_command(link, &command, 4096).await?;
        if absent && answer.status == 0 {
            return Ok(());
        }
        if !absent && answer.status == 0 {
            return verify_record(std::str::from_utf8(&answer.stdout)?, &remote.token);
        }
        ensure!(
            answer.status == 1,
            "Could not inspect Linux mount state: {}",
            diagnostic(&String::from_utf8_lossy(&answer.stderr))
        );
        sleep(Duration::from_millis(200)).await;
    }
}

fn verify_record(text: &str, token: &str) -> Result<()> {
    let fields: Vec<_> = text.split_ascii_whitespace().collect();
    let [uid, fs_type, source, vfs_options, fs_options] = fields.as_slice() else {
        bail!("Could not identify one Linux mount at the destination");
    };
    ensure!(
        *fs_type == format!("fuse.sshfs.{token}") && *source == token,
        "A different filesystem occupies the mount point ({fs_type}, {source})"
    );
    let owner = format!(
        "user_id={}",
        uid.parse::<u32>().context("Invalid Linux account uid")?
    );
    ensure!(
        vfs_options.split(',').any(|s| s == "ro")
            && fs_options.split(',').any(|s| s == owner)
            && !vfs_options
                .split(',')
                .chain(fs_options.split(','))
                .any(|s| matches!(s, "allow_other" | "allow_root")),
        "SSHFS did not mount read-only with owner-only access"
    );
    Ok(())
}

async fn run(
    link: ConnectionShare,
    remote: Remote,
    root: Root,
    state: &Mutex<FolderMountStatus>,
    revoked: &Arc<AtomicBool>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    if *stop.borrow() {
        return Ok(());
    }
    let helper = tokio::select! {
        biased;
        _ = stop.changed() => return Ok(()),
        answer = bounded_command(&link, HELPER_VERSION, 1024) =>
            answer.context("Could not check fusermount3 on the SSH host")?,
    };
    ensure!(
        helper.status == 0,
        "Could not run fusermount3. Install FUSE 3.15.0 or newer on the SSH host"
    );
    check_helper_version(&helper.stdout)?;
    let mut shell = tokio::select! {
        biased;
        _ = stop.changed() => return Ok(()),
        channel = timeout(Duration::from_secs(12), link.open_shell()) => {
            channel.context("Timed out opening the mount channel")??
        }
    };
    if *stop.borrow() {
        return Ok(());
    }
    timeout(
        Duration::from_secs(6),
        shell.channel().exec(true, remote.command(START)),
    )
    .await
    .context("Timed out starting SSHFS")??;
    let (init, initialized) = oneshot::channel();
    let (enable, enabled) = oneshot::channel();
    let mut mounted = false;
    let mut closed = false;
    let result = {
        let service = serve(
            shell.channel(),
            Export::new(root),
            revoked,
            init,
            enabled,
            &mut closed,
        );
        tokio::pin!(service);
        let ready = timeout(Duration::from_secs(24), async {
            initialized.await.context("SSHFS did not negotiate SFTP")?;
            verify(&link, &remote, false).await
        });
        tokio::pin!(ready);
        let mut enable = Some(enable);
        loop {
            tokio::select! {
                biased;
                _ = stop.changed() => break Ok(()),
                done = &mut service => {
                    if done.is_ok() && enable.is_some() { break Err(anyhow::anyhow!("SSHFS exited before mounting")); }
                    break done;
                }
                confirmed = &mut ready, if enable.is_some() => {
                    if let Err(e) = confirmed.context("Timed out verifying the Linux mount").and_then(|r| r) {
                        break Err(e);
                    }
                    let mut status = state.lock().unwrap();
                    if *status != FolderMountStatus::Starting {break Ok(());}
                    *status = FolderMountStatus::Mounted;
                    mounted = true;
                    let _ = enable.take().unwrap().send(());
                }
            }
        }
    };
    revoked.store(true, Ordering::Release);
    let shutdown = async {
        if closed {
            (true, String::new())
        } else {
            finish(shell, mounted).await
        }
    };
    let absent = async {
        timeout(Duration::from_secs(12), verify(&link, &remote, true))
            .await
            .context("Timed out verifying the SSHFS generation is absent from Linux mountinfo")?
    };
    let cleanup = confirm_shutdown(mounted, shutdown, absent).await;
    match (result, cleanup) {
        (Err(start), Err(stop)) => Err(anyhow::anyhow!(
            "{start:#}. Mount cleanup unconfirmed: {stop:#}"
        )),
        (result, cleanup) => result.and(cleanup),
    }
}

async fn confirm_shutdown(
    mounted: bool,
    shutdown: impl Future<Output = (bool, String)>,
    absent: impl Future<Output = Result<()>>,
) -> Result<()> {
    // Before a mount has been seen, an early empty mount table cannot prove
    // that the remote startup script has stopped. Once verified, mountinfo
    // can confirm removal even if inherited stderr delays SSH CLOSE.
    let ((exited, stderr), absent) = if mounted {
        tokio::join!(shutdown, absent)
    } else {
        (shutdown.await, absent.await)
    };
    let result = absent.and_then(|()| {
        ensure!(
            mounted || exited,
            "Could not confirm the SSHFS startup process exited"
        );
        Ok(())
    });
    if stderr.is_empty() {
        result
    } else {
        result.with_context(|| format!("SSHFS shutdown: {stderr}"))
    }
}

async fn finish(mut shell: ShellChannel, mounted: bool) -> (bool, String) {
    let ch = shell.channel();
    // A verified read-only mount has a separate fusermount3 auto-unmount
    // helper. Killing this channel's SSHFS process lets that helper match the
    // generation-specific subtype without SSHFS performing its normal unmount
    // by pathname. Let a partially started helper finish initialization.
    let mut stderr = Vec::new();
    let mut exited = false;
    let _ = timeout(Duration::from_secs(2), async {
        let _ = ch.signal(if mounted { Sig::KILL } else { Sig::TERM }).await;
        let _ = ch.eof().await;
        loop {
            match ch.wait().await {
                Some(ChannelMsg::Close) | None => {
                    exited = true;
                    break;
                }
                Some(ChannelMsg::ExitStatus { .. } | ChannelMsg::ExitSignal { .. }) => {
                    exited = true
                }
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    let take = data.len().min(2048 - stderr.len());
                    stderr.extend_from_slice(&data[..take]);
                }
                _ => {}
            }
        }
    })
    .await;
    let _ = timeout(Duration::from_secs(1), shell.close()).await;
    (exited, diagnostic(String::from_utf8_lossy(&stderr).trim()))
}

enum Request {
    Init,
    Reply(Packet),
    Disk(Packet),
}

fn is_root_query(packet: &Packet) -> bool {
    let path = match packet {
        Packet::RealPath(p) => &p.path,
        Packet::Stat(p) => &p.path,
        Packet::Lstat(p) => &p.path,
        _ => return false,
    };
    matches!(path.as_str(), "/" | ".")
}

fn decode(mut body: Bytes, initialized: bool) -> Result<Request> {
    ensure!(body.len() >= 5, "Truncated SFTP request");
    let kind = body[0];
    let id = u32::from_be_bytes(body[1..5].try_into()?);
    if kind == 1 {
        ensure!(
            !initialized && id >= 3 && body.len() == 5,
            "Invalid SFTP initialization"
        );
        return Ok(Request::Init);
    }
    ensure!(initialized, "Missing SFTP initialization");
    // Do not deserialize writes, extensions or response packets from this peer.
    // The only payload allocated for a local operation fits in one capped frame.
    match kind {
        3 | 4 | 5 | 7 | 8 | 11 | 12 | 16 | 17 => {
            ensure!(
                valid_read_path_encoding(&body),
                "Invalid SFTP path encoding"
            );
            let packet = Packet::try_from(&mut body)?;
            ensure!(body.is_empty(), "Trailing SFTP input");
            Ok(Request::Disk(packet))
        }
        6 | 9 | 10 | 13 | 14 | 15 | 18 | 20 => Ok(Request::Reply(Packet::error(
            id,
            StatusCode::PermissionDenied,
        ))),
        19 | 200 => Ok(Request::Reply(Packet::error(id, StatusCode::OpUnsupported))),
        _ => bail!("Unexpected SFTP packet type"),
    }
}

async fn serve(
    channel: &mut Channel<client::Msg>,
    mut export: Export,
    revoked: &Arc<AtomicBool>,
    init: oneshot::Sender<()>,
    enabled: oneshot::Receiver<()>,
    closed: &mut bool,
) -> Result<()> {
    let mut frames = BytesMut::new();
    let mut init = Some(init);
    let mut enabled = Some(enabled);
    let mut accepted = false;
    let mut code = None;
    let mut stderr = Vec::new();
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Success => accepted = true,
            ChannelMsg::Failure => bail!("The SSH server refused the SSHFS exec channel"),
            ChannelMsg::ExtendedData { data, .. } => {
                let take = data.len().min(2048 - stderr.len());
                stderr.extend_from_slice(&data[..take]);
            }
            ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
            ChannelMsg::Close => break,
            ChannelMsg::Data { data } => {
                ensure!(
                    accepted && data.len() + frames.len() <= 2 * MAX_PACKET,
                    "Invalid SSHFS input window"
                );
                frames.extend_from_slice(&data);
                while frames.len() >= 4 {
                    let length = u32::from_be_bytes(frames[..4].try_into()?) as usize;
                    ensure!(
                        (1..=MAX_PACKET).contains(&length),
                        "Invalid SSHFS packet length"
                    );
                    if frames.len() < 4 + length {
                        break;
                    }
                    frames.advance(4);
                    let reply = match decode(frames.split_to(length).freeze(), init.is_none())? {
                        Request::Init => {
                            let _ = init.take().unwrap().send(());
                            Packet::from(Version::new())
                        }
                        Request::Reply(p) => p,
                        Request::Disk(p) => {
                            // SSHFS can stat its synthetic root during startup. Only
                            // the verified mount may enumerate names or read files.
                            if !is_root_query(&p)
                                && let Some(gate) = enabled.take()
                            {
                                gate.await.context("Folder mount was not verified")?;
                            }
                            let gate = revoked.clone();
                            let (next, reply) = tokio::task::spawn_blocking(move || {
                                ensure!(!gate.load(Ordering::Acquire), "Folder share revoked");
                                let packet = export.reply(p);
                                anyhow::Ok((export, packet))
                            })
                            .await??;
                            export = next;
                            reply
                        }
                    };
                    ensure!(!revoked.load(Ordering::Acquire), "Folder share revoked");
                    channel.data_bytes(Bytes::try_from(reply)?).await?;
                }
            }
            _ => {}
        }
    }
    *closed = true;
    ensure!(
        accepted && init.is_none() && code == Some(0) && frames.is_empty(),
        "SSHFS exited ({code:?}): {}",
        diagnostic(&String::from_utf8_lossy(&stderr))
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh_sftp::protocol::{RealPath, Status};

    #[test]
    fn helper_must_support_owner_only_auto_unmount() {
        for output in [
            "fusermount3 version: 3.15.0\n",
            "fusermount3 version: 3.17.2",
        ] {
            assert!(check_helper_version(output.as_bytes()).is_ok(), "{output}");
        }
        for output in [
            "fusermount3 version: 3.14.9",
            "fusermount3 version: 3.15.0-rc.1",
            "fusermount3 version: unknown",
            "sshfs version: 3.17.2",
            "",
        ] {
            let error = check_helper_version(output.as_bytes()).unwrap_err();
            assert!(error.to_string().contains("3.15.0 or newer"), "{output}");
        }
    }

    #[tokio::test]
    async fn verified_mount_can_disappear_before_ssh_close_but_startup_must_exit() -> Result<()> {
        let (probed, seen) = oneshot::channel();
        let drain = async {
            seen.await.unwrap();
            (false, String::new())
        };
        let kernel_absence = async {
            probed.send(()).unwrap();
            Ok(())
        };
        timeout(
            Duration::from_secs(2),
            confirm_shutdown(true, drain, kernel_absence),
        )
        .await??;

        let (done, confirmed) = oneshot::channel();
        let drain = async {
            done.send(()).unwrap();
            (false, String::new())
        };
        let kernel_absence = async {
            confirmed.await.unwrap();
            Ok(())
        };
        let error = confirm_shutdown(false, drain, kernel_absence)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("startup process exited"));
        Ok(())
    }

    #[test]
    fn mount_names_are_portable_components_transported_as_data() {
        for s in [
            "",
            ".",
            "..",
            ".w11term-locks",
            "-opt",
            "a/b",
            "a\\b",
            "a:b",
            "a b",
            "$HOME",
            "x'y",
            "x;y",
            "雪",
        ] {
            assert!(!valid_mount_name(s), "{s:?}");
        }
        assert!(valid_mount_name(&"a".repeat(64)));
        assert!(!valid_mount_name(&"a".repeat(65)));
        let name = "HUD_assets-3.4";
        assert!(valid_mount_name(name));
        let remote = Remote::new(name);
        let command = remote.command(START);
        assert!(!command.contains(name));
        assert_eq!(STANDARD.decode(&remote.name_arg).unwrap(), name.as_bytes());
        assert_ne!(remote.token, Remote::new(name).token);
    }

    #[test]
    fn readiness_requires_the_exact_mount_generation_owner_and_access() {
        let valid =
            "1001 fuse.sshfs.w11term-test w11term-test ro,nosuid rw,user_id=1001,group_id=1001";
        assert!(verify_record(valid, "w11term-test").is_ok());
        for mismatch in [
            valid.replace("user_id=1001", "user_id=0"),
            valid.replace("ro,nosuid", "rw,nosuid"),
            valid.replace("nosuid", "allow_other"),
            valid.replace("group_id=1001", "allow_root"),
            format!("{valid}\n{valid}"),
        ] {
            assert!(verify_record(&mismatch, "w11term-test").is_err());
        }
        assert!(verify_record(valid, "w11term-other").is_err());
    }

    #[test]
    fn unmount_or_drop_revokes_the_root_once_before_returning() -> Result<()> {
        let temp = tempfile::tempdir()?;
        std::fs::write(temp.path().join("file"), b"local")?;
        for initial in [FolderMountStatus::Starting, FolderMountStatus::Mounted] {
            let root = Root::open(temp.path())?;
            let (stop, watch) = watch::channel(false);
            let mount = FolderMount {
                state: Arc::new(Mutex::new(initial.clone())),
                revoked: root.revoked.clone(),
                stop,
            };
            if initial == FolderMountStatus::Mounted {
                mount.unmount();
                mount.unmount();
                assert_eq!(mount.status(), FolderMountStatus::Unmounting);
            }
            drop(mount);
            assert!(*watch.borrow());
            assert_eq!(
                root.file("file").unwrap_err().kind(),
                std::io::ErrorKind::Interrupted
            );
            assert!(root.metadata("/").is_err() && root.list("/").is_err());
        }
        Ok(())
    }

    #[test]
    fn wire_gates_disk_access_and_rejects_malformed_frames() -> Result<()> {
        let init = Bytes::from_static(&[1, 0, 0, 0, 3]);
        assert!(matches!(decode(init.clone(), false)?, Request::Init));
        assert!(decode(init, true).is_err());
        let mut wire = Bytes::try_from(Packet::from(RealPath {
            id: 1,
            path: "/a".into(),
        }))?;
        wire.advance(4);
        assert!(decode(wire.clone(), false).is_err());
        assert!(matches!(
            decode(wire, true)?,
            Request::Disk(Packet::RealPath(_))
        ));
        assert!(is_root_query(
            &RealPath {
                id: 1,
                path: "/".into()
            }
            .into()
        ));
        assert!(!is_root_query(
            &RealPath {
                id: 1,
                path: "/a".into()
            }
            .into()
        ));
        assert!(!is_root_query(
            &russh_sftp::protocol::OpenDir {
                id: 1,
                path: "/".into()
            }
            .into()
        ));
        let denied = Bytes::from_static(&[6, 0, 0, 0, 9, 255, 255, 255, 255]);
        assert!(matches!(
            decode(denied, true)?,
            Request::Reply(Packet::Status(Status {
                id: 9,
                status_code: StatusCode::PermissionDenied,
                ..
            }))
        ));
        assert!(decode(Bytes::from_static(&[104, 0, 0, 0, 1]), true).is_err());
        assert!(decode(Bytes::from_static(&[16, 0, 0, 0, 1, 0, 0, 0, 1, 255]), true).is_err());
        Ok(())
    }
}
