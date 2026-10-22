//! Explicit enrollment at a game prompt, with a fresh server listing as proof.
use crate::{keys::DeviceKey, session::Command, terminal::TransportInput};
use anyhow::{Context, Result, ensure};
use std::{collections::BTreeSet, time::Duration};
use tokio::sync::{broadcast, mpsc, oneshot};
async fn send(input: &mpsc::UnboundedSender<Command>, text: String) -> Result<()> {
    let (reply, response) = oneshot::channel();
    input
        .send(Command::Raw(text.into_bytes(), reply))
        .context("tab closed")?;
    tokio::time::timeout(Duration::from_secs(5), response)
        .await??
        .map_err(anyhow::Error::msg)
}
async fn read_until<T>(
    output: &mut broadcast::Receiver<Vec<u8>>,
    test: impl Fn(&str) -> Option<T>,
) -> Result<T> {
    let (writer, _) = mpsc::unbounded_channel::<TransportInput>();
    let mut terminal = crate::terminal::create(
        wezterm_term::TerminalSize {
            cols: 512,
            rows: 128,
            ..Default::default()
        },
        writer,
    );
    let mut total = 0;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let bytes = output
                .recv()
                .await
                .context("enrollment output lost or connection closed")?;
            total += bytes.len();
            ensure!(total <= 256 * 1024, "enrollment output exceeds limit");
            terminal.advance_bytes(&bytes);
            let text = crate::terminal::Snapshot::capture(&terminal, None, 1, 0, String::new(), 0)
                .rows
                .iter()
                .map(|l| l.as_str().trim_end().to_owned())
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(result) = test(&text) {
                return Ok(result);
            }
        }
    })
    .await
    .context(
        "server did not confirm the device-key operation; profile authentication was not changed",
    )?
}
fn listing(text: &str, account: &str) -> Option<Result<BTreeSet<String>>> {
    let header =
        regex::RegexBuilder::new(&format!("SSH keys on account {}:", regex::escape(account)))
            .case_insensitive(true)
            .build()
            .ok()?;
    let empty = "@sshkey: you have no registered SSH keys.";
    // Request twice: the second response header bounds the complete first list,
    // even when SSH splits a listing across packets. Echoed command text is not proof.
    let markers: Vec<_> = header
        .find_iter(text)
        .map(|found| found.start())
        .chain(text.match_indices(empty).map(|(i, _)| i))
        .collect();
    if markers.len() < 2 {
        return None;
    }
    let mut markers = markers;
    markers.sort_unstable();
    let block = &text[markers[0]..markers[1]];
    let pattern =
        regex::Regex::new(r"(?m)^\s*\d+\) (SHA256:[A-Za-z0-9+/]+)\s+.*\(added [^\n]+\)").unwrap();
    Some(Ok(pattern
        .captures_iter(block)
        .map(|c| c[1].to_owned())
        .collect()))
}
pub async fn run(
    input: mpsc::UnboundedSender<Command>,
    device: DeviceKey,
    user: String,
    remove: bool,
) -> Result<()> {
    ensure!(
        device.public_line.len() <= 16384 && !device.public_line.chars().any(char::is_control),
        "public key must be a single printable OpenSSH line"
    );
    let public = russh::keys::PublicKey::from_openssh(&device.public_line)?;
    ensure!(
        public.fingerprint(russh::keys::HashAlg::Sha256).to_string() == device.fingerprint,
        "public-key fingerprint differs from generated device key"
    );
    let account = user.split('+').next().context("missing account")?;
    ensure!(
        !account.is_empty() && account.len() <= 128 && account.chars().all(|c| !c.is_control()),
        "invalid game account"
    );
    let (reply, response) = oneshot::channel();
    input.send(Command::Observe(reply)).context("tab closed")?;
    let mut output = response.await?;
    let command = if remove {
        format!("@sshkey/del {}\r", device.fingerprint)
    } else {
        format!("@sshkey/add {}\r", device.public_line)
    };
    send(&input, command).await?;
    read_until(&mut output, |text| {
        let confirmed = if remove {
            text.contains(&format!("@sshkey: removed key {}.", device.fingerprint))
        } else {
            text.contains(&format!("@sshkey: key {} added.", device.fingerprint))
                || text.contains(&format!(
                    "@sshkey: that key is already registered ({}).",
                    device.fingerprint
                ))
        };
        confirmed.then_some(())
    })
    .await?;
    send(&input, "@sshkey/list\r@sshkey/list\r".into()).await?;
    let keys = read_until(&mut output, |text| listing(text, account)).await??;
    ensure!(
        keys.contains(&device.fingerprint) != remove,
        "server listing did not confirm the expected fingerprint"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_fresh_complete_listing_and_exact_fingerprint() {
        let text =
            "@sshkey/list\nSSH keys on account pilot:\n  1) SHA256:abc  desktop  (added now)\n";
        assert!(listing(text, "pilot").is_none());
        let complete = format!("{text}SSH keys on account pilot:\n");
        assert!(
            listing(&complete, "pilot")
                .unwrap()
                .unwrap()
                .contains("SHA256:abc")
        );
        assert!(listing(&complete, "other").is_none());
        assert!(listing("@sshkey: you have no registered SSH keys.\n@sshkey: you have no registered SSH keys.","pilot").unwrap().unwrap().is_empty());
    }
}
