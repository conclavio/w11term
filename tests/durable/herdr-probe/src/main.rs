//! Linux CI adapter for the production codec. OpenSSH provides its byte streams.
#[allow(dead_code)]
#[path = "../../../../src/durable/herdr.rs"]
mod herdr;

use anyhow::{Context, Result, ensure};
use herdr::{Client, Entry, Target, Viewer};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

type Connection = Client<ChildStdout, ChildStdin>;
type Screen = Viewer<ChildStdout, ChildStdin>;

async fn connect(config: &Path) -> Result<(Child, Connection)> {
    let mut ssh = Command::new("ssh")
        .arg("-F")
        .arg(config)
        .args(["-T", "fixture", "hc", "connect"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let client = Client::connect(ssh.stdout.take().unwrap(), ssh.stdin.take().unwrap()).await?;
    Ok((ssh, client))
}

async fn only_terminal(client: &mut Connection) -> Result<Entry> {
    let mut entries = client.list().await?;
    ensure!(
        entries.len() == 1,
        "Expected one live terminal, got {entries:?}"
    );
    Ok(entries.remove(0))
}

async fn expect(viewer: &mut Screen, needles: &[&str]) -> Result<()> {
    let mut output = Vec::new();
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let bytes = viewer.next_data().await?.context("Viewer closed")?;
            viewer.credit(bytes.len()).await?;
            output.extend_from_slice(&bytes);
            if needles.iter().all(|needle| {
                output
                    .windows(needle.len())
                    .any(|part| part == needle.as_bytes())
            }) {
                return Ok(());
            }
            ensure!(
                output.len() <= 512 * 1024,
                "Unexpected terminal output volume"
            );
        }
    })
    .await
    .with_context(|| {
        format!(
            "Missing {needles:?}, last output {:?}",
            String::from_utf8_lossy(&output[output.len().saturating_sub(256)..])
        )
    })?
}

async fn exercise(config: &Path) -> Result<()> {
    let (ssh, mut client) = connect(config).await?;
    let mut entry = only_terminal(&mut client).await?;
    ensure!(
        entry.name == "fixture",
        "Unexpected stock terminal {entry:?}"
    );
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", entry.pid))?;
    let ticks = stat
        .rsplit_once(") ")
        .context("Missing Linux process name")?
        .1
        .split_whitespace()
        .nth(19)
        .context("Missing Linux start time")?
        .parse()?;
    let target = Target {
        core: client.core.clone(),
        pid: entry.pid,
        created_ms: entry.created_ms,
        boot: boot.trim().to_owned(),
        ticks,
    };
    let saved = Target::parse(&entry.address, &target.generation()).context("Saving selection")?;
    client.verify_core(&saved.core)?;
    saved.verify_birth(boot.trim(), Some(ticks))?;
    let mut viewer = client.attach(&entry, 90, 31).await?;
    expect(&mut viewer, &["W11_GRID=90x31"]).await?;
    viewer.input(b"A\0\x07\x1b[C\xff").await?;
    expect(
        &mut viewer,
        &["w11-renamed-title", "W11_INPUT=4100071b5b43ff"],
    )
    .await?;
    viewer.resize(117, 41).await?;
    expect(&mut viewer, &["W11_GRID=117x41"]).await?;
    viewer.close().await?;
    drop(viewer);
    drop(ssh);
    println!("Detached after exact byte input and a 117x41 resize");

    // Stock 0.1.21 lets the application change its OSC title. The saved UI
    // label is independently mutable and is deliberately stale on reattach.
    entry.name = "older display label".into();
    let (ssh, mut client) = connect(config).await?;
    let current = only_terminal(&mut client).await?;
    ensure!(
        current.address == entry.address
            && current.pid == saved.pid
            && current.created_ms == saved.created_ms,
        "Terminal changed while detached"
    );
    client.verify_core(&saved.core)?;
    ensure!(
        client.verify_core("wrong-core").is_err(),
        "Wrong core accepted"
    );
    ensure!(
        saved.verify_birth(boot.trim(), Some(ticks + 1)).is_err(),
        "Wrong process birth accepted"
    );
    let mut viewer = client.attach(&entry, 117, 41).await?;
    expect(
        &mut viewer,
        &["w11-renamed-title", "W11_INPUT=4100071b5b43ff"],
    )
    .await?;
    viewer.input(b"!").await?;
    expect(&mut viewer, &["W11_INPUT=4100071b5b43ff21"]).await?;
    viewer.close().await?;
    drop(viewer);
    drop(ssh);
    println!("Reattached with old display name and continued the same in-memory program");

    for field in ["missing address", "wrong creation time", "wrong PID"] {
        let (_ssh, client) = connect(config).await?;
        let mut stale = entry.clone();
        match field {
            "missing address" => stale.address = "t4294967294".into(),
            "wrong creation time" => stale.created_ms += 1,
            _ => stale.pid += 1,
        }
        ensure!(
            client.attach(&stale, 117, 41).await.is_err(),
            "Attached with {field}"
        );
    }
    let (_ssh, mut client) = connect(config).await?;
    let remaining = only_terminal(&mut client).await?;
    ensure!(
        remaining.address == entry.address
            && remaining.pid == saved.pid
            && remaining.created_ms == saved.created_ms,
        "A stale request replaced or created a terminal: {remaining:?}"
    );
    println!("Rejected missing/stale selections; the single original terminal remains live");
    Ok(())
}

fn main() -> Result<()> {
    let config = std::env::args_os()
        .nth(1)
        .context("Pass the fixture SSH config path")?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(60), exercise(Path::new(&config)))
                .await
                .context("Timed out testing stock Herdr over loopback SSH")?
        })
}
