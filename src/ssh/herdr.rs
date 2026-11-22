//! Opt-in existing Herdr terminals on an authenticated Linux SSH account.
use super::{ConnectionShare, durability::bounded_command, sharing::ShellChannel};
use crate::{
    durable::{
        Backend, DiscoveredSession, DurableReference,
        herdr::{self, Client, Target, Viewer},
    },
    options::SshOptions,
};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};
use tokio::io::{ReadHalf, WriteHalf};
type Stream = russh::ChannelStream<russh::client::Msg>;
pub(super) type Attached = Viewer<ReadHalf<Stream>, WriteHalf<Stream>>;

async fn client(mut guard: ShellChannel) -> Result<Client<ReadHalf<Stream>, WriteHalf<Stream>>> {
    let channel = guard.channel();
    channel.exec(true, "hc connect").await?;
    loop {
        match channel.wait().await {
            Some(russh::ChannelMsg::Success) => break,
            Some(russh::ChannelMsg::WindowAdjusted { .. }) => {}
            _ => anyhow::bail!("SSH server did not accept hc connect"),
        }
    }
    let (reader, writer) = tokio::io::split(guard.into_stream());
    Client::connect(reader, writer)
        .await
        .context("Opening an interactive Herdr Core connection")
}

// Linux process birth is required because a kept hc terminal can restart in place.
// Shell input is generated exclusively from u32 values. Find the final ") " since
// /proc/PID/stat's comm field can itself contain whitespace or parentheses.
async fn births(link: &ConnectionShare, pids: &[u32]) -> Result<(String, BTreeMap<u32, u64>)> {
    ensure!(
        !pids.is_empty() && pids.len() <= 64,
        "Invalid Herdr identity probe batch"
    );
    let numbers = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let query = format!(
        r#"sh -c 'set -f; cat /proc/sys/kernel/random/boot_id || exit 1; for p in "$@"; do s=$(cat "/proc/$p/stat" 2>/dev/null) || continue; set -- ${{s##*) }}; [ "$#" -ge 20 ] || continue; printf "%s %s\n" "$p" "${{20}}"; done' sh {numbers}"#
    );
    let result = bounded_command(link, &query, 8192).await?;
    ensure!(
        result.status == 0,
        "Herdr process verification needs the Linux /proc filesystem"
    );
    let text = std::str::from_utf8(&result.stdout)?;
    let mut lines = text.lines();
    let boot = lines
        .next()
        .filter(|s| herdr::boot_id(s))
        .context("Invalid Linux boot ID for Herdr verification")?
        .to_owned();
    let mut values = BTreeMap::new();
    for line in lines {
        let (pid, ticks) = line
            .split_once(' ')
            .context("Invalid Herdr process identity response")?;
        let (pid, ticks) = (pid.parse::<u32>()?, ticks.parse::<u64>()?);
        ensure!(
            pids.contains(&pid) && values.insert(pid, ticks).is_none(),
            "Unexpected Herdr process identity"
        );
    }
    Ok((boot, values))
}

pub(super) async fn discover(
    link: &ConnectionShare,
    options: &SshOptions,
) -> Result<(Vec<DiscoveredSession>, String)> {
    let mut hc = client(link.open_shell().await?).await?;
    let entries = hc.list().await?;
    let core = hc.core.clone();
    drop(hc);
    let pids = entries
        .iter()
        .map(|e| e.pid)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut checked = BTreeMap::new();
    for batch in pids.chunks(64) {
        let (boot, values) = births(link, batch).await?;
        for (pid, ticks) in values {
            checked.insert(pid, (boot.clone(), ticks));
        }
    }
    let count = entries.len();
    let mut sessions = Vec::new();
    for e in entries {
        let Some((boot, ticks)) = checked.get(&e.pid) else {
            continue;
        };
        let target = Target {
            core: core.clone(),
            created_ms: e.created_ms,
            pid: e.pid,
            boot: boot.clone(),
            ticks: *ticks,
        };
        sessions.push(DiscoveredSession {
            reference: DurableReference {
                backend: Backend::Herdr,
                host: options.host.clone(),
                host_key_alias: options.host_key_alias.clone(),
                port: options.port,
                user: options.user.clone(),
                host_key: link.fingerprint().into(),
                name: e.address.clone(),
                generation: target.generation(),
            },
            display_name: format!("{} ({})", e.name, e.address),
            windows: None,
            clients: 0,
            directory: e.cwd,
            command: e.command,
        });
    }
    let mut detail = format!("{} live Herdr terminals", sessions.len());
    if sessions.len() != count {
        detail.push_str(&format!(
            " · {} unavailable for process verification",
            count - sessions.len()
        ));
    }
    Ok((sessions, detail))
}

pub(super) async fn attach(
    channel: ShellChannel,
    link: &ConnectionShare,
    reference: &DurableReference,
    cols: u16,
    rows: u16,
) -> Result<Attached> {
    reference.validate()?;
    ensure!(
        reference.host_key == link.fingerprint(),
        "Herdr SSH host identity changed"
    );
    let target =
        Target::parse(&reference.name, &reference.generation).context("Invalid Herdr reference")?;
    let mut hc = client(channel).await?;
    hc.verify_core(&target.core)?;
    // Inspect the record before requesting resize/ownership, then verify the server's
    // attach reply and process birth again before exposing any input or terminal bytes.
    let selected = hc
        .list()
        .await?
        .into_iter()
        .find(|e| {
            e.address == reference.name && e.created_ms == target.created_ms && e.pid == target.pid
        })
        .context("Herdr terminal ended or its program changed; choose the terminal again")?;
    verify_birth(link, &target).await?;
    let viewer = hc.attach(&selected, cols, rows).await?;
    verify_birth(link, &target).await?;
    Ok(viewer)
}

async fn verify_birth(link: &ConnectionShare, target: &Target) -> Result<()> {
    let (boot, ids) = births(link, &[target.pid]).await?;
    target.verify_birth(&boot, ids.get(&target.pid).copied())
}
