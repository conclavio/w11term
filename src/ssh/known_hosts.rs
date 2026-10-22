//! Host-key policy including OpenSSH patterns, hashed names and revocation.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::{HashAlg, PublicKey};
use sha1::Sha1;
use std::path::Path;

fn glob(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut p, mut v, mut star, mut saved) = (0, 0, None, 0);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            saved = v;
        } else if let Some(s) = star {
            saved += 1;
            v = saved;
            p = s + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}
fn matches(patterns: &str, host: &str) -> Result<bool> {
    let mut positive = false;
    for entry in patterns.split(',') {
        let (negated, entry) = entry
            .strip_prefix('!')
            .map_or((false, entry), |s| (true, s));
        let matched = if entry.starts_with('|') {
            let parts: Vec<_> = entry.split('|').collect();
            ensure!(
                parts.len() == 4 && parts[1] == "1",
                "unsupported hashed host entry"
            );
            let salt = STANDARD.decode(parts[2])?;
            let hash = STANDARD.decode(parts[3])?;
            let hmac = Hmac::<Sha1>::new_from_slice(&salt)?;
            hmac.chain_update(host).verify_slice(&hash).is_ok()
        } else {
            glob(&entry.to_ascii_lowercase(), host)
        };
        if matched && negated {
            return Ok(false);
        }
        positive |= matched;
    }
    Ok(positive)
}

pub(super) fn verify(
    path: &Path,
    host: &str,
    port: u16,
    key: &PublicKey,
    pin: Option<&str>,
) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).context("reading known_hosts"),
    };
    let host = host.to_ascii_lowercase();
    let host = if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    };
    let (mut known, mut trusted, mut authority) = (false, false, false);
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let first = fields.next().unwrap();
        let (marker, patterns) = if first.starts_with('@') {
            (
                Some(first),
                fields.next().context("missing known_hosts host pattern")?,
            )
        } else {
            (None, first)
        };
        if !matches(patterns, &host).with_context(|| format!("known_hosts line {}", index + 1))? {
            continue;
        }
        let algorithm = fields.next().context("missing known_hosts key algorithm")?;
        let encoded = fields.next().context("missing known_hosts public key")?;
        let recorded = PublicKey::from_openssh(&format!("{algorithm} {encoded}"))?;
        let equal = recorded.key_data() == key.key_data();
        match marker {
            Some("@revoked") => ensure!(
                !equal,
                "server key is revoked in known_hosts line {}",
                index + 1
            ),
            Some("@cert-authority") => authority = true,
            Some(marker) => bail!("unsupported known_hosts marker {marker} on matching host"),
            None => {
                known = true;
                trusted |= equal;
            }
        }
    }
    if let Some(pin) = pin {
        ensure!(
            key.fingerprint(HashAlg::Sha256).to_string() == pin,
            "host key mismatch against configured fingerprint"
        );
        return Ok(true);
    }
    if trusted {
        return Ok(true);
    }
    ensure!(
        !known,
        "host key mismatch: a different key is recorded for this host; verify the change outside this connection"
    );
    ensure!(
        !authority,
        "matching known_hosts certificate authority requires SSH host-certificate support; refusing trust-on-first-use"
    );
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revocation_patterns_hashes_and_authorities_never_fall_back_to_tofu() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = PublicKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILIG2T/B0l0gaqj3puu510tu9N1OkQ4znY3LYuEm5zCF",
        )
        .unwrap();
        let line = key.to_openssh().unwrap();
        std::fs::write(&path, format!("*.com,!excluded.com\t{line}\n")).unwrap();
        assert!(verify(&path, "EXAMPLE.COM", 22, &key, None).unwrap());
        assert!(!verify(&path, "excluded.com", 22, &key, None).unwrap());
        assert!(!verify(&path, "example.com", 2222, &key, None).unwrap());
        std::fs::write(
            &path,
            format!("|1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|nuuC5vEqXlEZ/8BXQR7m619W6Ak= {line}\n"),
        )
        .unwrap();
        assert!(verify(&path, "example.com", 22, &key, None).unwrap());
        std::fs::write(
            &path,
            format!("example.com {line}\n@revoked *.com {line}\n"),
        )
        .unwrap();
        assert!(
            verify(&path, "example.com", 22, &key, None)
                .unwrap_err()
                .to_string()
                .contains("revoked")
        );
        let pin = key.fingerprint(HashAlg::Sha256).to_string();
        assert!(verify(&path, "example.com", 22, &key, Some(&pin)).is_err());
        std::fs::write(
            &path,
            format!("@cert-authority [example.com]:2222 {line}\n"),
        )
        .unwrap();
        assert!(
            verify(&path, "example.com", 2222, &key, None)
                .unwrap_err()
                .to_string()
                .contains("certificate")
        );
        assert!(!verify(&path, "other.com", 2222, &key, None).unwrap());
    }
}
