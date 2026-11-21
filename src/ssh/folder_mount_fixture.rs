//! Exercise profile grants and live Windows exports through a real Linux FUSE mount.
use super::{
    ConnectionShare, FolderMount, FolderMountStatus, FolderRoot, durability::bounded_command,
};
use crate::{
    folder_grants::{Authority, FolderScope, PaneShares},
    options::{Authentication, Source, SshOptions},
    session::{Command, Session},
};
use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use russh::keys::{HashAlg, PublicKey};
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path, process::Command as ProcessCommand, sync::Arc, time::Duration};
use tokio::time::{Instant, sleep, timeout};

const NAME: &str = "windows-binary.bin";
const CONTENT: &[u8] = b"w11term\x00\xff\x80\r\npassive SSHFS\x1b\xfe\x02";
const HOME: &str = "/home/w11fixture";

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
    timeout(Duration::from_secs(5), acknowledged)
        .await
        .context("interactive shell did not accept the PID probe")??
        .map_err(anyhow::Error::msg)?;
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

async fn check_mount(link: &ConnectionShare, mount: &str, tile_digest: &str) -> Result<String> {
    let record = format!("awk '$5 == \"{mount}\" {{ print }}' /proc/self/mountinfo");
    let bytes = checked(link, &record).await?;
    let line = std::str::from_utf8(&bytes)?;
    let (fields, fs) = line
        .split_once(" - ")
        .context("mount reported Mounted without a Linux mountinfo entry")?;
    let mut info = fs.split_whitespace();
    let source = info
        .next()
        .and_then(|kind| kind.strip_prefix("fuse.sshfs."))
        .context("unexpected filesystem type")?;
    let token = source
        .strip_prefix("w11term-")
        .context("absent mount generation")?;
    ensure!(
        token.len() == 32
            && token.bytes().all(|b| b.is_ascii_hexdigit())
            && info.next() == Some(source),
        "mount source and generation differ: {line}"
    );
    ensure!(
        fields
            .split_whitespace()
            .nth(5)
            .is_some_and(|opts| opts.split(',').any(|s| s == "ro")),
        "writable mount: {line}"
    );
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
    Ok(source.to_owned())
}

fn options() -> Result<SshOptions> {
    let port = env::var("W11TERM_MOUNT_TEST_PORT")?.parse::<u16>()?;
    let key = PublicKey::from_openssh(&env::var("W11TERM_MOUNT_TEST_HOST_KEY")?)?;
    let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
    Ok(SshOptions {
        host: "127.0.0.1".into(),
        port,
        user: "w11fixture".into(),
        authentication: Authentication::Identity {
            identity: env::var_os("W11TERM_MOUNT_TEST_IDENTITY")
                .context("missing temporary client key")?
                .into(),
        },
        host_key: Some(fingerprint),
        proxy_jump: vec![],
        terminal_type: Default::default(),
        durable: false,
        player: crate::options::PlayerOptions {
            reconnect: false,
            ..Default::default()
        },
    })
}

async fn login(options: &SshOptions) -> Result<(Session, ConnectionShare)> {
    let pane = Session::start_configured(Source::Ssh(options.clone()), Default::default(), false);
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
        Some(link.fingerprint()) == options.host_key.as_deref(),
        "unexpected SSH host identity"
    );
    Ok((pane, link))
}

async fn ready_authority(manager: &PaneShares) -> Result<Authority> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(authority) = manager.authority() {
            return Ok(authority);
        }
        ensure!(
            Instant::now() < deadline,
            "profile sharing did not bind to the verified SSH pane"
        );
        sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_status(
    get: impl Fn() -> Result<Option<FolderMountStatus>>,
    expected: Option<FolderMountStatus>,
    name: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = get()?;
        if status == expected {
            return Ok(());
        }
        if let Some(FolderMountStatus::Failed(error)) = &status {
            bail!("mount {name}: {error}");
        }
        ensure!(
            Instant::now() < deadline,
            "mount {name}: expected {expected:?}, received {status:?}"
        );
        sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_profile(
    manager: &PaneShares,
    auth: &Authority,
    name: &str,
    expected: Option<FolderMountStatus>,
) -> Result<()> {
    wait_status(
        || {
            Ok(manager
                .rows(auth)?
                .into_iter()
                .find(|row| row.name == name)
                .map(|row| row.status))
        },
        expected,
        name,
    )
    .await
}

async fn expect_refused(
    link: &ConnectionShare,
    path: &Path,
    name: &str,
    explanations: &[&str],
) -> Result<()> {
    ensure!(
        super::valid_mount_name(name),
        "invalid test mount name: {name}"
    );
    let root = FolderRoot::open(path)?;
    let error = match FolderMount::start(link.clone(), root, name) {
        Err(error) => format!("{error:#}"),
        Ok(mount) => {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                match mount.status() {
                    FolderMountStatus::Starting => {}
                    FolderMountStatus::Failed(error) => break error,
                    state => bail!("unsafe mount {name} reached {state:?}"),
                }
                ensure!(
                    Instant::now() < deadline,
                    "timed out waiting for mount {name} to be refused"
                );
                sleep(Duration::from_millis(50)).await;
            }
        }
    };
    ensure!(
        explanations.iter().any(|s| error.contains(s)),
        "unexpected reason for refusing {name}: {error}"
    );
    Ok(())
}

async fn wait_detached(link: &ConnectionShare, mount: &str) -> Result<()> {
    let record = format!("awk '$5 == \"{mount}\" {{ print }}' /proc/self/mountinfo");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !checked(link, &record).await?.is_empty() {
        ensure!(
            Instant::now() < deadline,
            "detached share is still mounted at {mount}"
        );
        sleep(Duration::from_millis(100)).await;
    }
    checked(link, &format!("test ! -e {mount}/{NAME}")).await?;
    Ok(())
}

async fn unmount_profile(
    manager: &PaneShares,
    auth: &Authority,
    link: &ConnectionShare,
    name: &str,
    remote: &str,
) -> Result<()> {
    manager.unmount(auth, name)?;
    if let Err(error) = wait_profile(manager, auth, name, Some(FolderMountStatus::Stopped)).await {
        // The process status and kernel mount record tell us whether the
        // daemon, its auto-unmount helper, or only the SSH channel survived.
        let inspect = r#"awk -v path="$1" '$5 == path' /proc/self/mountinfo
for proc in /proc/[0-9]*; do
  read -r comm < "$proc/comm" 2>/dev/null || continue
  case "$comm" in sshfs|fusermount3) awk '/^(Name|State|Pid|PPid):/' "$proc/status";; esac
done"#;
        // $1 is a fixture-generated absolute path.
        let details = checked(link, &format!("set -- {remote}; {inspect}")).await;
        return Err(error.context(format!(
            "unmount {remote}; guest state: {}",
            match details {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(error) => format!("{error:#}"),
            }
        )));
    }
    wait_detached(link, remote).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the disposable Linux guest with its stock pre-3.15 FUSE helper"]
async fn old_fusermount_refuses_before_export() -> Result<()> {
    let (pane, link) = login(&options()?).await?;
    let pid = shell_pid(&pane, "BEFORE_OLD_HELPER").await?;
    let output = checked(&link, "fusermount3 --version").await?;
    let version = std::str::from_utf8(&output)?
        .trim()
        .strip_prefix("fusermount3 version: ")
        .context("unrecognized guest FUSE helper")?;
    ensure!(
        version.starts_with("3.14."),
        "the legacy test requires a real 3.14.x helper, found {version}"
    );

    let clean_home = format!("test ! -e {HOME}/windows");
    checked(&link, &clean_home).await?;
    let fixture = tempfile::tempdir()?;
    fs::write(fixture.path().join(NAME), CONTENT)?;
    let name = format!("legacy-{:016x}", rand::random::<u64>());
    expect_refused(
        &link,
        fixture.path(),
        &name,
        &[&format!("Found fusermount3 {version}. Upgrade")],
    )
    .await?;
    wait_detached(&link, &format!("{HOME}/windows/{name}")).await?;
    checked(&link, &clean_home)
        .await
        .context("old helper created a mount or lock directory")?;
    ensure!(shell_pid(&pane, "AFTER_OLD_HELPER").await? == pid);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the disposable Linux FUSE guest and pinned loopback SSH supplied by the mount runner"]
async fn native_windows_passive_sshfs() -> Result<()> {
    let options = options()?;
    let (pane, link) = login(&options).await?;
    let before = shell_pid(&pane, "BEFORE_MOUNT").await?;
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
    let name = format!("ci-{:016x}", rand::random::<u64>());
    let remote = format!("{HOME}/windows/{name}");
    let catalog = fixture.path().join("profiles.json");
    fs::write(
        &catalog,
        serde_json::to_vec(&serde_json::json!({"profiles":{"linux": &options}}))?,
    )?;
    let scope = || {
        FolderScope::new(
            catalog.clone(),
            "linux".into(),
            fixture.path().join("grants"),
        )
    };
    let manager = pane.profile_folders(scope());
    let auth = ready_authority(&manager).await?;
    manager.add(&auth, &name, selected.clone())?;
    wait_profile(&manager, &auth, &name, Some(FolderMountStatus::Mounted))
        .await
        .context("initial saved grant mount")?;
    let initial = check_mount(&link, &remote, &tile_digest).await?;

    // Another lease on this name, even with a different root, must leave the first mount intact.
    expect_refused(
        &link,
        &outside,
        &name,
        &["holds this name", "already mounted"],
    )
    .await?;
    ensure!(
        initial == check_mount(&link, &remote, &tile_digest).await?,
        "collision replaced the original mount"
    );
    let nonempty = format!("{name}-occupied");
    let symlink = format!("{name}-link");
    let occupied = format!("{HOME}/windows/{nonempty}");
    let alias = format!("{HOME}/windows/{symlink}");
    let empty_target = format!("/tmp/{name}-elsewhere");
    checked(&link, &format!("set -e; mkdir -m 700 {occupied} {empty_target}; printf preserved > {occupied}/sentinel; ln -s {empty_target} {alias}")).await?;
    expect_refused(&link, &selected, &nonempty, &["not empty"]).await?;
    expect_refused(&link, &selected, &symlink, &["symbolic link"]).await?;
    ensure!(checked(&link, &format!("cat {occupied}/sentinel")).await? == b"preserved");
    ensure!(
        checked(&link, &format!("readlink {alias}")).await?
            == format!("{empty_target}\n").as_bytes()
    );
    checked(
        &link,
        &format!("set -e; rm -- {alias} {occupied}/sentinel; rmdir -- {occupied} {empty_target}"),
    )
    .await?;

    unmount_profile(&manager, &auth, &link, &name, &remote)
        .await
        .context("first explicit unmount after collision and destination checks")?;

    let original = fixture.path().join("original");
    fs::rename(&selected, &original)?;
    fs::create_dir(&selected)?;
    fs::write(selected.join(NAME), b"unapproved replacement")?;
    manager.mount(&auth, &name)?;
    let refusal = wait_profile(&manager, &auth, &name, Some(FolderMountStatus::Mounted))
        .await
        .err()
        .context("the saved grant mounted a replacement Windows directory")?;
    ensure!(
        refusal.to_string().contains("directory was replaced"),
        "unexpected refusal for a replaced directory: {refusal:#}"
    );
    wait_detached(&link, &remote).await?;
    fs::rename(&selected, fixture.path().join("replacement"))?;
    fs::rename(&original, &selected)?;
    manager.mount(&auth, &name)?;
    wait_profile(&manager, &auth, &name, Some(FolderMountStatus::Mounted)).await?;
    let restored = check_mount(&link, &remote, &tile_digest).await?;
    ensure!(
        restored != initial,
        "restoring the original reused its old mount"
    );
    unmount_profile(&manager, &auth, &link, &name, &remote)
        .await
        .context("unmount after restoring the original Windows directory")?;

    let manual = FolderMount::start(link.clone(), FolderRoot::open(&selected)?, &name)?;
    wait_status(
        || Ok(Some(manual.status())),
        Some(FolderMountStatus::Mounted),
        &name,
    )
    .await?;
    ensure!(
        check_mount(&link, &remote, &tile_digest).await? != restored,
        "new mount reused its generation"
    );
    drop(manual);
    wait_detached(&link, &remote).await?;
    manager.mount(&auth, &name)?;
    wait_profile(&manager, &auth, &name, Some(FolderMountStatus::Mounted)).await?;
    let before_disconnect = check_mount(&link, &remote, &tile_digest).await?;
    ensure!(shell_pid(&pane, "AFTER_MOUNT").await? == before);
    let still_live = pane
        .ssh_connection()
        .context("interactive SSH connection lost")?;
    ensure!(
        Arc::ptr_eq(&still_live.handle(), &link.handle()),
        "original SSH transport was replaced"
    );

    link.handle()
        .disconnect(
            russh::Disconnect::ByApplication,
            "isolated mount reconnect test",
            "en",
        )
        .await?;
    let (new_pane, new_link) = login(&options).await?;
    ensure!(
        !Arc::ptr_eq(&new_link.handle(), &link.handle()),
        "fixture did not open a fresh authenticated connection"
    );
    // Keep the old manager and lease alive until the guest confirms transport
    // loss itself revoked the mount.
    wait_detached(&new_link, &remote).await?;
    drop(manager);
    drop(pane);
    let new_pid = shell_pid(&new_pane, "RECONNECTED").await?;
    let reopened = new_pane.profile_folders(scope());
    let new_auth = ready_authority(&reopened).await?;
    wait_profile(
        &reopened,
        &new_auth,
        &name,
        Some(FolderMountStatus::Mounted),
    )
    .await?;
    ensure!(
        check_mount(&new_link, &remote, &tile_digest).await? != before_disconnect,
        "saved grant reused its stale mount"
    );
    ensure!(
        reopened.forget(&auth, &name).is_err(),
        "previous pane authority could change a new mount"
    );
    reopened.forget(&new_auth, &name)?;
    wait_profile(&reopened, &new_auth, &name, None).await?;
    wait_detached(&new_link, &remote).await?;
    drop(reopened);
    let after_forget = new_pane.profile_folders(scope());
    let auth = ready_authority(&after_forget).await?;
    ensure!(
        after_forget.rows(&auth)?.is_empty(),
        "the forgotten grant reappeared when the saved profile was reopened"
    );
    ensure!(shell_pid(&new_pane, "AFTER_FORGET").await? == new_pid);
    ensure!(
        fs::read(path)? == CONTENT && fs::read(secret)? == b"sibling data",
        "Windows fixture changed"
    );
    Ok(())
}
