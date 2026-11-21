//! Check an actual selected Windows folder from an unprivileged Linux shell.
use super::folder_share::{Export, Root, valid_read_path_encoding};
use super::{ConnectionShare, durability::bounded_command, sharing::ShellChannel};
use crate::{
    options::{Authentication, Source, SshOptions},
    session::{Command, Session},
};
use anyhow::{Context, Result, bail, ensure};
use bytes::{Buf, Bytes, BytesMut};
use russh::{
    ChannelMsg,
    keys::{HashAlg, PublicKey},
};
use russh_sftp::protocol::{Packet, Version};
use sha2::{Digest, Sha256};
use std::{env, fs, process::Command as ProcessCommand, sync::Arc, time::Duration};
use tokio::{
    task::JoinSet,
    time::{Instant, sleep, timeout},
};

const NAME: &str = "windows-binary.bin";
const CONTENT: &[u8] = b"w11term\x00\xff\x80\r\npassive SSHFS\x1b\xfe\x02";
const MAX_PACKET: usize = 64 * 1024;

async fn serve(mut shell: ShellChannel, mount: &str, mut export: Export) -> Result<()> {
    let channel = shell.channel();
    channel
        .exec(true, format!("exec sshfs -f -o passive,ro :/ {mount}"))
        .await?;
    let mut frames = BytesMut::new();
    let mut stderr = Vec::new();
    let mut accepted = false;
    let mut initialized = false;
    let mut status_code = None;
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => accepted = true,
            Some(ChannelMsg::Failure) => bail!("sshd refused the SSHFS exec channel"),
            Some(ChannelMsg::ExtendedData { data, .. }) => {
                let take = data.len().min(4096 - stderr.len());
                stderr.extend_from_slice(&data[..take]);
            }
            Some(ChannelMsg::Data { data }) => {
                ensure!(
                    accepted && frames.len() + data.len() <= 2 * MAX_PACKET,
                    "invalid SSHFS input window"
                );
                frames.extend_from_slice(&data);
                while frames.len() >= 4 {
                    let len = u32::from_be_bytes(frames[..4].try_into()?) as usize;
                    ensure!(
                        (1..=MAX_PACKET).contains(&len),
                        "invalid SFTP packet length"
                    );
                    if frames.len() < 4 + len {
                        break;
                    }
                    frames.advance(4);
                    let mut body = frames.split_to(len).freeze();
                    ensure!(
                        valid_read_path_encoding(&body),
                        "invalid SFTP path encoding"
                    );
                    let request = Packet::try_from(&mut body)?;
                    ensure!(body.is_empty(), "trailing SFTP input");
                    let reply = match request {
                        Packet::Init(init) if !initialized && init.version >= 3 => {
                            initialized = true;
                            Version::new().into()
                        }
                        Packet::Init(_) => bail!("unexpected SFTP initialization"),
                        _ if !initialized => bail!("expected SFTP initialization"),
                        request => export.reply(request),
                    };
                    channel.data_bytes(Bytes::try_from(reply)?).await?;
                }
            }
            Some(ChannelMsg::ExitStatus { exit_status }) => status_code = Some(exit_status),
            Some(ChannelMsg::Close) | None => break,
            _ => {}
        }
    }
    ensure!(
        accepted && initialized && status_code == Some(0) && frames.is_empty(),
        "SSHFS stopped with {status_code:?}: {}",
        String::from_utf8_lossy(&stderr)
    );
    Ok(())
}

async fn checked(link: &ConnectionShare, command: &str) -> Result<Vec<u8>> {
    let result = bounded_command(link, command, 8192).await?;
    ensure!(
        result.status == 0,
        "guest command exited {}: {command}\n{}",
        result.status,
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result.stdout)
}

async fn shell_pid(pane: &Session, label: &str) -> Result<u32> {
    let (ack, acknowledged) = tokio::sync::oneshot::channel();
    pane.input
        .send(Command::Raw(
            format!("printf '\\n{label}=%s\\n' \"$$\"\r").into_bytes(),
            ack,
        ))
        .map_err(|_| anyhow::anyhow!("interactive pane closed"))?;
    acknowledged.await?.map_err(anyhow::Error::msg)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let frame = pane.frames.load();
        if let Some(pid) = frame.rows.iter().find_map(|row| {
            row.as_str()
                .trim()
                .strip_prefix(label)?
                .strip_prefix('=')?
                .parse()
                .ok()
        }) {
            return Ok(pid);
        }
        ensure!(
            pane.frames.changed(frame.generation, deadline).await,
            "missing shell PID marker {label}"
        );
    }
}

async fn check_mount(link: &ConnectionShare, mount: &str, tile_digest: &str) -> Result<()> {
    let record = format!("awk '$5 == \"{mount}\" {{ print }}' /proc/self/mountinfo");
    loop {
        let bytes = checked(link, &record).await?;
        if !bytes.is_empty() {
            let line = std::str::from_utf8(&bytes)?;
            let (fields, fs) = line.split_once(" - ").context("malformed mountinfo")?;
            ensure!(
                fs.split_whitespace().next() == Some("fuse.sshfs"),
                "wrong mount: {line}"
            );
            ensure!(
                fields
                    .split_whitespace()
                    .nth(5)
                    .is_some_and(|opts| opts.split(',').any(|s| s == "ro")),
                "writable mount: {line}"
            );
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    let listing = checked(
        link,
        &format!("LC_ALL=C ls -1A --quoting-style=literal {mount}"),
    )
    .await?;
    ensure!(
        listing == format!("sprites 雪\n{NAME}\n").as_bytes(),
        "unexpected exported entries: {}",
        String::from_utf8_lossy(&listing)
    );
    ensure!(
        checked(link, &format!("cat {mount}/{NAME}")).await? == CONTENT,
        "guest read different bytes"
    );
    let hash = checked(link, &format!("sha256sum < '{mount}/sprites 雪/tile.bin'")).await?;
    ensure!(
        std::str::from_utf8(&hash)?.split_whitespace().next() == Some(tile_digest),
        "nested file changed across multiple SFTP reads"
    );
    checked(
        link,
        &format!("test ! -e '{mount}/other-folder/secret' && test ! -e '{mount}/other-file'"),
    )
    .await?;
    checked(
        link,
        &format!("if (printf X >> {mount}/{NAME}); then exit 1; fi"),
    )
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the disposable Linux FUSE guest and pinned loopback SSH supplied by the mount runner"]
async fn native_windows_passive_sshfs() -> Result<()> {
    let port = env::var("W11TERM_MOUNT_TEST_PORT")?.parse::<u16>()?;
    let key = PublicKey::from_openssh(&env::var("W11TERM_MOUNT_TEST_HOST_KEY")?)?;
    let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
    let options = SshOptions {
        host: "127.0.0.1".into(),
        port,
        user: "w11fixture".into(),
        authentication: Authentication::Identity {
            identity: env::var_os("W11TERM_MOUNT_TEST_IDENTITY")
                .context("missing temporary client key")?
                .into(),
        },
        host_key: Some(fingerprint.clone()),
        proxy_jump: vec![],
        terminal_type: Default::default(),
        durable: false,
        player: crate::options::PlayerOptions {
            reconnect: false,
            ..Default::default()
        },
    };
    let pane = Session::start_configured(Source::Ssh(options), Default::default(), false);
    let link = timeout(Duration::from_secs(20), async {
        loop {
            if let Some(link) = pane.ssh_connection() {
                return anyhow::Ok(link);
            }
            ensure!(
                pane.events.restart_token().is_none(),
                "{}",
                pane.frames.load().status
            );
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("loopback SSH login timed out")??;
    ensure!(
        link.fingerprint() == fingerprint,
        "unexpected SSH host identity"
    );
    let before = timeout(Duration::from_secs(6), shell_pid(&pane, "BEFORE_MOUNT")).await??;
    let fixture = tempfile::tempdir()?;
    let selected = fixture.path().join("selected");
    let outside = fixture.path().join("sibling");
    fs::create_dir(&selected)?;
    fs::create_dir(&outside)?;
    fs::create_dir(selected.join("sprites 雪"))?;
    let mut tiles = vec![0u8; 96 * 1024 + 17];
    rand::fill(&mut tiles);
    let tile_digest = format!("{:x}", Bytes::copy_from_slice(&Sha256::digest(&tiles)));
    fs::write(selected.join("sprites 雪").join("tile.bin"), tiles)?;
    let secret = outside.join("secret");
    fs::write(&secret, b"sibling data")?;
    fs::hard_link(&secret, selected.join("other-file"))?;
    // All shell arguments are fixed. A junction works on a stock, unelevated Windows host.
    let junction = ProcessCommand::new("cmd.exe")
        .current_dir(fixture.path())
        .args([
            "/d",
            "/c",
            "mklink",
            "/j",
            r"selected\other-folder",
            "sibling",
        ])
        .output()?;
    ensure!(
        junction.status.success(),
        "creating fixture junction: {}",
        String::from_utf8_lossy(&junction.stderr)
    );
    ensure!(fs::read(selected.join("other-folder").join("secret"))? == b"sibling data");
    let path = selected.join(NAME);
    fs::write(&path, CONTENT)?;
    let export = Export::new(Root::open(&selected)?);
    let mount = format!("/tmp/w11term-mount-{:032x}", rand::random::<u128>());
    let shell = timeout(Duration::from_secs(6), link.open_shell()).await??;
    checked(&link, &format!("umask 077 && mkdir {mount}")).await?;
    let mut servers = JoinSet::new();
    let mount_arg = mount.clone();
    servers.spawn(async move { serve(shell, &mount_arg, export).await });
    let verified = timeout(Duration::from_secs(24), async {
        tokio::select! {
            result = check_mount(&link, &mount, &tile_digest) => result,
            ended = servers.join_next() => bail!("SSHFS quit before mount verification: {ended:?}"),
        }
    })
    .await
    .context("SSHFS mount/read timed out")
    .and_then(|result| result);
    // Clean up only our freshly created, private directory, even when checks fail.
    let mounted =
        format!("awk '$5 == \"{mount}\" {{present=1}} END {{exit !present}}' /proc/self/mountinfo");
    let unmount = format!(
        "set -e
         if {mounted}; then fusermount3 -u {mount}; fi
         if {mounted}; then echo 'mount still present' >&2; exit 1; fi
         rmdir {mount}"
    );
    let cleanup = checked(&link, &unmount).await;
    verified?;
    cleanup?;
    timeout(Duration::from_secs(6), servers.join_next())
        .await?
        .context("SSHFS task missing")???;
    ensure!(
        fs::read(path)? == CONTENT && fs::read(secret)? == b"sibling data",
        "Windows fixture changed"
    );
    let after = timeout(Duration::from_secs(6), shell_pid(&pane, "AFTER_MOUNT")).await??;
    let still_live = pane
        .ssh_connection()
        .context("interactive SSH connection lost")?;
    ensure!(
        after == before && Arc::ptr_eq(&still_live.handle(), &link.handle()),
        "shell or transport replaced"
    );
    Ok(())
}
