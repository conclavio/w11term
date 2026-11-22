//! Host-key policy including OpenSSH patterns, hashed names and revocation.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, KeyInit, Mac};
use russh::keys::{
    Algorithm, Certificate, HashAlg, PublicKey, PublicKeyOrCertificate,
    ssh_key::certificate::CertType,
};
use sha1::Sha1;
use std::{path::Path, time::SystemTime};

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Marker {
    Host,
    Authority,
    Revoked,
}

fn matching_keys(
    path: &Path,
    host: &str,
    port: u16,
    mut visit: impl FnMut(Marker, PublicKey, usize) -> Result<()>,
) -> Result<()> {
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
        let marker = match marker {
            None => Marker::Host,
            Some("@revoked") => Marker::Revoked,
            Some("@cert-authority") => Marker::Authority,
            Some(marker) => bail!("unsupported known_hosts marker {marker} on matching host"),
        };
        visit(marker, recorded, index + 1)?;
    }
    Ok(())
}

pub(super) fn has_authority(path: &Path, host: &str, port: u16) -> Result<bool> {
    let mut found = false;
    matching_keys(path, host, port, |marker, _, _| {
        found |= marker == Marker::Authority;
        Ok(())
    })?;
    Ok(found)
}

fn allowed_ca_signature(algorithm: Algorithm) -> bool {
    matches!(
        algorithm,
        Algorithm::Ed25519
            | Algorithm::Ecdsa { .. }
            | Algorithm::Rsa {
                hash: Some(HashAlg::Sha256 | HashAlg::Sha512)
            }
            | Algorithm::SkEd25519
            | Algorithm::SkEcdsaSha2NistP256
    )
}

fn verify_certificate(cert: &Certificate, host: &str, now: u64) -> Result<()> {
    ensure!(
        cert.cert_type() == CertType::Host,
        "server presented an SSH user certificate"
    );
    ensure!(
        cert.valid_after() <= now,
        "SSH host certificate is not yet valid"
    );
    ensure!(
        now < cert.valid_before(),
        "SSH host certificate has expired"
    );
    // OpenSSH requires a nonempty principal list for host certificates and
    // matches each principal as a case-sensitive pattern against the hostname.
    let host = host.to_ascii_lowercase();
    ensure!(
        !cert.valid_principals().is_empty(),
        "SSH host certificate has no principals"
    );
    ensure!(
        cert.valid_principals()
            .iter()
            .any(|principal| glob(principal, &host)),
        "SSH host certificate has no matching principal for {host}"
    );
    ensure!(
        cert.critical_options().is_empty(),
        "SSH host certificate contains unsupported critical options"
    );
    let algorithm = cert.signature().algorithm();
    ensure!(
        allowed_ca_signature(algorithm.clone()),
        "disallowed SSH host certificate signature algorithm {algorithm}"
    );
    // The signature check proves the certificate was signed by its embedded
    // key. The caller separately establishes trust in that signer or the leaf.
    cert.verify_signature()
        .context("invalid SSH host certificate signature")
}

pub(super) fn verify(
    path: &Path,
    host: &str,
    port: u16,
    presented: &PublicKeyOrCertificate,
    pin: Option<&str>,
) -> Result<bool> {
    let key = presented.public_key();
    let certificate = presented.certificate();
    let (mut known, mut trusted, mut authority, mut trusted_signer) = (false, false, false, false);
    matching_keys(path, host, port, |marker, recorded, line| {
        let same_key = recorded.key_data() == key.key_data();
        let same_signer =
            certificate.is_some_and(|cert| cert.signature_key() == recorded.key_data());
        match marker {
            Marker::Revoked => ensure!(
                !same_key && !same_signer,
                "server key or certificate signer is revoked in known_hosts line {line}"
            ),
            Marker::Authority => {
                authority = true;
                trusted_signer |= same_signer;
            }
            Marker::Host => {
                known = true;
                trusted |= same_key;
            }
        }
        Ok(())
    })?;
    if let Some(pin) = pin {
        ensure!(
            key.fingerprint(HashAlg::Sha256).to_string() == pin,
            "host key mismatch against configured fingerprint"
        );
        trusted = true;
    }
    if let Some(cert) = certificate {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs();
        verify_certificate(cert, host, now)?;
        ensure!(
            trusted_signer || trusted,
            "untrusted SSH host certificate for {host}: no matching certificate authority or trusted host key"
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
        "matching known_hosts certificate authority requires a trusted host certificate; refusing trust-on-first-use"
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
        let pin = key.fingerprint(HashAlg::Sha256).to_string();
        let key = PublicKeyOrCertificate::from(key);
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

    use russh::keys::{PrivateKey, ssh_key::certificate::Builder};

    fn signing_key() -> PrivateKey {
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
    }

    fn certificate(
        ca: &PrivateKey,
        leaf: &PublicKey,
        kind: CertType,
        window: std::ops::Range<u64>,
        principals: &[&str],
        critical: bool,
    ) -> Certificate {
        let mut builder = Builder::new(
            vec![1; 32],
            leaf.key_data().clone(),
            window.start,
            window.end,
        )
        .unwrap();
        builder.cert_type(kind).unwrap();
        if principals.is_empty() {
            builder.all_principals_valid().unwrap();
        }
        for &principal in principals {
            builder.valid_principal(principal).unwrap();
        }
        if critical {
            builder
                .critical_option("source-address", "127.0.0.1")
                .unwrap();
        }
        builder.extension("unrecognized@example.org", "").unwrap();
        builder.sign(ca).unwrap()
    }

    #[test]
    fn certificate_type_principals_window_critical_options_and_signature() {
        let ca = signing_key();
        let leaf = signing_key();
        let make = |kind, principals: &[&str], critical| {
            certificate(&ca, leaf.public_key(), kind, 10..20, principals, critical)
        };
        let cert = make(CertType::Host, &["other", "g?te*.example.com"], false);
        assert!(verify_certificate(&cert, "GATE01.EXAMPLE.COM", 10).is_ok());
        assert!(verify_certificate(&cert, "gate01.example.com", 19).is_ok());
        assert!(verify_certificate(&cert, "gate01.example.com", 9).is_err());
        assert!(verify_certificate(&cert, "gate01.example.com", 20).is_err());
        assert!(verify_certificate(&cert, "gate01.example.com.bad", 11).is_err());
        for (kind, principals, critical) in [
            (CertType::User, vec!["*"], false),
            (CertType::Host, vec![], false),
            (CertType::Host, vec!["GATE*.example.com"], false),
            (CertType::Host, vec!["[gate01.example.com]:2222"], false),
            (CertType::Host, vec!["*"], true),
        ] {
            assert!(
                verify_certificate(&make(kind, &principals, critical), "gate01.example.com", 11)
                    .is_err()
            );
        }
        let mut tampered = cert.to_bytes().unwrap();
        *tampered.last_mut().unwrap() ^= 1;
        let tampered = Certificate::from_bytes(&tampered).unwrap();
        assert!(
            verify_certificate(&tampered, "gate01.example.com", 11)
                .unwrap_err()
                .to_string()
                .contains("signature")
        );
        assert!(!allowed_ca_signature(Algorithm::Rsa { hash: None }));
        assert!(allowed_ca_signature(Algorithm::Rsa {
            hash: Some(HashAlg::Sha256)
        }));
        assert!(allowed_ca_signature(Algorithm::Rsa {
            hash: Some(HashAlg::Sha512)
        }));
        assert!(!allowed_ca_signature(Algorithm::Dsa));
    }

    #[test]
    fn certificate_authorities_are_scoped_and_revocations_and_leaf_pins_take_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let ca = signing_key();
        let leaf = signing_key();
        let cert = certificate(
            &ca,
            leaf.public_key(),
            CertType::Host,
            0..u64::MAX,
            &["*.example.com"],
            false,
        )
        .into();
        let ca_line = ca.public_key().to_openssh().unwrap();
        let leaf_line = leaf.public_key().to_openssh().unwrap();
        let unrelated = signing_key().public_key().to_openssh().unwrap();
        let rule =
            format!("@cert-authority [*.example.com]:2222,![blocked.example.com]:2222 {ca_line}\n");
        let leaf_pin = leaf.public_key().fingerprint(HashAlg::Sha256).to_string();
        let ca_pin = ca.public_key().fingerprint(HashAlg::Sha256).to_string();
        let host = "relay.example.com";
        let check = |pin: Option<&str>| verify(&path, host, 2222, &cert, pin);

        std::fs::write(&path, format!("{rule}[{host}]:2222 {unrelated}")).unwrap();
        assert!(has_authority(&path, host, 2222).unwrap());
        assert!(!has_authority(&path, host, 22).unwrap());
        assert!(!has_authority(&path, "blocked.example.com", 2222).unwrap());
        assert!(check(None).unwrap());
        assert!(check(Some(&leaf_pin)).unwrap());
        assert!(check(Some(&ca_pin)).is_err());
        assert!(verify(&path, "blocked.example.com", 2222, &cert, None).is_err());
        assert!(verify(&path, host, 22, &cert, None).is_err());
        for revoked in [&ca_line, &leaf_line] {
            std::fs::write(
                &path,
                format!("{rule}[{host}]:2222 {leaf_line}\n@revoked [*.example.com]:2222 {revoked}"),
            )
            .unwrap();
            assert!(
                check(Some(&leaf_pin))
                    .unwrap_err()
                    .to_string()
                    .contains("revoked")
            );
        }

        // No authority is necessary when the leaf itself is already trusted.
        std::fs::write(&path, format!("[{host}]:2222 {leaf_line}")).unwrap();
        assert!(check(None).unwrap());
        std::fs::write(&path, format!("@cert-authority [{host}]:2222 {unrelated}")).unwrap();
        assert!(check(None).is_err());
        assert!(check(Some(&leaf_pin)).unwrap());
        std::fs::remove_file(&path).unwrap();
        assert!(check(None).is_err());
        assert!(check(Some(&leaf_pin)).unwrap());
        let invalid = certificate(
            &ca,
            leaf.public_key(),
            CertType::User,
            0..u64::MAX,
            &[host],
            false,
        )
        .into();
        assert!(verify(&path, host, 2222, &invalid, Some(&leaf_pin)).is_err());
    }
}
