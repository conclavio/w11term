use std::{
    io::{BufRead, BufReader, Read},
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use russh::keys::PublicKey;

use crate::options::AgentFingerprint;

const MAX_KEY_PREFIX: u64 = 16 * 1024;

pub(super) fn public_fingerprint(path: &Path) -> Result<Option<AgentFingerprint>> {
    let named_public = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pub"));
    let file = match super::includes::open_local_identity(path) {
        Ok(Some(file)) => file,
        Ok(None) if named_public => bail!("public key file does not exist"),
        Err(error) if named_public => return Err(error.context("opening local public key file")),
        _ => return Ok(None),
    };
    // Read at most a small prefix and stop at a private-key header. A private path
    // remains a path, and a public file contributes only its public fingerprint.
    let mut reader = BufReader::with_capacity(512, file.take(MAX_KEY_PREFIX + 1));
    let mut line = Vec::new();
    let mut consumed = 0;
    loop {
        let count = reader
            .read_until(b'\n', &mut line)
            .context("reading IdentityFile prefix");
        let count = match count {
            Ok(0) => break,
            Ok(count) => count,
            Err(_) if !named_public => return Ok(None),
            Err(error) => return Err(error),
        };
        consumed += count as u64;
        let text = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&line);
        let mut words = text
            .split(|byte| byte.is_ascii_whitespace())
            .filter(|word| !word.is_empty());
        if let Some(algorithm) = words.next() {
            if algorithm.starts_with(b"-----BEGIN") || text.starts_with(b"SSH PRIVATE KEY FILE") {
                return Ok(None);
            }
            if [b"ssh-".as_slice(), b"ecdsa-", b"sk-"]
                .iter()
                .any(|prefix| algorithm.starts_with(prefix))
            {
                ensure!(
                    consumed <= MAX_KEY_PREFIX,
                    "public key file exceeds 16 KiB before the key line ends"
                );
                ensure!(
                    !algorithm.ends_with(b"-cert-v01@openssh.com"),
                    "an OpenSSH certificate is not a plain public IdentityFile"
                );
                let algorithm = std::str::from_utf8(algorithm)?;
                let payload = words.next().context("missing OpenSSH public key data")?;
                let payload = std::str::from_utf8(payload)?;
                let key = PublicKey::from_openssh(&format!("{algorithm} {payload}"))
                    .context("invalid OpenSSH public key")?;
                return Ok(Some(AgentFingerprint::for_key(&key)));
            }
        }
        line.clear();
    }
    ensure!(!named_public, "expected a plain OpenSSH public key");
    Ok(None)
}
